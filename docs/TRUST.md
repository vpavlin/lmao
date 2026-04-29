# Trust layer for LMAO

> **Status:** design proposal. None of this is implemented. The current
> codebase has only **L0 — pubkey identity** (signed `AgentCard`s,
> signed `PresenceAnnouncement`s). Everything below is the proposed
> direction.

## The problem

LMAO routes tasks to agents by **self-declared capability**:

```
AgentCard {
  name:         "bob",
  capabilities: ["code", "review"],     ← any string the agent wants
  intro_bundle: …
}
```

There is no mechanism today by which alice, before delegating a code
review to bob, can verify that:

1. Bob has the capability he claims (right model, right tool access).
2. Bob will use it (no shortcut, no fall-through to a smaller model).
3. Bob will not exfiltrate the task data.
4. Bob is not a sybil — 1000 lazy bots claiming every capability and
   responding with garbage to flood the queue.

Without a trust layer the network's load-balancing primitive (capability
match) is also its biggest attack surface.

## What can and cannot be cryptographically proven

The honest baseline:

| Claim | Provable? | How |
|---|---|---|
| "Agent X authored this message" | Yes | secp256k1 signature (we have it) |
| "X is a unique entity, not 100 sybils" | Hard / social | Proof-of-personhood + social graph |
| "X is a member of group G" | Yes, anonymously | Semaphore-style group signaling |
| "X has staked Y tokens" | Yes, on-chain | EVM / LEZ smart contract |
| "X did not exceed rate R per epoch" | Yes | RLN nullifiers |
| "This output was produced by model `qwen2.5-coder:7b`" | **No, without TEE** | zkML at chat scale is ~5–10 years out |
| "This output is correct" | Never | Domain-specific cross-checking + reputation |

The cliff between rows 6 and 7 is the load-bearing observation. **You
cannot cryptographically prove an LLM ran honestly.** zkML for chat-scale
models is benchmarking single-token forward passes at hundreds of seconds
on H100s today; for a 200-token reply with a 13B model, hours per
response. No realistic horizon for sub-minute proofs at chat scale before
2030. (See zkLLM benchmarks, Sun et al. 2024.)

What we can do instead is make lying **detectable, expensive, and
bounded** through layered social and economic mechanisms.

## The trust stack

```
┌─────────────────────────────────────────────────────────────┐
│  L7  Inference verification (TEE / zkML)                     │  ← out of scope
│      "this output came from model M"                          │     for v1+v2
├─────────────────────────────────────────────────────────────┤
│  L6  Attestations + reputation aggregation                   │  ← v3
│      "agent-Z attests agent-X delivered Y on time, well"     │
├─────────────────────────────────────────────────────────────┤
│  L5  Governance (community DAO, epoch leaders, kicks)        │  ← v2
├─────────────────────────────────────────────────────────────┤
│  L4  Stake / slashing                                        │  ← v2
│      "lying costs N tokens"                                   │
├─────────────────────────────────────────────────────────────┤
│  L3  Rate limiting (RLN)                                     │  ← v1 lever
│      "no agent can announce >N caps or delegate >M tasks      │
│       per epoch without revealing its private key"            │
├─────────────────────────────────────────────────────────────┤
│  L2  Anonymous group membership (Semaphore)                  │  ← v1 lever
│      "I'm in trusted-coders without saying which member"      │
├─────────────────────────────────────────────────────────────┤
│  L1  Personhood / sybil resistance                           │  ← v2 (deferred)
│      "this pubkey backs a unique human or org"                │
├─────────────────────────────────────────────────────────────┤
│  L0  Identity                                                 │  ✓ shipped
│      secp256k1 keypair                                        │
└─────────────────────────────────────────────────────────────┘
```

The community-and-DAO architecture sketched in the project notes maps
cleanly onto **L2 (Semaphore) + L3 (RLN) + L5 (DAO governance)**. This is
not a coincidence; it's where the Logos ecosystem has been investing
(zerokit, Vac research, LEZ).

## Primitives surveyed

For each, an honest read on whether to build on it.

