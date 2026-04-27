# NightShift Enriched Spec — rxrpl — 2026-04-27

> The original user spec, enriched with answers to the Phase 0 questionnaire and findings from Phase 1 research. This is the SOURCE OF TRUTH for what the night-* agents must build. Anything not in this document is out of scope.

---

## Goal (one sentence)

Faire de rxrpl un validator XRPL complet en Rust capable de se sync au mainnet (run par rippled), en s'inspirant de LeJamon/go-xrpl pour la structure et de rippled pour la précision binaire/hash, en validant via xrpl-confluence et xrpl-hive.

## Out of scope

- Réécriture des modules SHAMap/crypto/storage déjà fonctionnels (PRs #29-38 sont la base)
- Mode SetHook WASM (`crates/hooks/`) — hors scope core validator
- Ajout de nouvelles RPC (sauf `submit`/`server_info` si bloquant pour les sims)
- Compatibilité forks non-mainnet (XAH, etc.)

## Phase 0 — Q/A

### Scope boundaries
- Q: Quels modules doivent être travaillés cette nuit ?
  - A: **Tous les 4** : Consensus engine (RCL port), Catchup robustesse, Validation pipeline, Wire compat finale

### Technical decisions
- Q: Stratégie de référence impl pour le port ?
  - A: **Hybride** : goXRPL pour structure (`internal/consensus/rcl/*`, `internal/consensus/ledger_timing.go`), rippled C++ pour précision (formats binaires, hash prefixes, STObject)

### Error handling
- Q: Comment les erreurs/divergences consensus doivent surfacer ?
  - A: **Logs structurés (tracing) + métriques** — `tracing::warn!` + counters, jamais `panic!`, jamais crash. Validator long-running.

### Tests
- Q: Quelle stratégie de tests pour les nouveaux ports ?
  - A: **Quatre stratégies cumulées** :
    - Unit tests par fonction portée (parité avec tests goXRPL/rippled correspondants)
    - Property tests sur encodings via `proptest` (round-trip STObject, SHAMap, manifests)
    - Integration cross-impl via xrpl-hive (sims en CI nightly)
    - Fuzz harness via `cargo-fuzz` (wire codec, STObject parser)

### UX
- Q: skip — backend-only

### Priorities
- Q: Critère de succès end-to-end pour la nuit ?
  - A: **Tous les tests xrpl-hive passent** (smoke + propagation + consensus + sync + rpccompat) — bar haute mais c'est l'objectif. Stratégie PR : build dessus des 10 PRs déjà mergées, ne rien casser.

### Explicit exclusions
- Q: Quels paths/modules doivent être EXCLUS ?
  - A: **Aucune exclusion** — tout le repo est fair game si justifié. Scope ambitieux assumé.

## Phase 1 — Research findings

### Existing code structure

- 25 crates in workspace under `crates/`
- Main consensus: `crates/consensus/src/{engine.rs, close_resolution.rs, timer.rs, phase.rs, unl.rs, negative_unl.rs}`
- Wire/overlay: `crates/overlay/src/{peer_manager.rs, ledger_sync.rs, proto_convert.rs, identity.rs, stobject.rs, manifest.rs, validation_aggregator.rs}`
- SHAMap: `crates/shamap/src/{shamap.rs, leaf_node.rs, inner_node.rs}` — already rippled-compat (PRs #29-31)
- Node orchestration: `crates/node/src/node.rs` (123 KB, central coordinator)
- Ledger format: `crates/ledger/src/{header.rs, ledger.rs, skip_list.rs}`
- Build: `cargo` workspace, Rust 1.85+, edition 2024
- Tests: `cargo test`, no proptest/criterion/cargo-fuzz currently in workspace but `fuzz/` dir exists

### Key gaps identified by architect

1. **AdaptiveCloseTime bins wrong**: uses 1/2/4/8/16/30 (binary halve), should be 10/20/30/60/90/120 like rippled
2. **No effCloseTime clamp** on prior_close_time + 1s — current `round_close_time` rounds in isolation
3. **STValidation incomplete**: only 5 fields encoded (Flags, LedgerSeq, SigningTime, LedgerHash, SigningPubKey), missing 14 SOTemplate fields (sfBaseFee, sfReserveBase, sfCookie, sfConsensusHash, sfAmendments, sfBaseFeeDrops, etc.)
4. **No LedgerTrie**: validation_aggregator is flat HashMap, can't compute preferred branch
5. **No staleness window**: validationCURRENT_WALL/LOCAL/EARLY not enforced
6. **Manifest signing missing**: only inbound parsing, no outbound creation/relay
7. **ProposalTracker missing**: peer positions stored in plain HashMap, no prop_seq monotonicity check

### Risks identified during planning

| Risk | Mitigation |
|---|---|
| STValidation field ordering bug breaks signature parity | T09 hardcoded fixture vs rippled hex |
| LedgerTrie naive port = O(n²) memory on long forks | T23 depth>20 test catches it |
| eff_close_time monotonicity breaks simulator (synthetic close_times) | T05 only clamps when prior != 0 |
| xrpl-hive Docker rebuild eats 8h budget (12 min/build) | Single nocache build per night |
| Manifest relay loops without peer_id origin filter | T21 includes 2-peer integration test |

## Whitelist of editable files

```yaml
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
```

## Original user spec

> le but est de faire un validateur en rust comme rippled et https://github.com/LeJamon/go-xrpl, utilise https://github.com/XRPL-Commons/xrpl-confluence et https://github.com/XRPL-Commons/xrpl-hive pour comparer et avoir les differents states etc
>
> le but est d'avoir un truc qui peut se sync au mainnet qui est run par des rippled

## Reference repos

- **goXRPL** (LeJamon/go-xrpl) : structure de référence pour le port. Notamment `internal/consensus/rcl/{engine.go, proposals.go, validations.go}`, `internal/consensus/ledger_timing.go`, `internal/consensus/adaptor/*`. ~57 fichiers consensus.
- **rippled** (~/Developer/rippled) : source de vérité C++ pour formats binaires, hashs, prefixes. Notamment `src/libxrpl/protocol/STValidation.cpp`, `src/libxrpl/shamap/*`, `src/xrpld/consensus/*`, `src/xrpld/overlay/detail/PeerImp.cpp`.
- **xrpl-hive** (~/Developer/xrpl-hive) : test harness Docker-compose pour simulations cross-impl (rxrpl + rippled).
- **xrpl-confluence** : alternative test harness Go. À évaluer en complément de hive.

## Current state (post 10 PRs merged)

- ✅ SHAMap wire format rippled-compat (PRs #29-31)
- ✅ Holding pen consensus + ledger_seq=0 fix (PR #32)
- ✅ Timestamps NetClock conversion (PR #33)
- ✅ Catchup header reconstruction depuis liBASE (PR #34, #36)
- ✅ Close_time rounding adaptive (PR #35, #37)
- ✅ Vote-counting + realignment (PR #38)
- ❌ **Cross-impl-payment échoue toujours** : hashes locaux divergent encore. Manque port complet RCL.
