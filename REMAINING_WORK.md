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

## Livré le 2026-06-13

Les trous secondaires « non bloquants » ont été comblés :

1. **`validator_token` / `validator_token_path`** — chargé via le parser
   existant ; `ValidatorIdentity` porte désormais un master *pubkey-only* +
   un manifest pré-signé relayé tel quel (PR #123).
2. **Indexation de l'historique des transactions NFT** — index
   `nft_transactions` (sqlite + postgres), alimenté au close en collectant
   les `NFTokenID` du tx + meta ; `nft_history` lit l'index (PR #126).
3. **Initiation de connexion P2P via RPC** — `connect` câblé à
   `OverlayCommand::ConnectTo` via le canal de commande overlay (PR #125).
4. **Chemin `TicketSequence` à travers le moteur** — validation centrale
   (`checkSeqProxy` : `tefNO_TICKET`/`terPRE_TICKET`, PR #127) + consommation
   du Ticket SLE dans OfferCreate/TrustSet (PR #128) et tous les chemins de
   Payment (PR #129).
5. **Durcissement RPC** — timeout de 30 s sur le dispatch HTTP (PR #124).

**Routage AMM dans les paiements** : déjà implémenté et testé (PRs multi-hop
#104-#109). `quote_amm_swap` + `AmmConsume` dans `payment.rs` couvrent les
strands AMM-seul, book+AMM combinés, et l'échec `tecPATH_PARTIAL` quand ni
book ni AMM ne satisfont la cible (voir les tests `apply_cross_currency_*`).

## À faire

### Différé par conception

- **Bootstrap checkpoint par hash** (`node.rs`, branche `StartingLedger::Hash`) :
  résoudre un hash arbitraire → seq exige une requête P2P header-by-hash
  asynchrone greffée dans la boucle de consensus/catchup — code critique,
  risque élevé pour une faible valeur (`--starting-ledger=<seq>` et `recent`
  couvrent le bootstrap). Laissé non implémenté volontairement.

### Améliorations mineures connues

- **Scaling partiel des strands AMM** (`payment.rs`) : une strand touchant
  l'AMM refuse le scaling partiel (`tecPATH_PARTIAL`) au lieu de re-résoudre
  le swap constant-product sous une cible réduite. Comportement sûr mais
  non optimal.

### Durcissement production

- Fuzzing en cours (corpus sous `fuzz/corpus/`).
- Refactoring continu (extraction de tests, typage — PRs #118-#120).
- Comportement sous partition réseau prolongée et montée en charge des peers.

## Hors repo rxrpl

Correctifs harness `xrpl-hive` (repo séparé `~/Developer/xrpl-hive`) :
sync sim, soak, consensus/propagation timeouts, peer_private — appliqués lors
des sessions de juin. Le snapshot `clients/rxrpl/src/` doit être resynchronisé
sur `main` avant un run d'interop (il était figé au 22 mai).
