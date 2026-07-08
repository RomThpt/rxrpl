# rxrpl — Rust XRPL Validator Node — Roadmap

## Mission

Produire un nœud validateur XRPL complet en Rust, à parité fonctionnelle avec **rippled** (C++, source de vérité protocolaire) et inspiré structurellement de **goXRPLd** (`LeJamon/go-xrpl`). Critère de succès : un nœud rxrpl rejoint un réseau mixte (rippled + rxrpl) et participe au consensus sans divergence — vérifié en continu via les harnais cross-impl **xrpl-hive** et **xrpl-confluence**.

État (2026-05-07) : 24 crates Rust, 460k+ LOC, 1392+ tests. Score interop empirique : **3/4 cas** via xrpl-hive sync (rxrpl→rxrpl, rxrpl→rippled, rippled→rippled OK ; rippled late-join sur réseau rxrpl-only encore bloqué — voir §3.2 et §5). Les cas déjà verts ont été débloqués par PR #71 (GetLedger itype dispatch + GetObjects wireType), PR #73 (TMStatusChange range advertising + otFETCH_PACK server) et PR #74 (PreviousTxnID/PreviousTxnLgrSeq sur AccountRoot) ; le dernier cas reste en cours de fix (`TMLedgerData` not-found, worktree `fix-tmledgerdata`).

## 1. Architecture cible — modules par parité rippled

24 crates sous `crates/`. Statut synthétique :