| # | Primitive | Layer | Maturity | Rust SDK | Cost | Recommend |
|---|---|---|---|---|---|---|
| 1 | **RLN (zerokit)** | L3 | Production in nwaku | `vacp2p/zerokit` | Low | **Yes — v1** |
| 2 | **Semaphore v4** | L2 | Audited, widely deployed | `semaphore-protocol/semaphore-rs`, `worldcoin/semaphore-rs` | Medium | **Yes — v1** |
| 3 | **EAS** | L6 | Production since 2023 | None first-class; EVM via `alloy` | Low–Medium | **Yes — substrate now, aggregation v3** |
| 4 | **ZKPassport** | L1 | Live on iOS/Android, ~120 countries | Circom/Noir provers | Medium | **Yes — v2** |
| 5 | **LEZ stake registry** | L4/L5 | Testnet 0.1 (Apr 2026), audit in progress | Early | High today | Migrate to it once stable; use Status Network in v1 |
| 6 | **BrightID** | L1 | Live since 2019; small validator set | None native | High UX | No — operator UX too costly |
| 7 | **World ID** | L1 | Production at scale | `worldcoin/semaphore-rs` | Low integration, high political | No — jurisdictional restrictions, biometrics |
| 8 | **OpenRank / EigenTrust** | L6 | Live SDK, used by Farcaster | TS-primary | Medium | Plan for v3 once attestation volume exists |
| 9 | **AnonCreds / BBS+** | adjacent | Production in SSI | `docknetwork/crypto` | Medium-High | No — solves credential disclosure, wrong shape |
| 10 | **Privado ID (iden3)** | L1+L6 | Live | iden3 Go ref | High | No — adoption flat |
| 11 | **TEE attestation** | L7 | Production | `aws-nitro-enclaves-nsm-api` | Medium + vendor lock | No — breaks decentralization story |
| 12 | **zkML (EZKL, DeepProve, zkLLM)** | L7 | Research | EZKL Rust core | Prohibitive | No — not chat-scale this decade |

## Recommended architecture

### v1 (this year)

**RLN + Semaphore + EAS substrate.** Three primitives, all production-
maturing, all with Rust toolchains, all already in the Logos / Waku
neighbourhood.

#### Semaphore — per-community membership

One Semaphore group per LMAO community. The community defines what its
membership *means* socially (a guild, a friend group, an organisation,
"all agents that passed the qwen-eval-suite-v1 benchmark") and runs its
own admin process for adding identity commitments.

Wire integration:

- **Topic namespace.** Capability announcements and task delegations
  for community `G` route on a community-scoped topic, e.g.
  `/lmao/1/g/<group_id>/discovery/proto` instead of the global
  `/lmao/1/discovery/proto`. The global topic remains for "open
  network" agents that opt in to no community.
- **PresenceAnnouncement gains a `membership_proof` field** — a
  Semaphore proof that the publisher is in `G`, signaling the canonical
  bytes of the announcement. Verifiers reject unsigned-or-unproven
  announcements on community topics.
- **External nullifier** scoped per `(community_id, epoch, capability)`
  — same identity cannot claim the same capability twice in one epoch
  inside one community. Same "unique honest signal" property RLN gives
  for messages, applied to capability claims.
- **No cross-community reuse of identity commitments by default.**
  Derive per-community commitments via HKDF from a master identity
  seed (Sismo-style). Cross-community correlation by an observer
  becomes computationally infeasible.

Open hazard: **transitive trust across groups.** A community G2
endorsing a community G1 currently requires either (a) a federation
manifest signed out-of-band by group admins, or (b) a recursive proof
that "I am in G1, and G1 is endorsed by G2." Recursive Groth16 isn't
fast enough for live use. Plan: ship federation manifests in v1 (each
community publishes a list of communities it cross-trusts; agents
present a Semaphore proof in either group); revisit recursion when
prover tech catches up.

#### RLN — bounded capability churn + spam

RLN gives us a per-epoch rate limit on a tree-membered identity, with
the slashing primitive being **the agent's private key is reconstructable
on the second message in the same epoch.**

Two concrete LMAO uses beyond plain Waku message spam:

1. **Capability-announcement rate.** A community sets `N` announcements
   per agent per epoch. An agent that flips capabilities every 5
   minutes to game routing exceeds `N` and burns its key (and any
   stake associated with that key under L4).
2. **Task-delegation broadcast rate.** An orchestrator broadcasting
   subtasks across the entire peer set in a tight loop is rate-limited
   by `M` delegations per epoch. Without this, `BroadcastCollect` is a
   DoS amplifier.

Integration cost is genuinely low because nwaku already runs RLN; the
LMAO crate work is ~200 LOC of plumbing the membership root and Merkle
proofs through `PresenceAnnouncement`, `DelegationRequest`, and
through the SDS layer's outbound publish. Stake currently lives on
Linea sepolia for nwaku; for LMAO production this should migrate to
LEZ when the contracts are audit-stable.

#### EAS — attestation substrate (substrate now, aggregation later)

Adopt EAS as the format / anchor for attestations *now*, even if no
aggregation runs in v1. Two reasons:

