# rxrpl — Travail restant

État au 2026-05-16, après vérification via les harness `xrpl-hive` et `interop/`.

## Résumé

rxrpl converge, propage et répond au RPC face à rippled. Le cœur consensus
et la couche RPC sont solides. Il reste des trous fonctionnels précis avant
de parler de client « fini ».

| Suite | Score | État |
|---|---|---|
| smoke | 3/3 | ✅ |
| rpccompat | 97/97 | ✅ |
| rpccompat-stateful | 29/29 | ✅ |
| txcompat | 225/225 | ✅ |
| wscompat | 5/5 | ✅ |
| consensus (mixed-validator-hash) | 1/1 | ✅ |
| propagation (cross-impl-payment) | 1/1 | ✅ |
| confluence (`genesis_amendments_disabled`) | OK | ✅ |
| sync (late-join-sync) | 0/4 | ❌ |
| soak (traffic-and-oracle) | 0/1 | ❌ |

## Cause racine commune — tx-set de consensus (priorité haute)

Investigué le 2026-05-17 via `soak --client rxrpl,rippled`. Les items #1,
#2 et #5 ont **une cause racine unique** : rxrpl ne peut ni calculer ni
servir un transaction-set de consensus compatible rippled.

Preuve (log rippled, `Consensus expired` ledger #6) : au premier ledger
contenant des transactions, rxrpl propose `transaction_hash=EBBAE1F3…`,
rippled demande ce tx-set, n'arrive pas à l'acquérir (`7 timeouts for
ledger 6`), abandonne et construit son propre #6 vide → divergence
permanente. Les ledgers vides (#2–#5) convergent (tx-set vide, rien à
servir) ; le premier ledger non vide diverge systématiquement.

Deux défauts couplés, **corrigés le 2026-05-18** :

1. **Hash de tx-set non-rippled.** `TxSet::new` calculait
   `sha512_half(concat des hashes triés)`. Désormais le hash est la
   **racine SHAMap** de l'arbre tx-no-metadata, via la nouvelle fonction
   `rxrpl_shamap::transaction_set_root` (test unitaire : identique à une
   vraie `SHAMap::transaction()` pour 1/2/5/17/64 tx).
2. **tx-set non servi.** `TxSet` porte maintenant les blobs canoniques
   (`from_items`). Le moteur consensus conserve les blobs à travers la
   résolution des disputes (`tx_blobs`, `publish_tx_set`). Le nœud
   construit le set au close en re-sérialisant `tx_json` en binaire
   canonique. `handle_get_tx_set` sert une vraie SHAMap au format wire
   rippled (leaf tx-no-meta = `blob || 0x00`) ; l'acquisition reconstruit
   le set avec blobs.

État après correctif (`soak --client rxrpl,rippled`, 2026-05-18) :
amélioration mesurable mais **soak encore en échec**. La divergence est
passée du ledger #6 au #5 et rxrpl ne stalle plus (il ferme des ledgers
en continu). Le résidu n'est plus le tx-set : c'est la **boucle de
catchup / lag de validation** (item #5) — rxrpl rattrape et adopte en
permanence la chaîne de rippled au lieu de co-valider. 1900+ tests
workspace verts.

## À faire

### 1. Sync — late-join (priorité haute)

`xrpl-hive --sim sync` échoue 0/4. La synchronisation d'un nœud rejoignant
tardivement un réseau ayant déjà de l'activité ne fonctionne pas.

- Le test `late-join-sync` échoue **même en rippled↔rippled** : confirmer
  si c'est un défaut du simulateur/harness ou un vrai manque côté rxrpl.
- Manque probable côté rxrpl : implémentation P2P `GetObjects` /
  `TMLedgerData` au format wire rippled (33 octets `(path, depth)` pour les
  `NodeId`), pour servir/récupérer les objets SHAMap pendant le catchup.
- Lié à la cause racine ci-dessus : un nœud qui rejoint doit acquérir des
  ledgers contenant des transactions.
- Référence : `crates/overlay/`, `crates/node/src/node.rs` (chemin catchup).

### 2. Soak — charge longue durée (priorité moyenne)

`xrpl-hive --sim soak` (`traffic-and-oracle`). Investigué le 2026-05-16.
Le mode de défaillance n'est PAS lié à la durée/charge :

- `--client rxrpl` seul : `numNodes = min(1,3) = 1`, l'oracle se compare à
  lui-même → PASS trivial (non significatif).
- `--client rxrpl,goxrpl` : reproduit l'échec. Deux causes distinctes :
  1. **Bug harness (corrigé).** Le client hive rxrpl démarrait en mode
     `standalone` dès que `XRPL_BOOTNODE` était vide — ce qui est le cas du
     nœud 0 de toute sim multi-nœuds. En standalone rxrpl n'ouvre pas son
     listener P2P 51235, donc le pair ne peut pas se connecter. Corrigé
     dans `clients/rxrpl/xrpl_start.sh` : on bascule en standalone
     uniquement si `XRPL_STANDALONE=1` (le test smoke le pose
     explicitement).
  2. **Interop consensus cross-impl (non corrigé).** Avec le fix harness,
     rxrpl démarre en mode réseau, ouvre le port P2P et le pair s'y
     connecte (vérifié 2026-05-17). Mais la convergence échoue dès le
     premier ledger contenant des transactions — c'est la **cause racine
     commune ci-dessus** (tx-set de consensus). En `rxrpl,rippled` :
     `DIVERGENCE at ledger 6` après 15 paiements soumis ; en `rxrpl,goxrpl` :
     blocage à `closing seq=2 peer_seq=0`.

Le « soak 0/1 » se ramène donc à la cause racine tx-set de consensus.

### 3. Clawback avec Tickets — CORRIGÉ 2026-05-16

Le handler `Clawback` (`crates/tx-engine/src/handlers/clawback.rs`)
consomme désormais le `Ticket` SLE quand `TicketSequence` est fourni
(retrait du owner directory, `erase`, `OwnerCount -1`), au lieu
d'incrémenter toujours la `Sequence` de l'émetteur. Test ajouté
(`clawback_with_ticket_consumes_ticket_not_sequence`). 514 tests
`rxrpl-tx-engine` verts.

### 4. Paiements cross-currency multi-hop / AMM (priorité moyenne)

La PR #92 a implémenté la conversion cross-currency **un seul hop**
IOU→IOU via carnet d'ordres, suffisante pour les 3 tests txcompat visés.
Manquent :

- Les paiements cross-currency **multi-hop** (plusieurs conversions en
  chaîne).
- Le routage via **AMM** (Automated Market Maker).

Cela demande de brancher le moteur `crates/pathfind/` (strands) sur le
tx-engine `crates/tx-engine/src/handlers/payment.rs`, qui fait aujourd'hui
sa propre logique simplifiée.

### 5. Boucle de catchup cross-impl (priorité haute)

Spec : `docs/superpowers/specs/2026-05-15-cross-impl-validation-lag-fix.md`.

Diagnostic affiné le 2026-05-18 (`consensus` ET `soak` en `rxrpl,rippled`
échouent — `node rxrpl did not reach ledger 10: timeout`, y compris sur
des ledgers **vides**) :

- rxrpl tourne un round complet en ~5 s (close-interval 3 s + Establish
  ~2 s). rippled tourne en ~16 s.
- Conséquence : quand rxrpl ferme `#N`, rippled a déjà accepté `#N` ~2 s
  plus tôt avec un autre `close_time`/hash. rxrpl ferme donc `#N` **en
  solo** (Establish trop court pour recevoir le proposal de rippled) →
  divergence.
- rxrpl détecte `wrong prev_ledger`, déclenche un catchup, adopte le `#N`
  de rippled — mais le catchup prend ~15-40 s, pendant lesquels rippled
  avance. rxrpl reste donc en permanence derrière.
- Côté rippled : `prop=0/0` — les proposals de rxrpl arrivent toujours
  périmées (pour un `prev_ledger` que rippled a déjà dépassé).

Le résidu n'est PAS le tx-set (corrigé, voir section « Cause racine »).
C'est un problème de **cadence de consensus** : rxrpl ferme sur un timer
fixe rapide au lieu de fermer quand le consensus converge avec les pairs.

Progrès 2026-05-18 :

- Diagnostic field-by-field ajouté au point de catchup (`node.rs`,
  `catchup: ... diverges from peer parent`). Il révèle le champ exact
  qui diverge : sur un ledger vide c'est le **`close_time`** (ex.
  `832428390` vs `832428400` — un bucket de résolution d'écart).
- `converge()` exige désormais l'accord sur le **bucket de close_time**
  en plus du `tx_set_hash` (mode UNL). Avant, deux ledgers vides
  matchaient instantanément sur `tx_set_hash == ZERO` et rxrpl fermait
  avec son propre close_time non convergé. 236 tests consensus verts.
- Ce correctif est correct mais **ne suffit pas** : le soak `rxrpl,rippled`
  diverge toujours au ledger #5. Couches résiduelles : la dérive
  `effCloseTime` se propage le long de la chaîne (le `close_time` d'un
  `#N` divergent fausse le `prior` du `#N+1`), et il subsiste une
  divergence `tx_hash`/`account_hash` sur les ledgers à transactions
  (ex. `#13`).

Reste à faire (spec A) : différer le close local quand un proposal de
pair indique une chaîne plus avancée (au plus un round, puis fallback
catchup) — la cadence « always-active proposer » a été choisie
volontairement (`node.rs:1520`, issue #76), un deferral naïf avait rendu
rxrpl passif. À implémenter incrémentalement avec vérification hive.

## Hors repo rxrpl

### Harness xrpl-hive

Branche `fix/hive-client-build-deps` du repo `~/Developer/xrpl-hive`
(commits locaux, non poussés). Correctifs cumulés :

- `clients/rxrpl/Dockerfile` : `clang`/`libclang-dev`.
- `clients/rippled/Dockerfile` : `python3`. **+ 2026-05-16** : `USER root`
  avant les étapes de build — l'image `rippleci/rippled:develop` tourne en
  utilisateur non-root, donc `apt-get`/`dnf` et l'écriture de `/version.txt`
  échouaient (l'image client ne se construisait pas du tout).
- `clients/goxrpl/Dockerfile` : **+ 2026-05-16** : ajout de `pkgconf`,
  `openssl-dev`, `openssl-libs-static` — le paquet cgo `peertls/shim`
  exige `pkg-config` (absent de `golang:1.24-alpine`).
- `clients/rxrpl/xrpl_start.sh` : **+ 2026-05-16** : standalone uniquement
  si `XRPL_STANDALONE=1` au lieu de « pas de `XRPL_BOOTNODE` » (voir item #2).

À pousser sur le fork et éventuellement proposer en PR upstream.

## Couverture non vérifiée cette session

- Tests de durée / charge réelle (au-delà de `soak`).
- Hardening production (limites de ressources, comportement sous partition
  réseau prolongée, montée en charge des peers).
