# Running an LMAO agent on a remote machine (in a container)

This guide walks through standing up an `lmao` agent on a server other
than the one you drive Basecamp / the CLI from. The agent gets its own
identity, dials the public Logos Dev Network, and becomes discoverable
to your local agent over the gossip mesh — same as if it were running
in the next tab.

The recipe is the standard repo `Dockerfile` + a single-service compose
override; nothing remote-specific is baked in.

## Prerequisites

On the remote host:

- Docker 24+ (or Docker Compose v2 standalone)
- Outbound TCP/30303 to the Logos Dev fleet (`delivery-*.logos.dev.status.im`)
- ~1 GB free disk for the image + libstorage data
- A way to feed the agent's executor — usually an OpenAI-compatible
  inference endpoint reachable from the container. If the model server
  runs on the host, plumb it via `extra_hosts: host.docker.internal:host-gateway`
  and point the executor at `http://host.docker.internal:<port>`.

You do **not** need:

- A public IP or port forwarding — the embedded Waku node uses libp2p
  NAT traversal and rendezvous via the public fleet.
- An entry-node from your laptop. Your laptop and the remote agent
  meet on the public mesh; they don't dial each other directly.

## 1. Pull the image

The pre-built image is published to GitHub Container Registry, so the
remote host doesn't need a Rust + Nim toolchain or the source tree:

```bash
docker pull ghcr.io/vpavlin/lmao:dev
```

(If the image is private — the default for personal GHCR namespaces —
authenticate first: `echo "$GH_PAT" | docker login ghcr.io -u <user>
--password-stdin`, where `$GH_PAT` is a token with `read:packages`. Or
flip the package to public on github.com so anonymous pulls work.)

