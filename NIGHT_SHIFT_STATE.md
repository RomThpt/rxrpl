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

- [ ] T16 [kind=code,deps=T15,T13]: Build ValidationsTrie aggregator on top of LedgerTrie + validation_aggregator
  - acceptance: new struct ValidationsTrie tracks (NodeId -> latest Validation) and feeds counts into LedgerTrie
  - acceptance: get_preferred(current_seq) returns hash of preferred branch tip at or above current_seq
  - acceptance: 5+ tests including conflicting validations from same node (latest wins) and trusted-set filter
  - globs: crates/consensus/src/validations_trie.rs, crates/consensus/src/lib.rs

- [ ] T17 [kind=code,deps=T16]: Wire ValidationsTrie into ConsensusEngine.start_round to detect wrong-prev-ledger via preferred-branch
  - acceptance: when ValidationsTrie.preferred() != engine.prev_ledger and trusted-validator support >= WRONG_PREV_LEDGER_THRESHOLD, return WrongPrevLedgerDetected
  - acceptance: existing wrong_prev_ledger_votes HashMap path becomes secondary (kept for proposals only)
  - globs: crates/consensus/src/engine.rs

- [ ] T19 [kind=code,deps=T18]: Replace peer_positions: HashMap with ProposalTracker; preserve existing engine API
  - acceptance: peer_proposal delegates to tracker, increments dispute counters only on accepted updates
  - acceptance: existing multi_node integration test still passes
  - globs: crates/consensus/src/engine.rs

- [ ] T20 [kind=code,deps=T18]: Dispute lifecycle — port goXRPL disputed-tx vote tracking with avalanche thresholds
  - acceptance: DisputedTx in types.rs gains methods update_vote(node_id, voted_yes), our_vote(threshold) returning bool
  - acceptance: thresholds match rippled (50% before mid, 65% after mid, 70% late, 95% stuck)
  - acceptance: 5+ tests, including threshold transitions across consensus rounds
  - globs: crates/consensus/src/types.rs, crates/consensus/src/engine.rs

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

### In progress

### Done
- T01 — close_resolution.rs rippled bins [10,20,30,60,90,120], commit 85ccdc0
- T02 — close_resolution next_resolution port, commit f78e4ba (23 tests green)
- T07 — stobject SOTemplate fields for STValidation, commit 1734f6f (211/211 tests green)
- T12 — validation_current.rs freshness window, commit b0f2d47 (7/7 tests green)
- T13 — validation_aggregator freshness gate, commit 27b3f77 (12/12 tests green)
- T03 — wire next_resolution into engine, commit 04a1799 (126/126 consensus tests green)
- T13b — fix node.rs quorum tests for freshness gate, commit d75a81f (40/40 node tests green)
- T15 — LedgerTrie data structure, commit 900846a (13 tests green; 2 NIGHT-SHIFT-REVIEW for span compression deferred)
- T04 — eff_close_time clamp prior+1 in engine.rs, commit 5b24c17 (143/143 consensus tests green)
- T21 — manifest::create_signed outbound + 3 round-trip tests, commit 580689c (217/217 overlay tests green)
- T23 — 27 new LedgerTrie tests ported from rippled (40 total), commit 9cf032b
- T05 — apply eff_close_time + prior_close_time field, commit ed05efd (172/172 consensus tests green)
- T18 — ProposalTracker module, commit f41061e (7/7 tests green)
- T22 — stobject_decode fuzz harness, commit dea175f (cargo fuzz build OK)

### Blocked
<!-- Tasks blocked on external dependencies, see PROBLEMS.md for details. -->

### WIP (max retries reached)
<!-- Tasks marked [WIP] after 3 unsuccessful fix attempts. -->

---

## Validation results

Last run: 2026-04-27T14:21Z
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
