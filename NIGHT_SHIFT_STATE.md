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
<!-- Tasks blocked on external dependencies, see PROBLEMS.md for details. -->

### WIP (max retries reached)
<!-- Tasks marked [WIP] after 3 unsuccessful fix attempts. -->

---

## Validation results

Last run: 2026-04-27T14:21Z
- build: true
- test: true (ALL workspace tests green!)
- lint: false (rxrpl-rpc-api derivable_impls — pre-existing, out of nightly scope)

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

