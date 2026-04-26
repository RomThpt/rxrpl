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

## Suggested next PR scope

Fix `consensus::engine::peer_proposal` to:
- Hold proposals whose `prev_ledger` we do not yet know rather than
  rejecting them outright.
- Replay the holding pen when catchup completes for a ledger that
  matches a held proposal's `prev_ledger`.
- Drop holding pen entries older than ~60 seconds to prevent
  unbounded growth.

This is a consensus/orchestration change, not a wire format change.
It does not depend on the SHAMap PRs and could land independently.

## How to reproduce locally

```bash
cd ~/Developer/xrpl-hive
./bin/xrpl-hive --sim propagation --client rxrpl,rippled_2.3.0
```

The rxrpl Dockerfile (`clients/rxrpl/Dockerfile`) is currently
pinned to `tag=fix/shamap-wire-type` so that the in-flight wire
format work is exercised. Adjust to a merged ref once #29-31 land.
