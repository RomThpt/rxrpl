# rxrpl — Gaps pour parité xrpl-hive complète

Document d'état mis à jour à chaque commit notable. Scores contre [XRPL-Commons/xrpl-hive](https://github.com/XRPL-Commons/xrpl-hive).

## T27 — byte-level diff goXRPLd vs rxrpl TMValidation (2026-04-28)

**Divergence trouvée** : `crates/overlay/src/proto_convert.rs::encode_validation`
émettait `sfSignature` (type 7, field 6) APRÈS `sfAmendments` (type 19, field 3),
violant l'ordre canonical `(type<<16)|field` croissant que rippled
(`STObject::add` dans `src/libxrpl/protocol/STObject.cpp`) et goXRPLd
(`internal/consensus/adaptor/stvalidation.go::SerializeSTValidation` lignes
~250) appliquent. Sur les flag-ledgers où amendments sont émis, le suppression
hash rxrpl divergeait de celui calculé par les peers, et la validation était
silencieusement classée comme "stray packet" par rippled — explicant les **0
validations reçues** observées dans les sims `propagation/cross-impl-payment`.

Vérifications croisées contre les 7 critères du spec T27 :

| # | Critère | Statut rxrpl |
|---|---|---|
| 1 | `sfFlags = 0x80000001` (vfFullyCanonicalSig\|vfFullValidation) | OK (`identity.rs:125` pour `full=true`) |
| 2 | Ordre canonique `(type, field)` ascendant | **CASSÉ avant T27** sur amendments → corrigé |
| 3 | `sfAmendments` = type 19 (Vector256), field 3, header `[0x03, 0x13]` | OK (`stobject.rs::put_vector256`) |
| 4 | secp256k1 = signature DER (pas R\|\|S brut) | OK (`crypto/src/secp256k1.rs::sign` ligne 139, `der::encode_der_signature`) |
| 5 | `sfSigningPubKey` VL-prefixé + 33 bytes secp256k1 compressé | OK (`stobject.rs::put_vl`) |
| 6 | `HashPrefix::validation = 0x56414C00` (`"VAL\0"`) prepend signing | OK (`identity.rs:117` + verifier ligne 255) |
| 7 | Frame header 6 bytes (4 length+flags BE / 2 type=41 BE) | OK (`p2p-proto/src/codec.rs::PeerCodec`) |

**Fix** : insère `sfSignature` à sa position canonique (avant `sfAmendments`)
via `canonical_signature_insert_offset()`. Régression couverte par
`crates/overlay/tests/wire_diff_validation.rs` (9 tests, 1 par critère).

**Note connexe** (non-bloquante, conservée pour audit ultérieur) : le décodeur
rxrpl exige `(flags & 0x80000001) == 0x80000001` pour marquer `full=true`
(`proto_convert.rs:172`). goXRPLd utilise `(flags & vfFullValidation) != 0`
(`stvalidation.go::parseSTValidation`). En pratique, rippled émet TOUJOURS
`vfFullyCanonicalSig`, donc la divergence est latente — mais une validation
partial signée par un peer non-canonical-strict serait classée full=false par
rxrpl. Documenté ici comme suivi possible si une nouvelle divergence émerge.

## Score actuel (2026-04-25)

| Simulator | Passés | Total | Taux |
|---|---|---|---|
| smoke | 3 | 3 | **100%** ✅ |
| wscompat | 5 | 5 | **100%** ✅ |
| rpccompat | 97 | 97 | **100%** ✅ |
| txcompat | 22 | 22 | **100%** ✅ |
| sync | 0 | 1 | bloqué externe (bug typo xrpl-hive) |
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

### Runs #4-#11 (2026-04-25) — fixes appliqués, late joiner sync OK

Cinq bugs identifiés et corrigés dans le pipeline late-join sync (tous sur `feature/sync-late-joiner`) :

| Commit | Fix |
|---|---|
| `c55dd81` | `handle_get_ledger` ignorait le `seq` demandé pour `liBASE` (`itype=0`), retournait toujours `latest_closed()` |
| `042935f` | `liBASE` doit renvoyer **les 118 bytes du raw header**, pas des leaves de state map |
| `79a869a` | Client envoyait `NodeId.to_wire_bytes()` (33 bytes path+depth) ; serveur fait `store.fetch(hash)` → jamais de match. Fix : envoyer `MissingNode.hash` (32 bytes content hash) |
| `24d20ef` | `feed_nodes` strippait 1 byte de queue en supposant le format rippled (avec depth byte). rxrpl envoie raw → corruption du content hash. |
| `4e43938` | `start_incremental_sync` retourne `0 missing` quand le state target == state local (early ledgers post-genesis). Ajout de `try_complete_sync` qui extrait directement les leaves du store et dispatch `LedgerData` immédiatement. |

**Résultat run #11** : late joiner cascade reconstruct #3 → #4 → #5 → #6 → #7 → #8 → #16, log `catchup complete, resuming consensus at ledger #17`. **Sync pipeline OK.**

### Bug bloquant restant — externe à rxrpl

Le test échoue toujours avec « late-join node doesn't have the account ». Diagnostic via tracing du submit handler (runs #12-14) :

> `Internal("encoding error: invalid checksum")`

L'adresse de destination dans `simulators/sync/main.go:75` — `rPMh7Pi9ct699iZUTWz6CFkakUy5JNb6FG` — a un **checksum base58 invalide** :
- Stored checksum : `9bba9c55`
- Expected checksum : `b5b9ca3d`
- Adresse correcte (1 char diff) : `rPMh7Pi9ct699iZUTWz6CFkakUy5Ju9f9v`

Le seed rxrpl rejette correctement la transaction → aucun payment appliqué → AccountInfo échoue. Bug à corriger dans xrpl-hive (à reporter à XRPL-Commons).

---

## Bloqué externe

1. `sync/late-join-sync` — typo d'adresse dans `simulators/sync/main.go:75` (cf. ci-dessus). 1-char fix nécessaire dans xrpl-hive.
2. `propagation/cross-impl-payment` — Dockerfile rippled (clients/rippled/Dockerfile) a un bug `rippled --version` non-zero sur arm64. À reporter à XRPL-Commons.

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
