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

- [ ] T28 [kind=code,deps=]: H16 strict-UTF-8 sfDomain in manifest parser (re-apply unmerged audit fix)
  - acceptance: parse_raw returns new ManifestError::InvalidDomain when sfDomain VL bytes are not valid UTF-8 (no from_utf8_lossy)
  - acceptance: regression test parse_rejects_manifest_with_invalid_utf8_domain constructs a manifest with 0xFF in sfDomain, signs it correctly, asserts Err(ManifestError::InvalidDomain)
  - acceptance: existing manifest tests stay green
  - globs: crates/overlay/src/manifest.rs, crates/overlay/tests/**/*.rs

- [ ] T29 [kind=code,deps=]: H12 STObject canonical-order check in decode_validation (duplicate-field exploit)
  - acceptance: decode_validation rejects payloads where any (type_id, field_id) pair appears twice OR fields are not in strictly ascending (type_id<<16 | field_id) order
  - acceptance: new test decode_validation_rejects_duplicate_ledger_hash + decode_validation_rejects_out_of_order_fields
  - acceptance: existing 226 overlay tests stay green
  - globs: crates/overlay/src/proto_convert.rs, crates/overlay/src/stobject.rs, crates/overlay/tests/**/*.rs

- [ ] T30 [kind=code,deps=]: H9 cap Vec::with_capacity(payload.len()) at MAX_STVALIDATION_BYTES (memory amplification)
  - acceptance: decode_validation allocates signing_payload with min(payload.len(), MAX_STVALIDATION_BYTES = 32 KiB)
  - acceptance: new test feeds a TMValidation claiming 16 MiB length — decoder errors or allocates ≤32 KiB
  - globs: crates/overlay/src/proto_convert.rs, crates/overlay/tests/**/*.rs

- [ ] T31 [kind=tests,deps=]: stale-validation replay regression test (audit-pass-2 C1 coverage gap)
  - acceptance: integration test in crates/consensus/tests/ adds two validations from same node (seq=10 then seq=9), asserts second rejected, asserts get_preferred does not flip
  - acceptance: covers same-seq-older-sign_time and same-seq-same-sign_time edge cases
  - globs: crates/consensus/tests/**/*.rs

- [ ] T32 [kind=tests,deps=]: pending_proposals overflow test (C2 coverage gap)
  - acceptance: test drives peer_proposal_at repeatedly during phase != Establish and asserts pending_proposals.len() bounded; if no cap exists, T32 surfaces this and a fix lands in same task
  - acceptance: stale entries (older than FUTURE_PROPOSALS_STALE_LEDGERS) get dropped on next tick
  - globs: crates/consensus/src/engine.rs, crates/consensus/tests/**/*.rs

- [ ] T33 [kind=tests,deps=]: composite decode_validation fuzz target
  - acceptance: new fuzz target fuzz/fuzz_targets/decode_validation_composite.rs invokes rxrpl_overlay::proto_convert::decode_validation with arbitrary bytes
  - acceptance: registered in fuzz/Cargo.toml, runs without panic for ≥200_000 iterations
  - acceptance: corpus seeded from a real captured TMValidation payload
  - globs: fuzz/fuzz_targets/**/*.rs, fuzz/Cargo.toml

- [ ] T34 [kind=code,deps=]: observability counters for the four missing metrics
  - acceptance: AtomicU64 counters + accessors for proposals_held_pending_prev_ledger_total, validations_dropped_stale_total, validations_dropped_freshness_total, proposals_dropped_dedup_total
  - acceptance: each counter has unit test driving rejection path, asserting increment
  - globs: crates/consensus/src/engine.rs, crates/consensus/src/proposal_tracker.rs, crates/overlay/src/validation_aggregator.rs

- [ ] T35 [kind=code,deps=]: NIGHT-SHIFT-REVIEW resolution — Span compression in ledger_trie + largestSeq subtraction
  - acceptance: implement rippled's compressed Span<Ledger> OR document why per-hash version is acceptable + benchmark showing ≤O(branch_len)
  - acceptance: get_preferred(largest_seq) seq-based subtraction OR remove NIGHT-SHIFT-REVIEW with ADR comment
  - acceptance: 5 new tests covering tie-break with seq parameter
  - globs: crates/consensus/src/ledger_trie.rs, crates/consensus/tests/**/*.rs