| Domaine | Crates | Statut |
|---|---|---|
| Primitives & crypto | `primitives`, `crypto`, `codec`, `amount` | rippled-compat |
| Protocole & ledger | `protocol`, `ledger`, `shamap`, `amendment`, `nodestore`, `storage` | wire-format compat (PRs #29-38) |
| Tx & exécution | `tx-engine`, `txq`, `pathfind`, `hooks` | tous tx types majeurs (IOU, NFT, Hooks WASM) |
| Consensus & overlay | `consensus`, `overlay`, `p2p-proto`, `validator-keys` | RPCA + nUNL + manifest gossip |
| Surface & ops | `rpc-api`, `rpc-client`, `rpc-server`, `grpc-server`, `node`, `config` | 97/97 rpccompat, 22/22 txcompat |

**Référence** : `~/Developer/rippled` (vérité binaire/hash) et `LeJamon/go-xrpl` (structure consensus RCL).

## 2. Stratégie de validation cross-impl

- **xrpl-hive** (`~/Developer/xrpl-hive`) — docker-compose, réseau mixte 3 rippled + 2 rxrpl. Suites : `smoke`, `wscompat`, `rpccompat`, `txcompat`, `propagation`, `consensus`, `sync`. Lancé via `bin/xrpl-hive --sim <suite> --client rxrpl`.
- **xrpl-confluence** — harnais Go, scénarios rxrpl-in-UNL avec voters mixtes + flaky nodes. Workflow `interop-e2e-confluence.yml` nightly.
- **CI** : `.github/workflows/ci.yml` (fmt + clippy + tests + 6 fuzz targets), `interop.yml`, `interop-e2e-confluence.yml`.
- **Logs** : `xrpl-hive/workspace/logs/*.json` + `details/*.log`.

Tout PR de feature doit conserver le score interop ≥ courant.

## 3. Travail restant

### 3.1 Initiatives en vol (worktrees actifs)

| Worktree | Initiative | Reste à faire |
|---|---|---|
| `validator-identity` | Two-key validator (master + signing) | B5 rotation runtime + B7 introspection CLI/RPC (PLAN.md feature présent) |
| `domain-attestation` | Validator publie un domaine ; vérifier `xrp-ledger.toml` | **DONE** (B1-B5) — branche prête à merger |
| `vl-v2-cascade` | Validator List v2 multi-blob + cascade trust | **DONE** (B1-B5) — branche prête à merger |
| `fix-tmledgerdata` | Drop empty `TMLedgerData` not-found responses (cause root test 2 sync) | en cours |
| `fix-payment-sequence` | Enforce `tx.Sequence` check (`tefPAST_SEQ` / `terPRE_SEQ`) — corrige double-apply | en cours |
| `nunl-pseudo-tx` | nUNL pseudo-transactions au flag ledger | Mergé sur main (`8a32c7a`) |
| `e2e-unl-confluence` | Harnais confluence rxrpl-in-UNL | Mergé (`3ada983`) |
| `ops-cli-docs` | CLI peers/validators/metrics + runbooks | Mergé (`c1fc64f`) |
| `seed-file-mode` | Seed file 0600 + écriture O_EXCL | Mergé (`9689ff4`) |

### 3.2 Bloqueurs externes

Tous résolus :
- ~~`xrpl-hive/simulators/sync/main.go:75` typo base58~~ — résolu upstream.
- ~~`xrpl-hive/clients/rippled/Dockerfile` arm64~~ — résolu via PR #11 (image swap).

Reste un bug **côté rxrpl** (pas externe) sur sync test 2 : `TMLedgerData` not-found envoie `nodes: vec![]` rejeté par rippled — fix en cours dans worktree `fix-tmledgerdata`.

### 3.3 Mainnet-readiness

- **Sync mainnet réel** : jamais testé contre `s1.ripple.com` / `s2.ripple.com`. Late-join validé localement uniquement.
- **Hardening prod** :
  - Prometheus exporter pas vérifié bout-en-bout (compteurs `tracing` présents).
  - Benchmarks criterion T36/T37 différés (whitelist à étendre).
- **Performance consensus** : pas de profiling formel sur réseau mixte ; LedgerTrie naïf O(n²) théorique sur forks longs — pas de mesure empirique.
- **Audit sécurité** : H11-H16 tous fermés (commits `4817900`, `eac6c93`, `1aee107`, `11e1bfd` sur `nightly/2026-04-27`). Pas de fichier source-of-truth post-`NIGHT_SHIFT_PROBLEMS.md` — créer issues GH si nécessaire pour traçabilité.

## 4. Fichiers critiques de référence

- **État global** : `gaps.md` (canonique pour interop scores + bloqueurs externes).
- **Workspace** : `Cargo.toml` (members), `bin/rxrpl/` (binaire validateur), `interop/` (pytest harness).
- **CI** : `.github/workflows/{ci,interop,interop-e2e-confluence}.yml`.
- **Configs** : `local/rxrpl-{mainnet,testnet,rippled-test}.toml`.
- **Référence externe** :
  - `~/Developer/rippled/src/{libxrpl,xrpld}/` — vérité binaire.
  - `~/Developer/xrpl-hive/` — harnais docker-compose.
  - https://github.com/LeJamon/go-xrpl — structure RCL Go.

## 5. Vérification end-to-end

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo +nightly fuzz run <target> -- -max_total_time=60

cd ~/Developer/xrpl-hive
./bin/xrpl-hive --sim smoke      --client rxrpl --docker.nocache rxrpl
./bin/xrpl-hive --sim rpccompat  --client rxrpl
./bin/xrpl-hive --sim txcompat   --client rxrpl
./bin/xrpl-hive --sim wscompat   --client rxrpl
./bin/xrpl-hive --sim sync       --client rxrpl  # KO test 2 — fix en cours
./bin/xrpl-hive --sim propagation --client rxrpl

gh workflow run interop-e2e-confluence.yml
```

**Critères de succès**
- `cargo test --workspace` vert (≥ 1392 tests).
- Score interop sync xrpl-hive : 4/4 cas (actuellement 3/4 — bloqué sur fix `TMLedgerData`).
- `interop-e2e-confluence.yml` vert nightly.
- Aucun panic ni divergence de hash sur session 24h.

## 6. Conventions

- Une feature = un worktree sous `.worktrees/<name>` avec son propre PLAN.md (gitignored). Branches `feature/<name>` ou `fix/<name>`.
- Commits conventionnels (`feat(scope): …`, batches `X-B1..X-Bn`).
- Aucune mention IA dans commits/PR.
- Tests obligatoires par feature : unit + (si overlay/consensus/codec) round-trip cross-impl ou fixture rippled.
