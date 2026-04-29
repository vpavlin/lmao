# Trust layer for LMAO

> **Status (v1):** designed, not yet implemented. The current codebase
> has only **L0 — pubkey identity** (signed `AgentCard`s, signed
> `PresenceAnnouncement`s). Everything below is the proposed direction.

## What we are actually trying to solve

LMAO routes tasks to agents by **self-declared capability**. A peer
`bob` puts `["code", "review"]` in his `AgentCard` and presence
announcement, and any agent that knows him will route a code-review
task his way. Nothing today verifies that:

1. Bob has the model / tool access he claims (capability honesty).
2. Bob will actually run the work he was paid for.
3. Bob isn't one of 1000 sybils flooding the queue with garbage.
4. The task data won't leave bob's host.

The honest cliff: **#1 is not solvable cryptographically at chat scale
this decade.** zkML for 7B+ models is hours per response on H100s
today. TEEs work but introduce a vendor trust root and break the
decentralisation story. Everything else (rate limits, sybil
resistance, attestations) helps with #3 and partly #2 — not #1.

So the trust layer can't *prove* honesty. The best it can do is make
lying **detectable, expensive, and bounded**, and let an operator pick
who they're willing to talk to.

## What v1 actually does

Friends only. **A local list of pubkeys you trust**, edited as a TOML
file, applied at two points:

- **Outgoing**: `delegate_task` only considers peers in the trust list.
- **Incoming**: `poll_tasks` rejects (or, with a flag, just logs)
  tasks from senders not in the list.

That's the whole v1 design. SSH `known_hosts` for agents. ~200 LOC of
Rust, no new cryptography, no new dependencies, no group ceremony, no
on-chain anything. It deliberately punts on every problem the previous
draft of this doc tried to solve at once, because:

- The threat model that matters first is "I want my agent to talk to
  my friends' agents and no one else." This solves that.
- It composes with everything heavier (libchat groups, RLN, EAS) we
  might add later. None of v1 has to be torn out.
- Bootstrap friction is a feature: you can't talk to strangers without
  an out-of-band introduction. That's exactly the property a
  small-honest-models-among-friends network wants.

## What can and cannot be cryptographically proven

The honest baseline. v1 only relies on rows 1–2; everything else is
roadmap.

| Claim | Provable? | How |
|---|---|---|
| "Agent X authored this message" | Yes | secp256k1 signature (shipped) |
| "X is in the trust list I personally curate" | Yes (trivially) | local TOML file (v1) |
| "X is a unique entity, not 100 sybils" | Hard / social | proof-of-personhood + social graph |
| "X is a member of group G" | Yes, anonymously | Semaphore-style group signaling |
| "X has staked Y tokens" | Yes, on-chain | EVM / LEZ contract |
| "X did not exceed rate R per epoch" | Yes | RLN nullifiers |
| "This output was produced by model `qwen2.5-coder:7b`" | **No, without TEE** | zkML at chat scale is years away |
| "This output is correct" | Never | domain-specific cross-checking + reputation |

## v1 — Friend-keyring

### TOML schema

`~/.config/lmao/trust.toml` (override with `--trust-file`):

```toml
# Mode: "off" disables filtering entirely (default if file missing).
#       "enforce" rejects untrusted senders silently.
#       "log"     accepts but logs untrusted senders — useful while building
#                 your trust list.
mode = "enforce"

[[peer]]
nickname     = "alice"
pubkey       = "02ab1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab"
# Empty / missing = trust for any capability. Non-empty = trust only when
# the matched capability is in this list.
capabilities = []
notes        = "met at ETHPrague 2026"

[[peer]]
nickname     = "bob"
pubkey       = "03cd9876543210cdef9876543210cdef9876543210cdef9876543210cdef9876"
capabilities = ["code", "review"]
notes        = "reviewer agent on bob's laptop"
```

The pubkey format is the same secp256k1 compressed-hex string already
used in `AgentCard.public_key`. No new identity primitive.

### Filter integration points

Two hooks, both already-trivial sites in the existing code:

