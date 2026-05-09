.PHONY: build test clippy fmt check doc clean examples bench demo demo-in-memory demo-containerized demo-image demo-down demo-logos-core demo-logos-core-real cli-logos-delivery basecamp basecamp-module basecamp-ui basecamp-install basecamp-lgx basecamp-lgx-install

# Build all crates
build:
	~/.cargo/bin/cargo build --workspace

# Build in release mode
release:
	~/.cargo/bin/cargo build --workspace --release

# Run all tests
test:
	~/.cargo/bin/cargo test --workspace

# Run clippy lints
clippy:
	~/.cargo/bin/cargo clippy --workspace -- -D warnings

# Format code
fmt:
	~/.cargo/bin/cargo fmt --all

# Check formatting (CI mode)
fmt-check:
	~/.cargo/bin/cargo fmt --all -- --check

# Full CI check (format + clippy + test)
check: fmt-check clippy test

# Generate documentation
doc:
	~/.cargo/bin/cargo doc --workspace --no-deps

# Build examples
examples:
	~/.cargo/bin/cargo build --examples

# Run benchmarks (requires criterion, see jimmy/add-benchmarks branch)
bench:
	~/.cargo/bin/cargo bench --workspace

# Real-network CLI demo on logos.dev — two agents + a delegating client,
# all CLI processes, all over real Logos Messaging gossip.
#
# Requires liblogosdelivery.so. Set LIBLOGOSDELIVERY_LIB_DIR before invoking.
demo: cli-logos-delivery
	@LIBLOGOSDELIVERY_LIB_DIR="$(LIBLOGOSDELIVERY_LIB_DIR)" ./scripts/demo.sh

# Build the CLI with the logos-delivery transport + libstorage features.
# liblogosdelivery.so must be on disk (LIBLOGOSDELIVERY_LIB_DIR set);
# storage-bindings downloads its own prebuilt static blob on first build.
cli-logos-delivery:
	~/.cargo/bin/cargo build --release -p logos-messaging-a2a-cli --features logos-delivery,libstorage

# In-memory two-agent demo — no native deps, fast smoke test.
demo-in-memory:
	~/.cargo/bin/cargo run --example two_agents

# Container-per-agent demo. Each agent runs as a non-root user inside
# its own debian-slim container with no host filesystem access except a
# scoped data volume. Same logos.dev fleet, same five-step narrative,
# stronger isolation story for `--exec goose` running untrusted task
# text. Requires docker + docker compose.
#
# First run builds the image (~15-20 min: Nim + Rust + Goose download).
# Subsequent runs reuse the image cache and finish in ~30 s.
demo-containerized: cli-logos-delivery
	@LIBLOGOSDELIVERY_LIB_DIR="$(LIBLOGOSDELIVERY_LIB_DIR)" ./scripts/demo-containerized.sh

# Force a rebuild of the demo image (--no-cache + host networking
# during build so the Nim/nimble package fetches don't hit DNS hiccups
# in Docker's default build network).
demo-image:
	docker compose build --no-cache

# Tear down the containerised demo if it's still running.
demo-down:
	docker compose down --remove-orphans

# Build the Basecamp module pair (`agent` core + `agent_ui` QML) via Nix.
# The UI flake takes the core module via `agent.url = path:..` — when
# that path can't be resolved (sub-projects git-init'd separately), we
# fall back to --override-input with an absolute path.
basecamp: basecamp-module basecamp-ui

basecamp-module:
	cd basecamp/agent-module && nix build -L

basecamp-ui: basecamp-module
	cd basecamp/agent-ui && nix build -L \
		--override-input agent "path:$(CURDIR)/basecamp/agent-module"