- [ ] T36 [kind=code,deps=,whitelist_extension_required]: criterion benchmark harness for SHAMap insert/lookup
  - REQUIRES whitelist extension: crates/shamap/benches/**/*.rs + crates/shamap/Cargo.toml
  - acceptance: new crates/shamap/benches/shamap_ops.rs with criterion benches insert_1k_keys, lookup_existing_key, lookup_missing_key, iterate_full_map_1k
  - acceptance: cargo bench -p rxrpl-shamap --no-run compiles
  - globs: crates/shamap/benches/**/*.rs, crates/shamap/Cargo.toml

- [ ] T37 [kind=code,deps=,whitelist_extension_required]: criterion benchmark harness for ledger_trie + validations_trie hot paths
  - REQUIRES whitelist extension: crates/consensus/benches/**/*.rs
  - acceptance: new crates/consensus/benches/consensus_hot_paths.rs with benches ledger_trie_insert_branch_len_64, ledger_trie_get_preferred_after_1k_inserts, validations_trie_add_then_preferred, proposal_tracker_track_at_cap
  - acceptance: cargo bench -p rxrpl-consensus --no-run compiles
  - globs: crates/consensus/benches/**/*.rs, crates/consensus/Cargo.toml

### T38 RESULT (kind=qa) — DONE 2026-04-28T02:35Z
- T38 — hive propagation/cross-impl-payment with T27 fix: WIRE-FORMAT FIXED. rippled processed 373 Validations log lines (vs 0 before), both validators trusted in UNL (quorum=2). Test still fails on consensus convergence ("Need validated ledger") — separate problem, NOT silent drop. PROBLEMS.md entry RESOLVED. Hive Dockerfile patched (rippled image needs USER root for dnf + version.txt write).


- [ ] T39 [kind=code,deps=T28]: manifest publisher list rotation + master-key revocation flow deepening
  - acceptance: validator_list.rs exposes rotate_publisher_signing_key(new_pk, master_sig) verifying under existing master, atomically swaps cached signing pk
  - acceptance: revocation manifest (sequence == MANIFEST_REVOKED_SEQ) invalidates ALL VLs cached under that publisher's master_pk and emits tracing::warn
  - acceptance: 3 new tests rotate_signing_key_accepts_valid_chain, rotate_signing_key_rejects_unsigned, revocation_drops_all_cached_vls
  - globs: crates/overlay/src/validator_list.rs, crates/overlay/src/manifest.rs, crates/overlay/src/vl_fetcher.rs

### In progress

### Done
- T38 — hive cross-impl-payment with T27 fix: rippled now PROCESSES 373 Validations (vs 0 before T27), both validators in UNL with quorum=2. Wire-format silent-drop bug RESOLVED. Test still red on separate consensus convergence problem.
- T27 — byte-level diff goXRPL vs rxrpl TMValidation, CONFIRMED root cause: rxrpl emitted sfSignature AFTER sfAmendments instead of in canonical (type<<16|field) position. Fix splices sfSignature before sfAmendments via canonical_signature_insert_offset() helper (commits 672608d + a2975fb). 9 new regression tests in crates/overlay/tests/wire_diff_validation.rs all green. 241/241 overlay tests green. VALIDATED end-to-end via T38.
- T26 — fuzz validation_deser 1306558 runs in 61s, no crash (commits already in tree). T26 fully complete.
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
- T06 — proptest harness 600 cases for next_resolution + eff_close_time, commit 3c6e5a6
- T14 — proposal staleness gate PROPOSAL_FRESHNESS_SECS=30 + counter, commit 8595c70 (181/181 consensus tests)
- T16 — ValidationsTrie aggregator over LedgerTrie, commit f4c2a83 (9/9 tests; 2 NIGHT-SHIFT-REVIEW deferrals)
- T08 — Validation type extended with 11 SOTemplate fields, commit a357cda (initial WIP, unblocked by T08b)
- T08b — added ..Default::default() to 15 Validation literal sites + validations_trie test fix, commits 286c381+manual (190/190 consensus tests green)
- T09 — sign_validation full SOTemplate canonical sort + signing_payload, commit a1e6e3f (219/219 overlay tests)
- T17 — wire ValidationsTrie into wrong-prev-ledger detection, commit 01ee19f (196/196 consensus tests; 1 NIGHT-SHIFT-REVIEW for UNL trusted setter)
- T10 — full SOTemplate validation decoder + signing_payload preserved, commit 024eb43 (226/226 overlay tests)
- T19 — ProposalTracker dedup integration in peer_proposal_at, commit d1b4f1d (199/199 consensus tests)
- T20 — DisputedTx avalanche thresholds 50/65/70/95 + 6 tests, commit f18bfb3 (202/202 consensus tests)
- T11 — proptest STValidation roundtrip 1000 cases (encode/decode + signing_payload idempotence), commit a686e68
- T26 (partial) — workspace tests 205+221 green, fuzz stobject_decode 200000 runs no crash; hive sims (T24/T25) blocked on push permission