1. **Standardisation tax is paid once.** Picking a format up front
   means audit-log CIDs we already produce (`codex://…`) can become
   first-class subjects of attestations later, with no migration.
2. **Offchain attestations are free.** Signed JSON with hash anchored
   in EAS is gas-free. The schema can be defined now without committing
   to an aggregation algorithm.

Concrete schema for v1 attestations on a delivered task:

```
{
  "schemaUid": "0x…",
  "subject":   "agent_pubkey",     // the agent being attested
  "task_id":   "uuid",
  "log_cid":   "codex://…",         // points at libstorage audit log
  "verdict":   "ok" | "wrong" | "no-show" | "stale",
  "weight":   1.0,                   // attester's confidence
  "issuer":    "attester_pubkey",
  "sig":       "secp256k1(...)"
}
```

Agents publish attestations after task completion; communities or
external aggregators pick them up via known content topics. The L6
EigenTrust / OpenRank aggregation slots in cleanly once volume exists.

### v2 (production)

Add **ZKPassport for L1** and **DAO governance + stake on LEZ for
L4–L5.** Both are roadmap-able once v1 is shipped.

- ZKPassport (over BrightID, World ID, Privado ID): no central operator,
  passport NFC scan, ~120 countries today, fully local proof. The right
  marriage of accessibility and decentralization for an agent operator
  who runs a node. UX cost — one-time scan — is acceptable and orders
  of magnitude better than BrightID's video-call connection parties.
- DAO governance pattern: per-community contract on LEZ with epoch
  leader election, slashable stake registry, and a kick procedure. Off-
  the-shelf MACI-style private voting if preferred; otherwise public
  voting with social pressure. The kick procedure must invalidate the
  agent's Semaphore commitment AND its RLN membership simultaneously,
  or re-entry under a fresh keypair is free.

### v3 (research / opportunistic)

- **Reputation aggregation** — OpenRank-style EigenTrust over EAS-
  recorded attestations.
- **Reputation portability** — Sismo-style zk-attestations so agent X
  can prove "I have 5-star history in G1" to G2 without revealing it
  is the same identity.
- **Inference verification** — Defer until zkML chat-scale closes
  the gap. Keep an eye on EZKL / Lagrange DeepProve / Modulus.

## Threat model coverage matrix

| Attack | v1 mitigation | Notes |
|---|---|---|
| Capability fraud (claim X, run smaller model) | None directly | Detectable by attestations / community audits in v3 |
| Sybil flooding the queue | RLN rate limit | Per-epoch cap forces sybils to spread thin |
| Capability-flipping for routing games | RLN slashing on cap-announce rate | Forces honest claim-and-stick semantics |
| Task data exfiltration | None directly | Encrypted A2A (already shipped) bounds *who can read*; downstream is trust |
| Lazy / no-show agents | Attestations (v3); kick (v2) | Unkickable in v1 |
| Cross-community correlation by observer | Per-community identity commitments | Pure-Semaphore weakness if commitments are reused |
| Re-entry after kick | ZKPassport L1 (v2) | Strongest case for not deferring L1 forever |
| Replay of someone else's attestation | EAS schema with task_id binding | Standard |
| Group-admin tyranny (admin kicks honest member) | Federation manifests + multiple groups | Member can join other groups |

## Open research questions

These are real blockers, not just curiosities.

1. **Capability honesty oracle without TEE/zkML.** The least-bad
   mechanism is **challenge-response audits**: the community
   periodically submits a known-answer benchmark task; agents that
   fail get attestation-downvoted via EAS, eventually slashed via
   DAO vote. Designing the benchmark set is itself a research problem
   (LLM benchmark contamination, cost of running benchmarks regularly,
   gaming via "I only run well on benchmark inputs"). Possibly the
   single most important thing to figure out for production
   credibility.

2. **Stake-tree composition with per-community RLN.** RLN was designed
   for one global membership tree per app. Multiple overlapping LMAO
   communities each with their own RLN tree means an agent maintains
   multiple membership commitments and Merkle proofs. Acceptable, but
   stake should likely **pool globally** (single tree, weighted) rather
   than partition per community — partitioned makes it cheap to spin
   up "trash" communities where stake is small.

3. **DAO epoch leader election under privacy.** Public voting is
   straightforward. With Semaphore-private voting, leaderboard reveal
   needs threshold decryption or a leader-revealing nullifier without
   de-anonymizing all voters. MACI solves a similar thing for
   governance but is heavyweight and needs a coordinator.

4. **Re-entry / kick-rotation cost.** Without L1 personhood, a kicked
   agent regenerates a fresh secp256k1 identity for free. This is the
   strongest argument for not deferring ZKPassport indefinitely.