# Copy plugin artifacts into the Logos Basecamp dev modules directory
# so a locally-built `LogosBasecamp` finds them. After this, launch
# Basecamp and the `agent_ui` tab should appear in the sidebar.
#
# Override LMAO_BASECAMP_MODULES to install elsewhere (e.g. a portable
# bundle's modules dir).
LMAO_BASECAMP_MODULES ?= $(HOME)/.local/share/Logos/LogosBasecampDev/modules
basecamp-install: basecamp
	mkdir -p "$(LMAO_BASECAMP_MODULES)/agent" "$(LMAO_BASECAMP_MODULES)/agent_ui"
	cp basecamp/agent-module/result/lib/agent_plugin.so "$(LMAO_BASECAMP_MODULES)/agent/"
	cp basecamp/agent-module/metadata.json              "$(LMAO_BASECAMP_MODULES)/agent/"
	cp basecamp/agent-ui/result/lib/Main.qml            "$(LMAO_BASECAMP_MODULES)/agent_ui/"
	cp basecamp/agent-ui/result/lib/metadata.json       "$(LMAO_BASECAMP_MODULES)/agent_ui/"
	@echo
	@echo "Installed to $(LMAO_BASECAMP_MODULES)"
	@echo "Launch Basecamp; the 'agent_ui' tab should appear."
	@echo "Make sure LMAO_BIN, LIBLOGOSDELIVERY_LIB_DIR, and LD_LIBRARY_PATH"
	@echo "are exported in the shell that launches Basecamp so the spawned"
	@echo "lmao agent run subprocess can find liblogosdelivery.so."

# Build portable .lgx packages — fully self-contained, no /nix/store
# references at runtime. These work with the prebuilt LogosBasecamp
# AppImage (Linux) / DMG (macOS) downloaded from the Basecamp releases.
#
# Output: dist/agent.lgx + dist/agent_ui.lgx
basecamp-lgx:
	cd basecamp/agent-module && nix build .#lgx-portable -L
	cd basecamp/agent-ui && nix build .#lgx-portable -L \
		--override-input agent "path:$(CURDIR)/basecamp/agent-module"
	mkdir -p dist
	cp basecamp/agent-module/result/logos-agent-module-lib.lgx dist/agent.lgx
	cp basecamp/agent-ui/result/logos-agent_ui-module.lgx       dist/agent_ui.lgx
	@echo
	@echo "Portable LGX packages:"
	@ls -lh dist/agent.lgx dist/agent_ui.lgx
	@echo
	@echo "Install into a prebuilt Basecamp with:"
	@echo "  make basecamp-lgx-install LMAO_BASECAMP_MODULES=<path>"
	@echo "or via the in-app Package Manager (drag/drop or import)."

# Install portable .lgx packages via lgpm. Override LMAO_BASECAMP_MODULES
# to point at the prebuilt Basecamp's modules dir (e.g. AppImage's
# unpacked $APPDIR/usr/share/Logos/modules, or
# ~/.local/share/Logos/LogosBasecamp/modules for the prebuilt's user dir).
basecamp-lgx-install: basecamp-lgx
	@which lgpm > /dev/null || { echo "error: lgpm not on PATH"; \
		echo "       lgpm ships with the Basecamp release; ensure"; \
		echo "       \$$BASECAMP_DIR/bin is on \$$PATH"; exit 1; }
	mkdir -p "$(LMAO_BASECAMP_MODULES)"
	lgpm --modules-dir "$(LMAO_BASECAMP_MODULES)" install --file dist/agent.lgx
	lgpm --modules-dir "$(LMAO_BASECAMP_MODULES)" install --file dist/agent_ui.lgx
	lgpm --modules-dir "$(LMAO_BASECAMP_MODULES)" list
	@echo
	@echo "Installed. Restart Basecamp to pick up the new modules."

# Run the ping-pong demo (optionally encrypted)
demo-ping:
	~/.cargo/bin/cargo run --example ping_pong

demo-ping-encrypted:
	~/.cargo/bin/cargo run --example ping_pong -- --encrypt

# Run the echo agent
demo-echo:
	~/.cargo/bin/cargo run --example echo_agent

# Logos Core e2e demo (stub)
demo-logos-core:
	~/.cargo/bin/cargo run -p logos-core-e2e-demo

# Logos Core e2e demo (real SDK)
# Usage: LOGOS_CORE_LIB_DIR=/path/to/logoscore make demo-logos-core-real
demo-logos-core-real:
	LD_LIBRARY_PATH="$(LOGOS_CORE_LIB_DIR):$$LD_LIBRARY_PATH" \
		LOGOS_CORE_LIB_DIR=$(LOGOS_CORE_LIB_DIR) \
		~/.cargo/bin/cargo run -p logos-core-e2e-demo

# Build MCP bridge
mcp:
	~/.cargo/bin/cargo build -p logos-messaging-a2a-mcp --release

# Build CLI
cli:
	~/.cargo/bin/cargo build -p logos-messaging-a2a-cli --release

# Build FFI shared library
ffi:
	~/.cargo/bin/cargo build -p logos-messaging-a2a-ffi --release

# Clean build artifacts
clean:
	~/.cargo/bin/cargo clean