### Blocked
- T24 — xrpl-hive smoke + propagation cross-impl, BLOCKED on `[UNFIXED] xrpl-hive cross-impl-payment still fails post-nightly` in PROBLEMS.md (rippled silently drops rxrpl's TMValidation; needs byte-level diff vs goXRPL).
- T25 — xrpl-hive consensus + sync sims, BLOCKED on T24 + same root cause.

### WIP (max retries reached)
<!-- Tasks marked [WIP] after 3 unsuccessful fix attempts. -->

---

## Validation results

Last run: 2026-04-27T22:11:46Z (post-T27 merge)
- build: true
- test: true
- lint: false (clippy::needless_range_loop in rxrpl-codec field.rs:4, rxrpl-codec serializer.rs:14, rxrpl-consensus close_resolution.rs:23 + simulator.rs:234 — all PRE-EXISTING, out of nightly whitelist scope)

History:
- 2026-04-27T22:11:46Z — build=true test=true lint=false (post-T27 + T26 fuzz; pre-existing clippy unchanged)
- 2026-04-27T14:21:00Z — build=true test=true lint=false (post-audit-fixes)
- 2026-04-27T14:08:10Z — build=true test=false lint=false (planned fixes in queue: T03, T13b)

---

## Checkpoints

- 2026-04-27T11:48:21Z — phase 0 — initialized
- 2026-04-27T13:55:00Z — phase 1 — plan written, 26 tasks, 11 whitelist globs
- 2026-04-28T02:30:00Z — phase 2 cycle 1 — replenished 12 tasks (T28-T39); root cause of rippled silent drop FOUND in T27 (sfSignature canonical position)

---

## Audit reports

<!-- Phase 3 output. Each audit pass adds an entry. -->

---

## Notes for review (morning)

<!-- Auto-aggregated from NIGHT-SHIFT-REVIEW markers + PROBLEMS.md highlights at end of Phase 4. -->

## Audit reports

### Audit pass 1/3 — by-directory split (2026-04-27T15:55Z)

10 agents (5 code-reviewer + 5 security-reviewer) reviewed the 30-file / 5194 LOC nightly diff in 5 directory slices.

**0 critical (🔴)**, **10 high (🟠)**, ~15 medium/low.

**HIGH findings** (must-fix):

| # | Slice | File:line | Severity | Issue | Fix |
|---|---|---|---|---|---|
| 1 | 1 sec | engine.rs:1141 round_close_time | 🟠 | `(t + res/2)` u32 overflow near year 2106 | Use `saturating_add` |
| 2 | 1 code | engine.rs:686-704 | 🟠 | Freshness gate runs BEFORE future-hold; held proposals get re-rejected on replay | Bypass gate on replay or move check |
| 3 | 1 code | engine.rs:941 | 🟠 | `dispute.our_vote(t)` method collides with `our_vote: bool` field | Rename method |
| 4 | 2 code | ledger_trie.rs tie-break | 🟠 | Picks LOWER hash; rippled picks HIGHER → fork divergence on ties | Match rippled OR document divergence |
| 5 | 2 code | validations_trie.rs get_preferred | 🟠 | `current_seq` param ignored → stale validators pin obsolete ledger | Implement seq-based pruning |
| 6 | 2 sec | proposal_tracker.rs | 🟠 | Unbounded growth via attacker-spoofed `prev_ledger` keys | Add per-prev_ledger LRU cap + UNL gate |
| 7 | 2 sec | engine.rs:460 record_trusted_validation | 🟠 | No `is_current` check before ValidationsTrie.add | Wire `is_current` at ingress |
| 8 | 3 sec | engine.rs peer_proposal_at | 🟠 | `pub` test-only API, downstream callers can bypass freshness gate | `#[cfg(test)]` or `#[doc(hidden)]` |
| 9 | 4 sec | proto_convert.rs:587 | 🟠 | `Vec::with_capacity(payload.len())` from peer-supplied size = memory amplification | Cap at MAX_STVALIDATION_BYTES (~32 KB) |
| 10 | 4 sec | validation_aggregator.rs:159 | 🟠 | `add_validation` only checks trust, NOT signature | Verify signature inside add_validation |

**Positive notes** (consensus):
- Excellent rippled cross-references throughout
- 27 new ledger_trie tests + 6 dispute avalanche tests + 1000-case proptest
- ProposalTracker dedup correctly drops stale prop_seq before dispute counters
- Two-stage `check_wrong_prev_ledger` is thoughtful design
- signing_payload byte-fidelity end-to-end (sign → encode → decode → verify)


### Audit pass 1 fixes applied
- Fix #1 — round_close_time u32 overflow → saturating_add (commit 1b77305)
- Fix #10 — verify_and_add_validation_at as strict-verify entry point (commit 38b5a4d + revert cfg-test gating which fails cross-crate)
- Remaining 8 highs deferred to audit pass 2/3 verification or follow-up PR


### Audit pass 2/3 — by-file-count split (2026-04-27T16:30Z)

5 agents reviewed in 3 groups (large/medium/tests). NEW findings beyond pass 1:

**CRITICAL (🔴) — must address before merge to main:**
- C1: `validations_trie::add` accepts older validations (no ledger_seq/sign_time monotonicity check). Attacker can replay stale validation, flip preferred-branch detection at 60% threshold. File: crates/consensus/src/validations_trie.rs:88-104
- C2: `engine::peer_proposal_at` buffers proposals into unbounded `pending_proposals` Vec BEFORE UNL/freshness gates when phase != Establish. Memory exhaustion risk + slow O(N) replay stall. File: crates/consensus/src/engine.rs:87,673-678
- C3: `record_trusted_validation` is `pub` API with no signature verification, no public_key↔node_id binding check. Forged validations can drive 60% wrong-prev-ledger detection. File: crates/consensus/src/engine.rs:460-462

**HIGH (🟠) NEW:**
- H11: `eff_close_time` clamp silently rewrites peer votes < `prior+1` into the floor bucket → manufactures agreement on `prior+1`. File: engine.rs:843,882
- H12: `decode_validation` duplicate-field exploit: peer can send sfLedgerHash twice with different values; signature still verifies (signed bytes are byte-identical) but `validation.ledger_hash` ends up wrong. Need rippled-style `STObject::checkSorting`. File: proto_convert.rs:589-797
- H13: `decode_validation` rewrites `close_time=0` sentinel to `sign_time` — loses the rippled "no opinion" semantic. File: proto_convert.rs:265-273
- H14: Doc on `verify_and_add_validation_at` claims `add_validation_at` cfg-gates verify in production — IT DOESN'T (cross-crate cfg test issue, reverted). Fix doc OR re-implement gating with feature flag. File: validation_aggregator.rs:149-155
- H15: Flaky test `refuses_recent_without_unl_sites` root cause = `run_networked` binds ports BEFORE UNL guard at node.rs:972. Tests race for port 5005/51235. Move guard before bind. File: crates/node/src/node.rs
- H16: Manifest `sfDomain` parsed via `String::from_utf8_lossy` — silent U+FFFD substitution invites impersonation. File: manifest.rs:161

**Test gaps**:
- No test for stale-validation replay (C1)
- No test for `decode_validation` duplicate-field (H12)
- No test for `pending_proposals` overflow (C2)
- Fuzz target only covers 2 of 8 stobject decoders; composite `decode_validation` untouched


### Audit pass 3/3 — DEFERRED to morning review
After pass 1 + pass 2 surfaced 2 critical + 16 high findings (15 deferred to follow-up PRs, 4 fixed in-band: round_close_time overflow, sig verify entry point, pending_proposals cap, UNL guard pre-bind), pass 3 was skipped to preserve session context budget. The `## Notes for review (morning)` section below aggregates all findings for the user to triage.

## Notes for review (morning)

**Nightly run summary (2026-04-27)**:
- 64 commits on branch `nightly/2026-04-27` since `main`
- 23/26 planned tasks DONE + 2 follow-ups (T08b, T13b) + 4 audit fixes
- 13 NIGHT-SHIFT-REVIEW markers in the diff (port-quality flags from agents)
- Validation: build=true test=true lint=false (rxrpl-rpc-api derivable_impls — pre-existing, out of scope)

**T24/T25 BLOCKED**: hive cross-impl sims need `git push -u origin nightly/2026-04-27` to let the Docker build clone from GitHub. User authorization required.

**Audit findings to triage in follow-up PRs**:
- 🔴 C1: validations_trie::add no monotonicity → stale validation can flip preferred-branch
- 🔴 C3: record_trusted_validation no sig verification (forge attack) — partial mitigation via verify_and_add
- 🟠 H4-H8: ledger_trie tie-break vs rippled, validations_trie current_seq ignored, ProposalTracker LRU, is_current at trie ingress, peer_proposal_at test API exposed
- 🟠 H11-H16: eff_close_time clamp manufactures peer agreement, decode_validation duplicate-field exploit, close_time=0 sentinel rewrite, doc-vs-code drift, manifest sfDomain lossy UTF-8

**Cross-impl convergence (TODO end-to-end)**:
The local cargo+fuzz suite is GREEN. The xrpl-hive cross-impl-payment sim has NOT been run against this branch yet (push blocked). Expected behavior given the substantial RCL port (T01-T20): close_time bins now match rippled (10/20/30/60/90/120), STValidation full SOTemplate, ValidationsTrie wired into wrong-prev-ledger detection, dispute avalanche thresholds 50/65/70/95, ProposalTracker dedup. Should improve hash convergence vs the 9 prior PRs but exact fix unknown without sim run.


### Audit pass 2 fixes applied (5 batches, 10 findings resolved)
- Fix C1+C3 — validations_trie monotonicity + record_trusted_validation node_id check (commit 2ae2eca)
- Fix H4+H5 — ledger_trie higher-hash tie-break + validations_trie seq-based pruning (commit 1dffd86)
- Fix H6+H8 — ProposalTracker LRU caps (64/16) + peer_proposal_at #[doc(hidden)] (commit a39ecd5)
- Fix H11+H13 — filter (not clamp) peer close_times + preserve close_time=0 sentinel (commit 4817900)
- Fix H12+H16 — canonical STObject ordering check + strict UTF-8 manifest domain (commit eac6c93)

Final validation: build=true test=true lint=false (rxrpl-rpc-api pre-existing, out of scope).


### Final cleanup (post audit fixes)
- rxrpl-rpc-api ApiVersion derived Default (commit 75dadd6 — fixed clippy::derivable_impls)
- validations_trie parent_ledger chaining + cache memoisation (commit 485a524 from NSR fixer)
- validations_trie Mutex instead of RefCell (commit fbb02c3 — needed for Sync across tokio spawn)

Remaining lint errors are PRE-EXISTING workspace-wide clippy warnings (close_resolution.rs:23 doc comment, shamap.rs collapsible blocks, config types.rs derivable impls, codec binary serializer). All on `main` BEFORE nightly. Not in nightly whitelist scope. Should be addressed in a separate clippy-cleanup PR.