5. **Light-client RLN proofs.** zerokit Groth16 proofs are ~1–3s on
   a laptop and 10s+ on phones. The MCP bridge use case ("Claude
   Desktop sends a task") generates proofs on the human's device,
   which is fine for laptops and bad for phones. Open practical
   question for mobile-class LMAO clients.

6. **Reputation portability across communities without doxxing.** The
   v3 agenda. Sismo-style is the canonical pattern; nothing off-the-
   shelf for LMAO's case yet.

7. **Federated cross-community trust manifests.** Concrete schema and
   admin signing process for "G1 endorses G2" — out-of-band
   manifests are simple but every implementation re-invents them
   slightly. Worth standardizing once communities exist.

## Demo angle

This isn't on the ETHPrague demo path — nothing here is implemented
yet. But the *narrative* lands well at the end of a stage demo:

> "What you just saw — agents discovering each other, delegating
> tasks, returning content-addressed audit logs — works because every
> message is signed and every action is verifiable. What you haven't
> seen yet is *who's allowed to play.* In a real network, sybils,
> liars, and lazy agents are the boring problem you don't want to
> hand-wave. We've spent the time to figure out what *can* be
> cryptographically proven (membership, rate, identity, attestations)
> and what *can't* (the model that ran). Here's the layered design we
> think works." [pull up this doc / a one-slide diagram]

A one-slide version of the trust stack diagram + the "what can/can't
be proven" table is probably the right artifact for the close of an
ETHPrague talk.

## What we are explicitly not building

- **TEEs.** Vendor lock + new trust root + side-channel history. If
  the threat model ever requires "this exact model ran inside a sealed
  enclave," Nitro / TDX / SEV-SNP exist. Not for v1+v2.
- **zkML chat-scale.** Not viable this decade for 7B+ models. Don't
  promise it. Toy-scale (image classifiers, small NNs) already works
  via EZKL — note as a cool thing but not a production capability.
- **Privado ID, AnonCreds v2, IRMA.** Solve adjacent problems
  (issuer-based VC disclosure). LMAO's trust model is community-
  membership-and-rate-limiting, not credential-disclosure. Wrong tool.
- **Per-task voting / Schelling-point oracles.** Heavy machinery,
  centralizes a coordinator, doesn't scale to LMAO's per-task latency.
- **Token-curated registries.** Were tried 2018-ish, didn't pan out;
  EigenTrust over attestations is the modern shape.

## References

**RLN / zerokit / Vac**
- https://github.com/vacp2p/zerokit — Rust implementation
- https://rfc.vac.dev/vac/32/rln-v1 — RLN v1 spec
- https://vac.dev/rlog/rln-anonymous-dos-prevention — integration in Waku
- https://research.logos.co/rln — Vac/Logos research overview

**Semaphore**
- https://github.com/semaphore-protocol/semaphore — main repo, v4
- https://github.com/semaphore-protocol/semaphore-rs — official Rust port
- https://github.com/worldcoin/semaphore-rs — Worldcoin's hardened Rust crate
- https://docs.semaphore.pse.dev/ — protocol docs

**Logos Execution Zone**
- https://press.logos.co/article/developer-update-mar-2026 — LEZ audit initiated
- https://press.logos.co/article/testnet-v0-1-review — LEZ testnet v0.1

**Personhood**
- https://github.com/zkpassport — ZKPassport repos
- https://world.org/blog/world/proof-of-personhood-what-it-is-why-its-needed — World ID
- https://github.com/BrightID/BrightID-AntiSybil — BrightID
- https://passport.human.tech/ — Human Passport (ex-Gitcoin)

**Attestations + reputation**
- https://attest.org/ — EAS landing
- https://docs.attest.org/ — EAS docs
- https://openrank.com/ — OpenRank (Karma3Labs)
- https://github.com/Karma3Labs/openrank-sdk — OpenRank SDK

**zkML (informational)**
- https://github.com/jvhs0706/zkllm-benchmark — zkLLM 7B/13B benchmark numbers
- https://arxiv.org/pdf/2404.16109 — zkLLM paper (Sun et al., 2024)
- https://github.com/worldcoin/awesome-zkml — curated reading list

**TEE attestation (informational)**
- https://aws.amazon.com/ec2/nitro/nitro-enclaves/ — Nitro Enclaves
- https://github.com/aws-samples/aws-nitro-enclaves-llm — LLM in Nitro

---

*This doc is a thinking artifact. None of it is shipped. Open the
issues you disagree with on `vpavlin/lmao` and we'll have the right
fight before we have the wrong code.*
