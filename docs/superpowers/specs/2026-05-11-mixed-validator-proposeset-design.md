# Mixed-validator ProposeSet — Design

Tracks issue [#76](https://github.com/RomThpt/rxrpl/issues/76).

## Goal

rxrpl must emit `ProposeSet` every round in a mixed-validator topology (rippled + rxrpl), without regressing the rxrpl-only sync simulation. Empirical state: rxrpl currently runs as passive validator (Validation only, no ProposeSet).

## Root cause recap

Two deferrals in `crates/node/src/node.rs:1416-1446` block `consensus.close_ledger()` whenever a peer is at-or-past our seq (`peer_at_or_past`) or slightly behind but alive (`peer_behind_alive`). With rippled present, one of these is always true, so rxrpl never enters Establish phase and never proposes.

Naively removing the deferrals (attempted six times, reverted in 35bab39) breaks the rxrpl-only sync sim because the `account_hash` produced locally diverges from rippled at flag ledgers — rxrpl's `default_vote` settings differ from rippled-2.6.2's amendment table.

## Architecture

Three independent PRs, sequenced.

### PR1 — Amendment vote configuration

Make amendment votes/vetos configurable via TOML, with a `compatibility = "rippled-2.6.2"` preset that locks the vote table to exactly what rippled-2.6.2 ships.

New modules:

- `crates/amendment/src/config.rs` — `AmendmentConfig` struct + `apply(&self, registry: &mut FeatureRegistry, table: &mut AmendmentTable)`
- `crates/amendment/src/presets/rippled_2_6_2.rs` — captured list of `(name, vote)` for rippled-2.6.2

TOML shape:

```toml
[amendments]
compatibility = "rippled-2.6.2"   # OR manual:
vote = ["AMM", "DID"]
veto = ["DeepFreeze"]
```

Integration:

- `crates/node/src/config.rs` adds `amendments: Option<AmendmentConfig>`
- `crates/node/src/node.rs:146-147` and `:204-205` call `apply()` after `with_known_amendments()`

Errors are hard-fail at boot: unknown amendment name, `compatibility` + manual override both set, preset file missing.

### PR2 — Always-active proposer

Remove the two deferrals in `node.rs:1416-1446`:

- Drop `peer_at_or_past` branch
- Drop `peer_behind_alive` branch
- Keep `first_close_grace` (bootstrap-only)
- Keep `latest_peer_close_time` bucket adoption

Add `rxrpl_consensus_local_closes_total` counter (consumed by PR3).

PR2 depends on PR1: without amendment alignment, flag ledgers fork → `wrong_prev_ledger_detected` → sawtooth `complete_ledgers`.

### PR3 — Observability + decoder cleanup

- `crates/overlay/src/proto_convert.rs:49-71`: change `decode_propose_set` signature to derive `ledger_seq` from caller context instead of hardcoding 0
- `crates/rpc-server/src/handlers/consensus_info.rs`: expose `peer_proposals_seen`, `local_closes_total`
- Prometheus: `rxrpl_consensus_local_closes_total`, `_peer_proposals_received_total`, `_pending_proposals_dropped_total`, `_phase` gauge
- `docs/operations/mixed-validator.md` — how to configure + verify

## Data flow

```
boot                                       every ~4s round
 │                                          │
 └─ AmendmentConfig::apply        [PR1]     ├─ TimerAction::CloseLedger
                                            ├─ first_close_grace check
                                            ├─ adopt peer close_time bucket
                                            ├─ consensus.start_round
                                            ├─ consensus.close_ledger    [PR2 unblocks]
                                            ├─ overlay broadcasts TMProposeSet
                                            ├─ decode peer TMProposeSet   [PR3 fixes seq=0]
                                            └─ converge → if accepted:
                                                 ├─ flag_ledger:
                                                 │   ├─ apply_amendment_voting  [PR1: matches rippled]
                                                 │   └─ apply_negative_unl
                                                 ├─ l.close → account_hash matches rippled
                                                 └─ broadcast Validation
```

## Error handling

| Scenario | Behavior | Source |
|---|---|---|
| Unknown amendment name in TOML | hard fail at boot | PR1 |
| `compatibility` + `vote/veto` both set | hard fail at boot | PR1 |
| Preset missing | hard fail at boot | PR1 |
| `wrong_prev_ledger_detected` | existing recovery path | unchanged |
| `pending_proposals cap reached` | logged + surfaced as counter | PR3 |
| `decode_propose_set` unknown prev_ledger | seq=0, warn once | PR3 |
| Peer disconnect mid-round | existing handling | unchanged |
| Close-time race (rxrpl ahead of rippled) | bucket adoption via `latest_peer_close_time` | unchanged |

## Risks

1. **PR1 misses a flag-ledger pseudo-tx besides EnableAmendment** (e.g. UNLModify ordering). Mitigation: integration test diffing `account_hash` ledger-by-ledger against rippled-2.6.2 across ≥ 512 ledgers.
2. **PR2 still breaks sync sim with PR1 in place** → divergence is elsewhere (close_time bucket, tx ordering). Plan B: per-ledger line diff vs rippled.
3. **rippled-2.6.2 obsolescence** → new preset file when supporting newer rippled. Backwards-compatible via named presets.

## Acceptance criteria

- Issue #76 criteria:
  - `consensus_info.previous_proposers >= 1` on rippled-0 after 20 ledgers
  - `validated_seq` advances continuously
  - No `lgrNotFound` for ledgers in `complete_ledgers`
  - No `wrong_prev_ledger detected` in steady-state
- Kurtosis suites green in `2 rippled + 2 rxrpl` (consensus, propagation, soak, sync)
- Kurtosis suites green in rxrpl-only (regression gate)
- `cargo test --workspace` green
- `cargo clippy --workspace -- -D warnings` green

## Out of scope

- Dynamic RPC `feature` bootstrap
- Standalone-mode removal
- Consensus engine refactor
- Runtime multi-amendment-table

## Merge order and rollback

1. PR1 merged. If nothing breaks, continue.
2. PR2 merged. If sync sim breaks → revert PR2, PR1 remains useful (still aligns amendment votes for any future use).
3. PR3 merged. Cosmetic, low risk.