- **Outgoing** —
  `crates/logos-messaging-a2a-node/src/delegation.rs:31-68`. Each
  `DelegationStrategy` arm builds a candidate peer list. Wrap each
  with `.filter(|(id, _)| trust.is_trusted_for(id, &capability))`
  before the `.next()` / strategy pick. `BroadcastCollect` and
  `RoundRobin` filter the same way.

- **Incoming** —
  `crates/logos-messaging-a2a-node/src/tasks/poll.rs:52-54`. After
  `extract_task` returns a `Task`, check `trust.is_trusted(&task.from)`
  before pushing into the result vector. In `mode = "log"`, push and
  log; in `mode = "enforce"`, drop and emit a metric.

Both sites are 1–2 lines of change with the trust list in scope.

### Type sketch

New file: `crates/logos-messaging-a2a-core/src/trust.rs`.

```rust
pub struct TrustList {
    mode: TrustMode,
    entries: BTreeMap<String, TrustEntry>,  // pubkey -> entry
}

pub enum TrustMode { Off, Enforce, Log }

pub struct TrustEntry {
    pub pubkey: String,
    pub nickname: String,
    pub capabilities: Vec<String>,    // empty = any
    pub notes: Option<String>,
    pub added_at: SystemTime,
}

impl TrustList {
    pub fn load_from(path: &Path) -> io::Result<Self>;       // missing file → Off mode, empty
    pub fn save_to(&self, path: &Path)   -> io::Result<()>;
    pub fn default_path()                -> PathBuf;          // $XDG_CONFIG_HOME/lmao/trust.toml
    pub fn is_trusted(&self, pubkey: &str) -> bool;
    pub fn is_trusted_for(&self, pubkey: &str, capability: &str) -> bool;
    pub fn mode(&self) -> TrustMode;
    pub fn add(&mut self, entry: TrustEntry);
    pub fn remove(&mut self, pubkey: &str) -> Option<TrustEntry>;
    pub fn iter(&self)                    -> impl Iterator<Item = &TrustEntry>;
}
```

Plumbing: a new builder method
`LmaoNode::with_trust_list(self, list: Arc<TrustList>) -> Self`
(near `lib.rs:449`, alongside `with_registry`). Default = no list,
`mode = Off`, identical to today's behaviour.

### CLI

```
lmao trust list                                       # show current trust list
lmao trust add  <pubkey> --nickname <n> [--cap <c>]…  # add or replace
lmao trust remove <pubkey | nickname>                 # drop an entry
lmao trust mode (off|enforce|log)                     # toggle filter mode
lmao trust import <file>                              # merge another TOML
lmao trust export                                     # print TOML to stdout
```

`lmao agent run` reads the same file at startup. SIGHUP could reload
in v1.5; for v1, "restart the agent" is fine.

### Bootstrap UX

Two acceptable patterns at first:

1. **Manual**: paste the pubkey from `lmao info` over Signal / in
   person; both sides `lmao trust add`.
2. **QR / link share**: `lmao trust export-card` prints a base32 string
   encoding `(pubkey, nickname, default-capabilities)`; `lmao trust
   import-card <string>` consumes it. Same-room demos love this.

Both are out-of-band. That's the point.

## v2 — When this runs out

Two specific cases will retire v1:

### Transitive trust (PGP-style)

When a friend group grows past direct introductions, "alice trusts
charlie, I trust alice, so I'm willing to talk to charlie at weight 0.7"
gets useful. Implementation is a depth-1 walk over signed `TrustList`
exports. Still a local-only computation; no new on-chain machinery.
Decision is per-operator: do you accept a peer because *your friend's
friend* did?

### libchat groups

