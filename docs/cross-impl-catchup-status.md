# Cross-impl catchup status (rxrpl ↔ rippled 2.3.0)

After PRs #29 (peer fallback), #30 (leaf hash byte order) and #31
(rippled wireType encoding/decoding), rxrpl can correctly
reconstruct SHAMap state received from a real rippled peer. The
remaining test failure has a different root cause.

## What now works (validated against rippled 2.3.0)

xrpl-hive `propagation` simulator with `--client rxrpl,rippled_2.3.0`,
log: `~/Developer/xrpl-hive/workspace/logs/rxrpl/client-d54...log`

```
incremental sync complete for ledger #3 (3 leaf nodes)
sync: adopted ledger #3, open ledger is now #4
catchup complete, resuming consensus at ledger #4
incremental sync complete for ledger #4 (3 leaf nodes)
... ledgers #5, #6, #7, #8, #9, #10, #11, #12 all sync ...
catchup: reconstructed ledger #12 hash=DF9732C28BC0...
sync: adopted ledger #12, open ledger is now #13
```

`feed_nodes` adds 2-3 nodes per round (out of 4 received) where
before the wire format fix it added 0 every round and gave up after
21 zero-add rounds. The full chain rxrpl ← (TLS handshake) ← rippled
→ TMGetLedger → TMLedgerData → SHAMap reconstruction now works.

## What still doesn't work

Test `cross-impl-payment` still fails:

```
node rippled did not reach ledger 5: timeout waiting for ledger seq >= 5
```

This is a *consensus timing* issue, not a wire format issue. Chain:

1. rippled closes a candidate ledger every ~16 seconds.
2. rippled sends a `TMProposeSet` to rxrpl referencing
   `prev_ledger=X`.
3. rxrpl checks: `our prev_ledger = X-1`, theirs = `X`.
4. rxrpl rejects the proposal: `rejected proposal: prev_ledger
   mismatch (ours=..., theirs=...), tracking for recovery`.
5. rxrpl triggers catchup sync of ledger `X`.
6. By the time catchup of `X` completes (~16 s), rippled has closed
   `X+1`. Loop repeats.

Result:
- rxrpl never participates in consensus (always one ledger behind).
- rippled never produces a validated ledger because quorum=2 needs
  rxrpl's validation.
- `WaitForLedger(5)` queries rippled's `validated_ledger.seq` which
  stays at 0 forever, so the test times out.

## Root cause categorisation

| Layer | Status |
|---|---|
| TLS handshake rxrpl ↔ rippled | works |
| HTTP protocol upgrade | works |
| TMGetLedger request format | works |
| TMLedgerData decoding | **fixed by PRs #29-31** |
| SHAMap node hash compatibility | **fixed by PR #30** |
| SHAMap wire format compatibility | **fixed by PR #31** |
| Catchup adoption of validated ledger | works |
| Consensus participation while catching up | broken |
| Validation broadcast / signature acceptance | not yet exercised |

The remaining gap is in rxrpl's consensus engine: it cannot
participate in a round whose `prev_ledger` it has only just learned
about. rippled's behaviour (treat unknown prev_ledger as a hint to
sync, but keep the proposal in a holding pen so it can be processed
once sync completes) would unblock this — rxrpl currently discards
the proposal and only triggers sync as a side-effect.

## What landed in PR #32

Done. `consensus::engine::peer_proposal` now:
- Holds proposals whose `prev_ledger` we do not yet know in a bounded
  pen (`future_proposals`), keyed by their `prev_ledger` hash.
- Replays held proposals into `pending_proposals` when `start_round`
  moves us to a matching prev_ledger.
- Evicts entries older than 5 ledger seqs, caps at 64 distinct keys
  and 16 proposals per key with per-node dedup.

Plus a follow-up fix to `peer_proposal`: treat `proposal.ledger_seq
== 0` as "unknown" and trust the prev_ledger equality. rippled's
`TMProposeSet` wire format does not carry ledger_seq, so
`proto_convert::decode_propose_set` always emits 0; the prior
seq-mismatch guard rejected every cross-impl proposal.

End-to-end measurement (xrpl-hive prop_v11.log with `--client
rxrpl,rippled_2.3.0 --docker.nocache rxrpl`):

```
proposals received  : 23
proposals held      : 20
proposals replayed  : 4
proposals accepted  : 3
proposals broadcast : 8
validations broadcast: 4
ledgers closed locally: 4
ledgers adopted via sync: 4
```

rxrpl now participates in consensus rounds with rippled. Held
proposals get replayed cleanly after catchup. The cross-impl-payment
test still fails for a *different* downstream reason described
below.

## Remaining gap: rippled silently drops rxrpl's TMValidation

```
rxrpl  : validations broadcast = 4
rippled: validations received  = 0
```

Rippled never logs receipt of any validation from rxrpl, even though
rxrpl serializes and writes them on the open peer connection. Both
peers stay connected throughout the test. Likely causes (untested):

1. TMValidation STObject encoding has a field rippled refuses to
   deserialize (e.g. ordering, an extra optional field, missing
   sfBaseFee, sfReserveBase, etc.).
2. The validator's master signature is missing or computed over a
   different signing payload than what rippled expects from a
   manifest-less validator (no token, just a `[validation_seed]`).
3. The relay path on rippled silently discards messages whose source
   peer ID does not match the manifest-claimed key.

Diagnostic next step: enable rippled's `Protocol:DBG` and
`Validations:DBG` log levels in the hive client config, then look
for `Invalid signature`, `dup validation`, `wrong signing key`, or
`unknown validator`. If nothing logs, capture the raw bytes of the
TMValidation frame leaving rxrpl and feed it into a rippled
unit-test harness offline.

This is the next concrete blocker after the 4 PRs in the SHAMap +
consensus stack land. It is independent of any of them and would be
its own follow-up PR.

## How to reproduce locally

```bash
cd ~/Developer/xrpl-hive
./bin/xrpl-hive --sim propagation --client rxrpl,rippled_2.3.0
```

The rxrpl Dockerfile (`clients/rxrpl/Dockerfile`) is currently
pinned to `tag=fix/shamap-wire-type` so that the in-flight wire
format work is exercised. Adjust to a merged ref once #29-31 land.
