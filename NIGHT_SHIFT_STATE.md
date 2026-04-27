---
nightshift_version: 1
repo: rxrpl
initialized_at: 2026-04-27T11:48:21Z
date: 2026-04-27
phase: 2  # 0=init, 1=planned, 2=running, 3=auditing, 4=finalizing, 5=done
current_task: null
time_budget_hours: 10
nightly_branch: nightly/2026-04-27
whitelist:
  - "crates/consensus/src/**/*.rs"
  - "crates/consensus/tests/**/*.rs"
  - "crates/consensus/Cargo.toml"
  - "crates/overlay/src/**/*.rs"
  - "crates/overlay/tests/**/*.rs"
  - "crates/overlay/Cargo.toml"
  - "crates/codec/src/binary/**/*.rs"
  - "crates/node/src/node.rs"
  - "fuzz/fuzz_targets/**/*.rs"
  - "fuzz/Cargo.toml"
  - "NIGHT_SHIFT_LOG.md"
  - "gaps.md"
forbidden_paths:
  - ".github/**"
  - "NIGHT_SHIFT_STATE.md"
  - "NIGHT_SHIFT_CONTRACTS.md"
  - "NIGHT_SHIFT_ENRICHED_SPEC.md"
  - "NIGHT_SHIFT_PROBLEMS.md"
  - ".nightly-lock"
  - "Cargo.lock"
---

# NightShift State — rxrpl — 2026-04-27

> Single source of truth for the autonomous run. The orchestrator and agents read this file at the start of every Ralph-loop iteration. The "Tasks" section below is FROZEN at /nightly init time and protected by `.nightly-lock`. Only the "Validation results" and "Checkpoints" sections may be mutated during execution.

---

## Tasks

### Ready

- [ ] T03 [kind=code,deps=T02]: Wire next_resolution into ConsensusEngine.start_round; remove on_agreement/on_disagreement consecutive-counter pathway
  - acceptance: start_round records prev_agree from previous round, computes new resolution via next_resolution(prev_res, prev_agree, ledger_seq)
  - acceptance: AdaptiveCloseTime API simplified — old hooks deprecated
  - globs: crates/consensus/src/engine.rs, crates/consensus/src/close_resolution.rs

- [ ] T04 [kind=code,deps=T01]: Implement effCloseTime clamp to prior_close_time+1
  - acceptance: new fn eff_close_time(close_time: u32, resolution: u32, prior_close_time: u32) -> u32 returns max(round_close_time(...), prior+1) when close_time != 0
  - acceptance: returns 0 when close_time == 0 (matches rippled "untrusted close time" sentinel)
  - acceptance: 4+ unit tests including clamp-active, clamp-inactive, zero passthrough
  - globs: crates/consensus/src/engine.rs

- [ ] T05 [kind=code,deps=T04]: Apply eff_close_time in establish-phase aggregation (replace bare round_close_time call sites)
  - acceptance: engine.rs:572 and engine.rs:577 + sibling call sites use eff_close_time(_, _, prior_close_time)
  - acceptance: ConsensusEngine carries prior_close_time field, set by start_round
  - acceptance: existing engine tests still pass; new test asserts monotonic close_time across two rounds
  - globs: crates/consensus/src/engine.rs

- [ ] T06 [kind=tests,deps=T05]: Property tests for adaptive bins and effCloseTime via proptest
  - acceptance: proptest harness in tests/close_time_props.rs round-trips next_resolution and asserts eff_close_time(_, _, prior) > prior whenever non-zero input
  - acceptance: 200+ generated cases, no panics
  - globs: crates/consensus/tests/close_time_props.rs, crates/consensus/Cargo.toml

- [ ] T09 [kind=code,deps=T08]: Update identity::sign_validation to encode full SOTemplate (canonical sort by field tag)
  - acceptance: builder appends fields in canonical SField order; sfSignature excluded from signing payload
  - acceptance: signing_payload populated with the strip-result so verifier can replay
  - acceptance: parity test against a captured rippled validation hex (hardcoded fixture) passes
  - globs: crates/overlay/src/identity.rs

- [ ] T10 [kind=code,deps=T09]: Validation decoder in overlay reconstructs all SOTemplate fields and signing_payload
  - acceptance: parse_validation in validation_aggregator/validator_list/proto_convert emits full Validation struct
  - acceptance: round-trip test: encode→decode preserves every optional field
  - globs: crates/overlay/src/validation_aggregator.rs, crates/overlay/src/proto_convert.rs

- [ ] T11 [kind=tests,deps=T10]: Property tests on STValidation encoding round-trip via proptest
  - acceptance: 500+ random Validation structs survive encode→decode without loss
  - acceptance: signing_hash stable across encode→decode→encode (idempotent)
  - globs: crates/overlay/tests/stvalidation_roundtrip.rs

