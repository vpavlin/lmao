# Container-per-agent build for LMAO.
#
# - Builder stage compiles `liblogosdelivery.so` (via Nim 2.x) and the
#   `lmao` CLI with both `logos-delivery` and `libstorage` features.
#   `storage-bindings` downloads its prebuilt static blob during cargo
#   build, so no Codex/Nim build for storage.
# - Runtime stage is debian-slim + libgomp/libssl/ca-certificates,
#   running as a non-root user. The image bundles Goose so the default
#   --exec recipe works out of the box; configure it for your local
#   Ollama (or any OpenAI-compatible endpoint) at runtime.
#
# Build:
#   docker build -t lmao .
#
# Or with a non-default Goose / lib version pinned via build-args.
# See `docker-compose.yml` and `scripts/demo-containerized.sh` for a
# fleet of two agents talking through the embedded daemon socket.

# ─── Builder ────────────────────────────────────────────────────────
FROM rust:1.83-bookworm AS builder

ARG LOGOS_DELIVERY_REF=master

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        curl \
        git \
        jq \
        libclang-dev \
        libssl-dev \
        make \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Nim 2.x via choosenim. CHOOSENIM_NO_ANALYTICS keeps the install quiet
# and skips the network call to Mixpanel.
ENV CHOOSENIM_NO_ANALYTICS=1
RUN curl -fsSL https://nim-lang.org/choosenim/init.sh -o /tmp/choosenim.sh \
    && bash /tmp/choosenim.sh -y \
    && rm /tmp/choosenim.sh
ENV PATH=/root/.nimble/bin:$PATH

# Build liblogosdelivery from upstream. ~5 min on a warm image cache.
WORKDIR /build
RUN git clone --depth 1 --branch "${LOGOS_DELIVERY_REF}" --recurse-submodules \
        https://github.com/logos-messaging/logos-delivery.git
RUN cd logos-delivery && make liblogosdelivery
ENV LIBLOGOSDELIVERY_LIB_DIR=/build/logos-delivery/build

# Build the LMAO CLI with FFI features. Cargo's build cache is layered
# above the Nim build so editing source code only triggers cargo work.
WORKDIR /src
COPY . .
RUN cargo build --release -p logos-messaging-a2a-cli \
        --features logos-delivery,libstorage

# Goose CLI binary. We ship it inside the image so --exec works without
# the operator having to install anything host-side. CONFIGURE=false
# skips the interactive provider setup.
ENV GOOSE_BIN_DIR=/build/goose-bin
RUN mkdir -p "$GOOSE_BIN_DIR" \
    && curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh \
       | CONFIGURE=false bash

# ─── Runtime ────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libgomp1 \
        libssl3 \
        libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

# Non-root user. The agent never needs root inside the container; if
# you mount in a host directory, chown it to 1000:1000.
RUN useradd --create-home --uid 1000 --shell /bin/bash lmao

# liblogosdelivery is a runtime dynamic dep — must live on the linker
# search path. ldconfig refreshes the cache so the lmao binary's rpath
# can find it.
COPY --from=builder /build/logos-delivery/build/liblogosdelivery.so /usr/local/lib/
RUN ldconfig

COPY --from=builder /src/target/release/logos-messaging-a2a /usr/local/bin/lmao
COPY --from=builder /build/goose-bin/goose                   /usr/local/bin/goose

# Default writable locations. Compose volumes mount over /data and
# /run/lmao at runtime so storage and the daemon socket persist (or
# stay shared with the host) outside the container's filesystem.
RUN mkdir -p /data /run/lmao \
    && chown -R lmao:lmao /data /run/lmao

USER lmao
WORKDIR /data

# Sensible defaults for an in-container agent: persistent identity in
# the data volume, libstorage data dir alongside, daemon socket on the
# shared volume so host CLIs can drive it.
ENV LMAO_KEYFILE=/data/keyfile \
    LMAO_STORAGE_DATA_DIR=/data/storage \
    LMAO_DAEMON_SOCKET=/run/lmao/lmao.sock

ENTRYPOINT ["/usr/local/bin/lmao"]
CMD ["--help"]