[logos-messaging/libchat](https://github.com/logos-messaging/libchat) —
Jazz's project — is a Rust library for Logos chat. **MLS group support
(GroupV1)** is in flight (PR #92, draft). Once it lands and a stable
client crate is published, libchat replaces both:

- The trust filter (group membership = trust set).
- The hand-rolled X25519 + ChaCha20 A2A envelope crypto in
  `logos-messaging-a2a-crypto` — MLS gives forward secrecy and
  post-compromise security for free.

That's a one-for-two trade and is the right time to migrate. Migrating
*just* the trust filter is not — libchat carries non-trivial weight
(parallel Ed25519 identity, Nix-built logos-delivery dep, group-admin
ceremony) that isn't justified for what is otherwise a TOML file.

Concrete migration shape, when the time comes:

- An LMAO "circle" = an MLS group. Group state is libchat's problem.
- `AgentCard` carries the group's epoch + member commitment instead of
  the local pubkey list.
- Discovery topic moves from global `/lmao/1/discovery/proto` to a
  per-group derived topic.
- Out-of-band invite = libchat's intro-bundle + add-commit dance,
  documented in `jimmy-claw/lmao#3` and `#8`.

## v3+ — Beyond friends

If LMAO ever grows past "people I've personally vetted" into an open
network of strangers, the heavier machinery from the previous draft of
this doc becomes interesting:

- **RLN** (vacp2p/zerokit) — bounded broadcast amplification on
  `BroadcastCollect`. Already at the transport layer for spam; lifting
  it to the application layer for delegation rate limits is a real
  use case.
- **Semaphore v4** — anonymous group membership when a circle wants to
  prove "one of us said this" without identifying the member.
- **EAS** as a substrate for delivered-task attestations. Offchain
  signed JSON anchored by hash; aggregation (OpenRank-style EigenTrust)
  comes later when there's volume.
- **ZKPassport for L1** — the only clean answer to "kicked agent
  rotates a fresh key" that doesn't require a vendor trust root.
- **Capability-honesty oracle** — challenge-response benchmarks against
  a known answer set, attestation-downvoting on failure. **The
  hardest unsolved problem in the stack.** Does not have a clean
  cryptographic answer at chat scale; depends on benchmark design,
  contamination resistance, and willingness to slash. Probably the
  load-bearing piece of credibility for production.

None of v3+ is being designed today. The friend-keyring composes with
all of it.

## What we are explicitly not building

- **TEEs.** Vendor lock + new trust root + side-channel history. If
  the threat model ever demands "this exact model ran inside a sealed
  enclave," Nitro / TDX / SEV-SNP exist. Not v1, not v2.
- **zkML chat-scale.** Not viable this decade for 7B+ models. Toy-scale
  (image classifiers, small NNs) already works via EZKL; note as cool,
  not as a production capability.
- **Per-task voting / Schelling-point oracles.** Heavy machinery,
  centralises a coordinator, doesn't scale to LMAO's per-task latency.
- **Token-curated registries.** Tried 2018-ish, didn't pan out;
  EigenTrust over attestations is the modern shape — when there's
  attestation volume to aggregate.
- **A custom group-membership cryptosystem.** If we need MLS we use
  libchat. If we need anonymous group signing we use Semaphore. If we
  need RLN we use zerokit. Rolling our own here is the wrong work.

## References

**Friend-keyring inspiration**
- [SSH `known_hosts(5)`](https://man.openbsd.org/sshd.8#SSH_KNOWN_HOSTS_FILE_FORMAT) — the original key-pinning model
- [git's `allowed_signers`](https://git-scm.com/docs/git-config#Documentation/git-config.txt-gpgsshallowedSignersFile) — same shape, applied to commit signing

**libchat / Logos chat**
- https://github.com/logos-messaging/libchat — Rust core
- jimmy-claw/lmao#3, jimmy-claw/lmao#8 — Jazz ↔ Jimmy on Rust bindings + MLS groups

**v3+ primitives (informational, not v1 dependencies)**
- https://github.com/vacp2p/zerokit — RLN Rust impl
- https://github.com/semaphore-protocol/semaphore-rs — Semaphore v4
- https://attest.org/ — EAS
- https://github.com/zkpassport — ZKPassport
- https://github.com/jvhs0706/zkllm-benchmark — zkLLM benchmark numbers (why not zkML)

---

*This doc is a thinking artifact for v1. The friend-keyring is the
intended next implementation step. Open issues on `vpavlin/lmao` to
push back before it gets written.*