- [ ] T14 [kind=code,deps=T12]: Apply same freshness check to incoming proposals (proposal staleness)
  - acceptance: ConsensusEngine.peer_proposal rejects when |now - close_time| > PROPOSAL_FRESHNESS (use 30s like rippled propRELAY_INTERVAL)
  - acceptance: tracing::debug log with reason; counter proposal_dropped_stale_total
  - globs: crates/consensus/src/engine.rs

- [ ] T15 [kind=code,deps=]: Port LedgerTrie data structure (rippled LedgerTrie.h) — single-writer, no concurrency
  - acceptance: crates/consensus/src/ledger_trie.rs implements insert(branch, ledger_seq, hash, count), remove, get_preferred_branch
  - acceptance: matches rippled spans (parent path, support count) — tip discovery returns branch with most cumulative support
  - acceptance: 8+ unit tests exercising single chain, fork, deeper-fork-with-less-support scenarios
  - globs: crates/consensus/src/ledger_trie.rs, crates/consensus/src/lib.rs

- [ ] T16 [kind=code,deps=T15,T13]: Build ValidationsTrie aggregator on top of LedgerTrie + validation_aggregator
  - acceptance: new struct ValidationsTrie tracks (NodeId -> latest Validation) and feeds counts into LedgerTrie
  - acceptance: get_preferred(current_seq) returns hash of preferred branch tip at or above current_seq
  - acceptance: 5+ tests including conflicting validations from same node (latest wins) and trusted-set filter
  - globs: crates/consensus/src/validations_trie.rs, crates/consensus/src/lib.rs

- [ ] T17 [kind=code,deps=T16]: Wire ValidationsTrie into ConsensusEngine.start_round to detect wrong-prev-ledger via preferred-branch
  - acceptance: when ValidationsTrie.preferred() != engine.prev_ledger and trusted-validator support >= WRONG_PREV_LEDGER_THRESHOLD, return WrongPrevLedgerDetected
  - acceptance: existing wrong_prev_ledger_votes HashMap path becomes secondary (kept for proposals only)
  - globs: crates/consensus/src/engine.rs

- [ ] T18 [kind=code,deps=]: Port goXRPL ProposalTracker — peer position lifecycle (received_at, last_seen, prop_seq monotonicity)
  - acceptance: new module crates/consensus/src/proposal_tracker.rs with track(node_id, proposal) -> bool (false = older or duplicate)
  - acceptance: rejects out-of-order prop_seq for a given (node_id, prev_ledger)
  - acceptance: 6+ unit tests
  - globs: crates/consensus/src/proposal_tracker.rs, crates/consensus/src/lib.rs

- [ ] T19 [kind=code,deps=T18]: Replace peer_positions: HashMap with ProposalTracker; preserve existing engine API
  - acceptance: peer_proposal delegates to tracker, increments dispute counters only on accepted updates
  - acceptance: existing multi_node integration test still passes
  - globs: crates/consensus/src/engine.rs

- [ ] T20 [kind=code,deps=T18]: Dispute lifecycle — port goXRPL disputed-tx vote tracking with avalanche thresholds
  - acceptance: DisputedTx in types.rs gains methods update_vote(node_id, voted_yes), our_vote(threshold) returning bool
  - acceptance: thresholds match rippled (50% before mid, 65% after mid, 70% late, 95% stuck)
  - acceptance: 5+ tests, including threshold transitions across consensus rounds
  - globs: crates/consensus/src/types.rs, crates/consensus/src/engine.rs

- [ ] T21 [kind=code,deps=]: Manifest signing — outbound creation + relay broadcast
  - acceptance: new fn manifest::create_signed(master: &KeyPair, ephemeral: &KeyPair, sequence: u32, domain: Option<&str>) -> Vec<u8> producing rippled-compatible bytes
  - acceptance: relay path forwards manifests on receipt to peer set excluding origin
  - acceptance: round-trip test encode→parse→verify_signatures green
  - globs: crates/overlay/src/manifest.rs

- [ ] T22 [kind=tests,deps=T07]: Fuzz harness for STObject parser (decode_field_id + decode_vl_length)
  - acceptance: new fuzz/fuzz_targets/stobject_decode.rs feeds arbitrary bytes into decode_field_id and decode_vl_length
  - acceptance: cargo +nightly fuzz build stobject_decode succeeds
  - acceptance: registered in fuzz/Cargo.toml
  - globs: fuzz/fuzz_targets/stobject_decode.rs, fuzz/Cargo.toml