Prefer building from source — for development, custom feature flags, a
fork, or air-gapped operation? Skip to the [Build from source](#build-from-source)
section at the bottom; the rest of the recipe stays the same.

## 2. Drop a single-agent compose file

This is one self-contained YAML — no source tree needed. Save it as
`docker-compose.yml` (or any name) in a directory on the remote host.

```yaml
services:
  agent:
    image: ghcr.io/vpavlin/lmao:dev
    container_name: lmao-agent
    user: "1000:1000"
    restart: unless-stopped
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
      # Persistent identity + libstorage data + audit-log uploads.
      # Keep this volume across restarts — the keyfile inside it is the
      # agent's stable pubkey that peers will whitelist / trust.
      - ./remote-data:/data
      # Daemon socket lives on a host-mounted dir so you can drive the
      # daemon from outside the container (e.g. with `docker exec`,
      # or by SSH-ing in and running `lmao daemon status`).
      - ./remote-data-sock:/run/lmao
    ports:
      # Optional — only useful if you'll dial this agent directly from
      # another machine on the same LAN. The mesh works fine without it.
      - "60010:60010/tcp"
      - "9010:9010/udp"
    environment:
      LMAO_PRESENCE_REANNOUNCE_SECS: "15"
      # Point the executor at whatever local model endpoint you have.
      # Examples:
      #   - Ollama on the host:    http://host.docker.internal:11434
      #   - lemonade-server:       http://host.docker.internal:8000
      #   - vLLM / llama.cpp:      http://host.docker.internal:<port>
      OPENAI_BASE_URL: http://host.docker.internal:11434/v1
    command: >
      --transport logos-delivery
      --storage libstorage
      --storage-data-dir /data/storage
      --storage-port 19200
      --keyfile /data/keyfile
      --tcp-port 60010 --udp-port 9010
      --daemon-socket /run/lmao/lmao.sock
      agent run
        --name remote-agent
        --capabilities text,summarize
        --exec "sed s/^/[remote]\\ /"
```

Replace `--exec "sed …"` with whatever executor binary you actually
want — `goose`, `pi`, `aider`, a shell script that pipes to your
preferred model. The container ships `goose` and `pi` out of the box;
both pick up `OPENAI_BASE_URL` for endpoint config.

## 3. Start

```bash
docker compose up -d
```

First start pulls the image (~300-500 MB compressed); subsequent starts
are instant. No build step on the remote host.

## 4. Confirm it's on the mesh

```bash
docker logs -f lmao-agent | grep -E "Dial successful|cVykME|peer-exchange"
```

You want to see successful dials to several `delivery-XX.logos.dev.status.im`
peer-IDs within the first 30s. If everything is failing with
`Noise handshake, peer id don't match!` — the public fleet's keys may
have rotated again; see [issues.md](issues.md).

Print the agent's identity:

```bash
docker exec lmao-agent /usr/local/bin/lmao --daemon-socket /run/lmao/lmao.sock info
```

You'll get the agent's name, secp256k1 pubkey, X25519 pubkey,
capabilities, and storage SPR. **The pubkey is the agent's stable
identity** — share it with anyone who should be able to find / trust /
delegate to this agent.

## 5. Find it from your laptop

On your local machine (Basecamp host or bare CLI):

```bash
lmao discover --capability text
```

The remote agent should appear in the peer list within ~30s of starting
(presence beacons every 15s by default, plus the gossip relay delay).
Once you see its pubkey, you can:

- Delegate by capability: `lmao delegate --capability summarize "your task"`
- Send directly: `lmao send <remote-pubkey> "your task"`
- Add it to your trust list (see [TRUST.md](TRUST.md)) if you want to
  scope what it accepts from you / what you accept from it.

## Operational notes

- **Persistent identity.** The keyfile lives in `./remote-data/keyfile`.
  Back it up; if you lose it the agent comes back with a new pubkey and
  trust relationships break.
- **Logs.** Container stderr is verbose Waku/libp2p tracing. For agent-
  level events, look at `docker exec lmao-agent /usr/local/bin/lmao
  --daemon-socket /run/lmao/lmao.sock logs` (per-task audit lines).
- **Audit trail uploads.** Each completed task uploads its full transcript
  to libstorage as a content-addressed blob. The response carries the CID;
  the data is fetchable from anywhere on the mesh that can reach a Codex
  node.
- **Storage size.** libstorage's data dir grows with serviced tasks +
  whatever the network gossips at this node. Plan for it; rotate or
  cap as you would any local content store.
- **Updating.** `docker compose pull && docker compose up -d`. The
  persistent volume survives the image swap, so identity + libstorage
  state carry over.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Agent never appears in your local `lmao discover` | Outbound TCP/30303 blocked, or fleet keys rotated | `docker logs` and look for successful dials. If none, check egress firewall; if many `Noise handshake mismatch`, see [issues.md](issues.md). |
| `Address already in use` on startup | Another `lmao` is using the same TCP/UDP/storage ports on the same host | Change `--tcp-port` / `--udp-port` / `--storage-port` (and matching `ports:` mapping) to free numbers. |
| `liblogosdelivery.so: cannot open shared object file` | Image was built without the lib (only possible when building from source — the published image always includes it) | Rebuild without cache: `docker compose build --no-cache agent`, or pull the published image. |
| `unauthorized` / `manifest unknown` on `docker pull` | GHCR package is private and you're not authenticated | `echo "$GH_PAT" \| docker login ghcr.io -u <user> --password-stdin` with a `read:packages` token, or set the package to public. |
| Executor returns empty / errors out | Inference endpoint unreachable from container, or model not loaded | `docker exec lmao-agent curl -fsS $OPENAI_BASE_URL/models` — if that fails, the container can't reach the model. |
| High CPU on idle agent | Stale entry-node peer-IDs in `liblogosdelivery`'s preset (see [issue 3858](https://github.com/logos-messaging/logos-delivery/issues/3858)) | Live with it for now, or pass `--entry-node` overrides if/when the upstream fix lands. |

## Build from source

If you can't (or don't want to) pull the published image — forking, custom
features, air-gapped network, distrust of an opaque binary blob —
clone the source on the remote host and build locally instead:

```bash
git clone https://github.com/vpavlin/lmao.git
cd lmao
docker compose -f docker-compose.yml build agent
```

Then in your `docker-compose.yml`, swap the `image:` line for a `build:`
block:

```yaml
services:
  agent:
    build:
      context: .
      network: host
    image: lmao:dev   # local tag; not pushed anywhere
    # ... rest of the service definition unchanged
```

First build is ~10 min (the Nim compile of `liblogosdelivery` is the
long pole; subsequent rebuilds are seconds). The repo's top-level
`docker-compose.yml` is also a working multi-agent local fleet
(alice + bob + pi-analyst) you can crib from.
