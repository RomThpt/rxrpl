# rxrpl — Gaps pour parité xrpl-hive complète

Document d'état mis à jour à chaque commit notable. Scores contre [XRPL-Commons/xrpl-hive](https://github.com/XRPL-Commons/xrpl-hive).

## Score actuel (2026-04-25)

| Simulator | Passés | Total | Taux |
|---|---|---|---|
| smoke | 3 | 3 | **100%** ✅ |
| wscompat | 5 | 5 | **100%** ✅ |
| rpccompat | 97 | 97 | **100%** ✅ |
| txcompat | 22 | 22 | **100%** ✅ |
| sync | 0 | 1 | en cours d'investigation |
| propagation | — | 1 | bloqué externe |
| **Total actionnable** | **127** | **128** | **99.2%** ✅ |

## Branche actuelle

`feature/sync-late-joiner` (le PR #13 récapitulatif a été mergé sur main : `bce92fe`).

## Historique des commits par session

### Session 2026-04-25 (sync — branche `feature/sync-late-joiner`)
- `1e58fec` — `parse_node_seed` accepte hex (32 chars) **ou** base58 family seed (`snXxx...`). xrpl-hive passe les seeds validateurs en base58 via `XRPL_VALIDATOR_SEED` ; rxrpl crashait à l'init du late joiner avec « invalid node_seed hex: Odd number of digits ».
- `8c15b38` — `--sync-rpc` truly optional dans `cmd_network_run`. Le default `--url=https://s1.ripple.com:51234` était utilisé en fallback même en mode network ; le late joiner bootstrappait sur mainnet et son open ledger devenait `103802001` au lieu du local `~16`. Désormais sans `--sync-rpc` explicite, on skippe le bootstrap externe et on découvre via P2P.
- (xrpl-hive `clients/rxrpl/xrpl_start.sh`) — bascule en `MODE=network` quand `XRPL_VALIDATOR_SEED` est défini (pas seulement `XRPL_BOOTNODE`). Sans ça, le node 0 (seed-anchor) tournait en standalone, sans P2P listener, donc inaccessible aux peers et au late joiner.

### Sessions précédentes (mergées dans main via PR #13 — `bce92fe`)
- `7aab836` — Cleanup tracing diagnostique + `XRPL_LOGLEVEL` configurable + adapt clawback_lifecycle test.
- `846d56a` — Enforce `lsfAllowTrustLineClawback` (0x80000000) dans Clawback preclaim ; `asfAllowTrustLineClawback` (16) au mapping AccountSet. +1 txcompat (clawback_iou + clawback_without_flag stable).
- `403198c` — `trust_set` link au owner_dir + invariant `ValidClawback` perspective-aware. +1 txcompat (payment_iou).
- `943ea02` — Support IOU pour Payment + AMM (issuer-mint, single-asset). +3 txcompat.
- `6fcaf8a` — Owner-dir linking pour Oracle/DID/SignerList + scan d'état pour `nft_buy_offers`/`nft_sell_offers`. +4 txcompat.
- `510b46a` — Émission de `TransactionValidated` events depuis `ledger_accept`. Restaure `ws_subscribe_transactions`.
- `164dc7e` — Purge des tx confirmées de la queue retry sur `ledger_accept`. Bug racine de `nft_mint_and_burn`/`check_create_and_cancel`/`offer_cancel`.
- `8cc3b6d` — `account_objects.index` field + `Escrow.Sequence`.
- `f98902d` — Owner directory linking + payment reserve check. +3 txcompat.
- `b591851` — Shapes de réponse + admin gating. +17 rpccompat + 1 wscompat.
- `f0f4a85` — Tokens d'erreur rippled. +23 rpccompat.

**Total cumulé sessions 2026-04-24/25 : +56 tests sur 128 actionnables (55% → 99%).**

---

## Investigation sync en cours

### Progression observée (run par run)
1. **Run #1** : crash « invalid node_seed hex » → fix `1e58fec`.
2. **Run #2** : « timed out waiting for container startup » — late joiner bootstrap sur s1.ripple.com → fix `8c15b38`.
3. **Run #3** : late joiner reach ledger 10 ✓ mais « late-join node doesn't have the account ». Cause : node 0 en mode standalone (pas de P2P listener), donc le late joiner ne se connecte à personne et avance ses propres ledgers locaux → fix xrpl_start.sh (network mode si VALIDATOR_SEED).
4. **Run #4** (en cours) : si tous les nodes sont en network mode, le late joiner devrait pouvoir se connecter et sync.

### Run #4 (2026-04-25 16:16) — late joiner stalle au sync
- ✅ Late joiner démarre, écoute P2P, se connecte au peer fixe (192.168.97.2:51235)
- ✅ Détecte « peer ahead by 14 ledgers, entering sync mode (target #16) »
- ❌ « sync stalled at #2 for 30s, re-requesting target #X » — boucle indéfiniment, jamais ne dépasse #2

Diagnostic : `node.rs:1188-1193` envoie `OverlayCommand::RequestLedger {seq: peer_seq, hash: peer_hash}`. La réponse `LedgerData` n'arrive pas, ou arrive vide, ou échoue à `try_reconstruct_ledger`. Probables causes :
1. `handle_get_ledger` (`peer_manager.rs:2031`) sert depuis `ledger_provider.latest_closed()` — mais le state nodes envoyés peuvent être insuffisants pour reconstruire l'arbre côté late joiner.
2. La reconstruction côté late joiner (`Node::try_reconstruct_ledger`) requiert tous les nœuds SHAMap accessibles ; la première requête ne précise pas `node_ids` donc serve juste le root + quelques enfants ; le late joiner doit ensuite faire des sous-requêtes (incremental sync via `LedgerSyncer.feed_nodes`) — c'est probablement là que ça coince.
3. Les peers se déconnectent après ~3 minutes (vu dans les logs) — la reconnect logic recherche peut-être trop tôt.

### Prochaines étapes
1. Activer trace logs P2P (debug niveau) pour voir GetLedger/LedgerData en clair
2. Vérifier `try_reconstruct_ledger` : retourne-t-il `Ok` ou `Err` ?
3. Vérifier que `LedgerSyncer.feed_nodes` est invoqué quand des nodes arrivent
4. Implémenter ou compléter le flow incremental → quand reconstruction échoue, demander spécifiquement les nodes manquants
5. Tester avec 2 nodes seulement (1 initial + 1 late joiner) pour isoler les variables

---

## Bloqué externe

`propagation/cross-impl-payment` — Dockerfile rippled (clients/rippled/Dockerfile) a un bug `rippled --version` non-zero sur arm64. À reporter à XRPL-Commons.

---

## Reproduction

```bash
# Branche RomThpt/rxrpl@feature/sync-late-joiner pour le travail courant
# (main a tout le reste depuis le merge bce92fe)
cd /Users/romt/Developer/xrpl-hive
# Dockerfile pointe sur la branche en cours ; xrpl_start.sh OK
./bin/xrpl-hive --sim smoke --client rxrpl --docker.nocache rxrpl
./bin/xrpl-hive --sim rpccompat --client rxrpl
./bin/xrpl-hive --sim txcompat --client rxrpl
./bin/xrpl-hive --sim wscompat --client rxrpl
./bin/xrpl-hive --sim sync --client rxrpl
```

Logs : `/Users/romt/Developer/xrpl-hive/workspace/logs/*.json` + `details/*.log`.