- [ ] T23 [kind=tests,deps=T15]: Unit tests for LedgerTrie covering 12+ scenarios from rippled LedgerTrie_test.cpp
  - acceptance: tests cover empty trie, single-branch, fork-equal-support, fork-tilted-support, removal, deep-trie (depth>20)
  - acceptance: file mirrors structure of rippled test
  - globs: crates/consensus/src/ledger_trie.rs (inline #[cfg(test)] mod tests)

- [ ] T24 [kind=qa,deps=T05,T10,T17]: xrpl-hive smoke + propagation cross-impl run after merges
  - acceptance: ./bin/xrpl-hive --sim smoke --client rxrpl passes (3/3)
  - acceptance: ./bin/xrpl-hive --sim propagation --client rxrpl,rippled_2.3.0 reaches "validated_ledger.seq advances" cross-impl, no validation rejected with "bad signature"
  - acceptance: log excerpt + workspace/logs path captured in NIGHT_SHIFT_LOG.md
  - globs: NIGHT_SHIFT_LOG.md, gaps.md

- [ ] T25 [kind=qa,deps=T24]: xrpl-hive consensus + sync sims pass cross-impl
  - acceptance: ./bin/xrpl-hive --sim consensus --client rxrpl,rippled_2.3.0 — at least 1 round closed with rxrpl validator participating
  - acceptance: ./bin/xrpl-hive --sim sync --client rxrpl — late joiner reaches mainnet_seq within 60s
  - globs: NIGHT_SHIFT_LOG.md

- [ ] T26 [kind=qa,deps=T11,T22,T23]: Run full property test + fuzz smoke (60s each) and surface any crash
  - acceptance: cargo test -p rxrpl-consensus -p rxrpl-overlay --all-features green
  - acceptance: cargo +nightly fuzz run stobject_decode -- -max_total_time=60 — exits clean
  - acceptance: cargo +nightly fuzz run validation_deser -- -max_total_time=60 — exits clean
  - globs: NIGHT_SHIFT_LOG.md

- [ ] T08b [kind=code,deps=T08]: Update 14 Validation { ... } literal sites to add ..Default::default()
  - acceptance: nightly-agent-T08-T08 branch merges cleanly into nightly/2026-04-27 with all dependent crates building
  - acceptance: production sites (engine.rs:808, proto_convert.rs:221, node.rs:1800) compile
  - acceptance: test sites in identity.rs (5), validation_aggregator.rs, node.rs (4), checkpoint.rs, peer_handshake.rs all compile
  - globs: crates/consensus/src/engine.rs, crates/overlay/src/proto_convert.rs, crates/overlay/src/identity.rs, crates/overlay/src/validation_aggregator.rs, crates/overlay/tests/peer_handshake.rs, crates/node/src/node.rs, crates/node/src/checkpoint.rs

- [ ] T13b [kind=code,deps=T13]: Fix 2 pre-existing node.rs tests broken by T13 freshness gate
  - acceptance: tests `quorum_auto_set_integration` and `quorum_not_overridden_when_configured` in crates/node/src/node.rs use ripple_now()-aligned sign_time or call add_validation_at(v, now) instead of add_validation
  - acceptance: cargo test -p rxrpl-node --lib green
  - globs: crates/node/src/node.rs

### In progress
- T03 (agent-T03) — branch nightly-agent-T03-T03 — started 2026-04-27T14:10:04Z
- T13b (agent-T13b) — branch nightly-agent-T13b-T13b — started 2026-04-27T14:10:04Z
- T15 (agent-T15) — branch nightly-agent-T15-T15 — started 2026-04-27T14:10:04Z
- T08 (agent-T08) — branch nightly-agent-T08-T08 — fields added but blocks 14 consumer sites, awaiting T08b

### Done
- T01 — close_resolution.rs rippled bins [10,20,30,60,90,120], commit 85ccdc0
- T02 — close_resolution next_resolution port, commit f78e4ba (23 tests green)
- T07 — stobject SOTemplate fields for STValidation, commit 1734f6f (211/211 tests green)
- T12 — validation_current.rs freshness window, commit b0f2d47 (7/7 tests green)
- T13 — validation_aggregator freshness gate, commit 27b3f77 (12/12 tests green; 2 node tests need T13b fix)

### Blocked
<!-- Tasks blocked on external dependencies, see PROBLEMS.md for details. -->

### WIP (max retries reached)
<!-- Tasks marked [WIP] after 3 unsuccessful fix attempts. -->

---

## Validation results

Last run: 2026-04-27T14:08:10Z
- build: true
- test: false (engine.rs tests await T03; node.rs quorum tests await T13b)
- lint: false (rpc-api derivable_impls — pre-existing on main, untouched by nightly)

History:
- 2026-04-27T14:08:10Z — build=true test=false lint=false (planned fixes in queue: T03, T13b)

---

## Checkpoints

- 2026-04-27T11:48:21Z — phase 0 — initialized
- 2026-04-27T13:55:00Z — phase 1 — plan written, 26 tasks, 11 whitelist globs

---

## Audit reports

<!-- Phase 3 output. Each audit pass adds an entry. -->

---

## Notes for review (morning)

<!-- Auto-aggregated from NIGHT-SHIFT-REVIEW markers + PROBLEMS.md highlights at end of Phase 4. -->
