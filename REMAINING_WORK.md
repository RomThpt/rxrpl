# rxrpl — Travail restant

État au 2026-06-13. Le gros œuvre est terminé : rxrpl converge, propage,
sync et co-valide face à un vrai rippled, et tolère la perte d'un
validateur. Ce qui reste relève de fonctionnalités secondaires et du
durcissement, pas de bugs bloquants.

## État vérifié

### Vérifié le 2026-06-13

- **CI verte** (sur PR #121, avant merge) : `Test` (cargo test workspace),
  `Build Release`, `Clippy`, `Format`, `Fuzz`, `Docker Build` — tous pass.
- **Interop rxrpl ↔ rippled** (`interop/`, réseau mixte 3 rippled + 2 rxrpl,
  rippled 3.1.3) :
  - 16 tests fonctionnels (configs, consensus, mixed-voter, propagation,
    sync) : **PASS** (mode test-runner officiel).
  - 4 tests de chaos (crash/recover, crash/rejoin, observe-quorum, no-panic) :
    **PASS** (host-mode isolé ; skippés par défaut dans le harness car le
    socket Docker n'est pas monté).
  - Tolérance à 1 panne validée (quorum 80 % = 4 sur 5 validateurs).

### Résolu lors des sessions précédentes (non re-vérifié aujourd'hui)

- **Suite hive complète 9/9 verte** au 2026-06-08 (smoke, rpccompat,
  rpccompat-stateful, txcompat, wscompat, consensus, propagation, sync,
  soak). Les causes racines historiques — tx-set de consensus, cadence de
  consensus / boucle de catchup cross-impl, sync late-join, soak — ont
  toutes été corrigées (PRs rxrpl #110-#117 ; correctifs harness xrpl-hive).
  Diagnostic field-by-field : rxrpl confirmé sain, les échecs résiduels
  étaient dans le harness de test.
- **Paiements cross-currency multi-hop** : Phases 1-4 livrées (PRs #104-#109).

## À faire

### Fonctionnalités secondaires (trous précis, non bloquants)

Tous présents sous forme de `TODO`/`not yet wired` dans le code de prod :

1. **`validator_token` / `validator_token_path`** (`crates/node/src/node.rs`,
   `build_validator_identity`). Seule la forme explicite `master_secret` +
   `ephemeral_seed` est câblée. Le parser existe déjà
   (`crates/config/src/validator_token.rs`) mais n'est pas branché : il faut
   permettre à `ValidatorIdentity` de porter un master *pubkey-only* (le
   master secret ne vit jamais sur le validateur en mode token), parser le
   manifest du token pour en extraire master-pubkey + sequence, et diffuser
   ce manifest pré-signé au lieu d'en régénérer un. Chantier dédié (touche le
   cœur de l'identité validateur).
2. **Bootstrap checkpoint par hash** (`node.rs`) : le lookup d'un header par
   hash n'est pas câblé ; un node démarre depuis genesis.
3. **Indexation de l'historique des transactions NFT**
   (`crates/rpc-server/src/handlers/nft_history.rs`).
4. **Initiation de connexion P2P via RPC**
   (`crates/rpc-server/src/handlers/connect.rs`).
5. **Chemin `TicketSequence` à travers le moteur** (`crates/tx-engine/src/engine.rs`).

### Routage AMM dans les paiements

Le multi-hop cross-currency est livré ; le routage via AMM (Automated Market
Maker) reste à brancher (`crates/pathfind/` strands → `payment.rs`).

### Durcissement production

- Fuzzing en cours (corpus sous `fuzz/corpus/`).
- Refactoring continu (extraction de tests, typage — PRs #118-#120).
- Comportement sous partition réseau prolongée et montée en charge des peers.

## Hors repo rxrpl

Correctifs harness `xrpl-hive` (repo séparé `~/Developer/xrpl-hive`) :
sync sim, soak, consensus/propagation timeouts, peer_private — appliqués lors
des sessions de juin. Le snapshot `clients/rxrpl/src/` doit être resynchronisé
sur `main` avant un run d'interop (il était figé au 22 mai).
