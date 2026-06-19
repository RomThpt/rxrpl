# Scope: following the modern mainnet via play-forward

Status: scoping (2026-06-19). Prereq context: `catchup_via_replay` + follow-mode
are wired (step 4 done); the node tracks early-2013 mainnet #268000..=#268071
byte-faithful. This document scopes what it takes to follow the *modern* mainnet
(2024+) byte-exact, based on a real tx-type inventory.

## Why "modern" and not "extend the 2013 range"

The early-2013 small-state ledgers are the only cheaply-bootstrappable ones, but
they carry pre-amendment quirks with no gate (directory page-linking changed
pre-rippled-0.27; trust-create peer-stamp is not derivable from on-ledger state —
empirically the same issuer is stamped on some creations and not others at the
same page). The live play-forward node never replays 2013, so those are
out-of-scope artifacts of test-oracle selection. Real validation must use modern
state.

## Tx-type inventory (mainnet tip ~#105032915, 120 ledgers, 8925 tx)

- OfferCreate 48%, Payment 31%, OfferCancel 10%  -> 89%
- AccountSet 2%, TrustSet 1%, NFTokenCreateOffer 1%, CheckCash 1%
- TicketCreate, NFTokenMint/Burn/Accept/Cancel, CheckCreate, AMMDeposit,
  OracleSet, AccountDelete  -> each <1%

Implication: a ledger is byte-exact only if EVERY tx in it is. With ~74 tx/ledger
and ~11% "other" types, essentially every modern ledger contains a type outside
OfferCreate/Payment/OfferCancel. Following modern mainnet byte-exact therefore
requires the (near) full modern transactor set, not just the 89% head.

## What is already byte-exact

OfferCreate (no-cross / full-fill / partial-single / owner-funds clamp),
OfferCancel, TrustSet (modern), Payment (XRP-only). Skip-list + metadata +
Rules-from-Amendments wired.

## Phases

1. Bootstrap modern state at scale. `download_state_via_rpc` already verifies
   root == account_hash; validate it at modern size (~tens of millions of
   entries, thousands of `ledger_data` pages). Mostly a perf/memory check.
2. Per-tx-type fidelity (the bulk, multi-month):
   - Payment IOU path: f64 -> IOUAmount for apply_trust_delta / adjust_iou_balance
     / issuer_transfer_rate / cross-currency / pathfinding.
   - OfferCreate: flags (tfSell/tfPassive/IoC/FoK), IOU<->IOU + autobridge,
     AMM-routed crossing.
   - New transactors byte-exact: AccountSet, NFToken* (6), Check* (3), Ticket,
     AMM* (deposit/withdraw/vote/bid/create/delete), Oracle*, AccountDelete,
     Escrow, PayChannel, DID, etc.
3. Live follow demo: run_networked follow-mode against a full-history hub,
   bootstrap a recent ledger, follow K ledgers byte-exact.

## Testing strategy (avoid full-state bootstrap per test)

Full modern-state download per test is impractical (~30 min). Instead, unit-test
each transactor against rippled oracles using targeted `ledger_entry` queries for
only the SLEs a tx touches (parent + affected entries), replay the single tx, and
compare byte-for-byte. Reserve the full-state e2e for the final live demo.

## Blocked / external

- Multi-offer legacy crossing: needs a pre-FlowCross rippled (~2017, no docker
  image) to observe the stop condition. Blocked.

## Recommended first milestone

Pick one modern ledger whose successor tx-set is entirely already-supported types
(Payment XRP / OfferCancel / simple OfferCreate), bootstrap it, and prove one
byte-exact modern forward step end-to-end. Then drive phase 2 type-by-type,
ordered by frequency (Payment IOU and OfferCreate flags first).
