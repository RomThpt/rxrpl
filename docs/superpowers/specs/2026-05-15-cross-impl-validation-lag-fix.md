# Spec — Fix cross-impl validation lag in hive propagation

Date: 2026-05-15
Branch: TBD (`fix/cross-impl-validation-lag`)
Status: Draft

## TL;DR

In the hive `propagation` simulator (mixed cluster: rxrpl + rippled-stock),
rxrpl stays exactly one validated ledger behind rippled and the test times
out. The lag is structural, not a tuning problem: rxrpl signs and broadcasts
its `Validation` only at the very end of `close_consensus_round`, and the
validation carries the hash of the ledger rxrpl just closed *locally*. While
rxrpl is behind, that local hash does not match the hash rippled validated for
the same sequence, so the two implementations' validations land under
different `(seq, hash)` keys in `ValidationAggregator` and neither side ever
reaches UNL quorum for rxrpl's branch. `validated_ledger` therefore never
advances past rippled's tip.

Tuning the converge cadence (done in #84, converge ≈ 2 s) does not help: the
validation is always emitted at the tail of the round, never ahead, and it
always names a divergent hash.

## Where the lag is created

Path: `TimerAction::CloseLedger` → `TimerAction::Converge` → `converge()` →
`close_consensus_round` (`crates/node/src/node.rs:2581`).

1. `close_consensus_round` fully closes ledger `#N`: `l.close(...)`
   (`node.rs:2667`), then `*l = Ledger::new_open(&closed)` (`node.rs:2735`).
2. Only after that, in the `Broadcast validation` block (`node.rs:2754-2797`),
   it builds `Validation { ledger_hash: hash, ledger_seq: closed_seq, .. }`
   where `hash` is the **locally closed** hash (`node.rs:2766`), signs it, and
   broadcasts + self-injects it.
3. `validated_ledger` only advances when `ValidationAggregator::add_validation`
   returns a quorum-reaching `ValidatedLedger`
   (`crates/overlay/src/validation_aggregator.rs:171`). The aggregator keys
   votes by `by_ledger: HashMap<(u32, Hash256), Vec<Validation>>`
   (`validation_aggregator.rs:36`) — quorum is per **(seq, hash)** pair.
4. `LedgerClosed` is deliberately NOT emitted at local close
   (`node.rs:2720-2728`); the network-validated view is gated entirely on the
   aggregator.

Consequence: while rxrpl is one ledger behind, its local close of `#N`
produces a hash divergent from the `#N` rippled already validated. rxrpl's
validation for `#N` lands under key `(N, hash_rxrpl)`, rippled's under
`(N, hash_rippled)`. Neither key collects the `ceil(N*0.8)` quorum. By the
time rxrpl catches up via catchup-adopt (which *does* broadcast a validation,
`node.rs:2370-2423`, with the adopted hash `reconstructed.header.hash`),
rippled has already closed `#N+1` — the lag is preserved one ledger forward.
See `docs/cross-impl-catchup-status.md` for the empirical loop.

## Root cause

Two coupled defects:

1. **Divergent hash.** rxrpl validates the hash of its own local close, not the
   hash the network converged on. If rxrpl's `#N` header is not byte-identical
   to rippled's `#N`, its validation is unusable for cross-impl quorum.
2. **Late emission.** Even with identical hashes, the validation is emitted at
   the *tail* of the round, after `new_open`. There is no half-round of slack
   to absorb normal cross-impl timing skew.

Defect (1) is dominant: timing fixes are worthless while hashes diverge.

## Proposed fix

Ordered by leverage. (A) and (B) are the real fix; (C)/(D) are follow-ups.

### (A) Validate the converged/adopted hash, not the local close — REQUIRED

When `check_wrong_prev_ledger` or the catchup-adopt path indicates the network
is on a different (more advanced) chain, rxrpl must validate the **adopted**
hash, never re-broadcast a validation for its divergent local close. The
catchup-adopt block (`node.rs:2370-2423`) already does the right thing — it
validates `reconstructed.header.hash`. The gap is the *normal* close path
(`close_consensus_round`): it should not emit a validation for `#N` at all
when a trusted peer position already proves `#N` converged to a different
hash. Defer the local close instead (see recommendation in
`docs/cross-impl-catchup-status.md`: "defer local closes when peer proposals
indicate a more advanced chain").

Concretely: before the `Broadcast validation` block at `node.rs:2754`, check
whether the engine saw a quorum-supported peer hash for `closed_seq` that
differs from `hash`. If so, skip the broadcast/self-inject and let the
catchup-adopt path produce the validation for the network-agreed hash.

### (B) Guarantee byte-identical headers vs rippled — REQUIRED

Cross-impl quorum is impossible unless rxrpl and rippled produce identical
`#N` headers. The close-time monotonicity fix
(`docs/superpowers/specs/2026-05-14-close-time-monotonicity-fix.md`,
`eff_close_time` at `node.rs:2635`) is the prerequisite. Verify on the hive
`consensus` simulator that headers are byte-identical from seq=2 onward
*before* relying on (A). If any field still diverges (close_flags, account
hash ordering, amendment-vote application order on flag ledgers), that must be
closed first — list each divergent field with a per-field fix.

### (C) Emit the validation earlier — FOLLOW-UP

Once hashes match, move signing/broadcast of the `Validation` from the tail of
`close_consensus_round` to the Establish→accepted transition, on the converged
position, before `Ledger::new_open`. This buys ~half a round of slack against
timing skew. Pure latency optimization — do not attempt before (A)+(B).

### (D) Speculative pre-vote — OPTIONAL

Allow rxrpl to validate a peer-proposed seq that has been accepted before its
own local close completes, mirroring rippled's `Validations::add` from
`onAccept`. Only worthwhile if (C) proves insufficient.

## Success criteria

1. hive `consensus` simulator: rxrpl/rippled `#N` headers byte-identical for
   all `N >= 2` (precondition, gate for the rest).
2. hive `propagation` simulator passes: after a payment, the target reaches
   `current + 2` within the 60 s budget (`interop/tests/test_propagation.py`).
3. `validated_ledger.seq` on rxrpl tracks rippled with lag 0 in steady state
   (no permanent one-ledger offset).
4. `cargo test --workspace` green; no regression in the mixed-validator
   confluence tests.

## Risks / open questions

- (A) requires a reliable "network converged on a different hash for
  `closed_seq`" signal inside `close_consensus_round`. The engine exposes
  `check_wrong_prev_ledger` and peer positions — confirm one of them can be
  evaluated for the *just-closed* seq, not only the open round.
- Deferring local closes risks rxrpl never closing at all if the signal is
  noisy. Bound it: defer at most one round, then fall back to local close +
  catchup (the existing recovery).
- (B) may surface additional header-divergence sources beyond close_time;
  budget time to enumerate them on the `consensus` simulator.
