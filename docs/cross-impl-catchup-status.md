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

### What the trace logs revealed (prop_v13)

Re-ran with `--sim.loglevel 5` to put rippled at trace severity.
Rippled log only grew to 348 lines for a 3-min sim, suggesting the
`[rpc_startup] log_level` mechanism does not actually elevate the
file-logger severity.

The single useful artefact in the trace log:

```
Protocol:NFO [001] processLedgerRequest: Got request for 1 nodes at depth 2, return 4 nodes
```

repeated four times during the sim. This proves rxrpl's TMGetLedger
messages **do** reach rippled and dispatch into `processLedgerRequest`
correctly — so wire framing (4-byte BE size + 2-byte type + payload)
and the protobuf decoding of TMGetLedger are not the problem.

What is conspicuously absent: no `onMessage(TMValidation)` traces,
no `Validation:` messages of any flavour, no exception-from-parse
warnings. rxrpl broadcasts 4 validations, rippled records 0.

Two remaining hypotheses for the next investigator:

1. Rippled dispatches by `protocol::MessageType` enum; rxrpl emits
   the right value (41 = mtVALIDATION, verified against
   `xrpl.proto`). But `onReadMessage` may filter by maximum frame
   size differently for validations than for GetLedger, or apply a
   per-message-type rate limit that drops 100% in this scenario.

2. rxrpl's outbound TMValidation has `Some(stobj)` where the inner
   STObject has a field rippled's `STObject(SerialIter, ...)`
   constructor refuses (e.g. unknown sf, wrong type tag, etc.). The
   exception would normally hit the catch at `PeerImp.cpp:2346`
   (warn level), but if the message is rate-limited *before* reaching
   `onMessage(TMValidation)` then the warn never fires.

Concrete next steps that would actually nail this:

- Add an LLDB breakpoint on `PeerImp::onMessage(TMValidation)` in
  the hive rippled container, or rebuild rippled with an extra
  unconditional log line at the top of that method.
- Or capture the raw 158-byte TMValidation frame from rxrpl with
  tcpdump on the docker bridge and replay it through a rippled unit
  test harness (`STValidation_test.cpp`) to see the exact parse
  error.

### Root cause found in PR #33: NetClock epoch mismatch

The "TMValidation silently dropped" symptom turned out to be a
30-year clock offset. Rxrpl was passing `SystemTime::now() -
UNIX_EPOCH` as the consensus close_time, which then became the
sign_time of every broadcast validation. XRPL's `NetClock` counts
from 2000-01-01 UTC, not 1970, so every validation we sent had a
sign_time ~30 years in rippled's future. Rippled's `isCurrent`
check rejects timestamps outside `[now-3min, now+5min]` and drops
the validation silently at trace severity (`Validation: not
current`), which is why no warning appeared in the rippled log
even after enabling debug.

`crates/rpc-server/src/handlers/ledger_accept.rs` already had the
conversion (with a comment about the XRPL epoch); only the
consensus loops in `node.rs` were missing it. PR #33 applies the
fix using the existing `RIPPLE_EPOCH_OFFSET` constant.

Verified end-to-end via `prop_v14`: `closing with
effective_close_time=830589367` now matches rippled's own clock
(`We closed at 830589xxx`). The drop at `isCurrent` no longer fires.

### Remaining gap: chain divergence after catchup

Even with the timestamp fix, the cross-impl-payment hive sim still
fails because rxrpl and rippled close *different ledger hashes* for
the same sequence:

```
WARN wrong prev_ledger detected: 1/1 trusted peers reference
0873169ADD536733CE174E23E37501002FE0796EBBEA78D94A06BBC4DD4B7A5D,
ours is D7373E4E31B76D102F0D0A14979E8D04892F6B53EE28B3EECC755397261366EB.
Triggering recovery.
```

The flow is:
1. Both nodes start from independently-constructed genesis.
2. rxrpl closes #2 → hash D7373E (its genesis chain).
3. rippled closes #2 → hash 0873169A (its genesis chain).
4. rxrpl receives rippled's TMProposeSet for #N+1 with prev=0873.
5. `wrong_prev_ledger` fires; rxrpl triggers catchup of rippled's #N.
6. Catchup adopts rippled's hash; `Ledger::new_open(reconstructed)`
   makes the next open inherit the adopted parent.
7. Next consensus tick should now propose against rippled's prev,
   but by then rippled has moved on to #N+2 → loop.

After the timestamp fix, validations from rxrpl now reach rippled
without being dropped at `isCurrent`. But because rxrpl validates
its *own* locally-closed hashes (D7373E…) rather than the
catchup-adopted ones (0873…), the two never validate the same
hash, quorum is never met, and `validated_ledger.seq` on rippled
stays at 0.

Concrete next steps for whoever picks this up:
- Audit `node.rs::run_networked` close path: after catchup adoption
  via `*l = new_open(&reconstructed)`, confirm the next consensus
  tick reads the adopted `parent_hash` and not a stale value
  (timing race between `syncing=false` flip and the next tick).
- Cross-check the close pipeline. The state map after adoption
  should be byte-identical to rippled's reconstructed map (PR #30
  + #31 made the hashes compatible). If rxrpl's close adds extra
  state entries (e.g. validator registration, pseudo-tx) that
  rippled does not, the closed-ledger hash will diverge even if
  the input states match.
- Consider deferring local closes when peer proposals indicate a
  more advanced chain. Today rxrpl always closes on its timer tick
  even when it knows it is catching up; closing should wait until
  consensus actually agrees with peers.

### Header reconstruction landed in PR #34

PR #34 caches the parsed `LedgerHeader` from each peer liBASE
response and uses it to populate the catchup-built ledger so the
next `Ledger::new_open(&reconstructed)` inherits the right
parent_close_time, drops, and close_time_resolution from the peer
chain. Verified via `prop_v15`: cache hit on every catchup, header
fields propagate.

### Remaining gap: close_time consensus convergence

After PR #34, with header fields correct on the catchup-derived
ledger, the next *locally-closed* ledger still has a different
hash than what rippled would compute. Diff is in close_time:

```
rxrpl:    closed ledger #4 hash=0A597FD…  effective_close_time=830589420 (08:47:17)
rippled:  CNF Val 1CAF7CC…                close_time=830589440 (08:47:21)
```

A 4-second gap on the wall clock translates into different
close_time fields → different ledger hashes even though everything
else (parent_hash, account_hash, drops, close_time_resolution,
close_flags) matches.

The consensus engine already implements adaptive close-time
resolution (`crates/consensus/src/close_resolution.rs`) and median
+ rounding (`engine.rs::effective_close_time`), but the trigger
sequence in `node.rs::run_networked` does not give peer proposals a
chance to populate before the local close fires:

1. `TimerAction::CloseLedger` → `consensus.close_ledger()` →
   transitions to Establish phase
2. Same tick immediately calls `consensus.converge()` with
   `peer_positions` still empty (or stale)
3. `converge()` finds 1 self < 2 quorum, returns false, no accept
4. The actual close fires later via `TimerAction::Converge`, but
   the engine has already locked in `our_position.close_time` from
   step 1

For 2-validator quorum with quorum=2, both validators MUST agree on
close_time before either closes. This means rxrpl needs to:

- Hold the local close back during the Establish phase until either
  (a) quorum agrees on a close_time, or (b) the establish phase
  times out.
- When converging, replace `our_position.close_time` with the
  `effective_close_time(median, rounded)` derived from peer
  proposals, not the original local SystemTime.

This is the next consensus-engine PR. It does not touch wire
format, hashes, or the adapter — purely the orchestration in
`run_networked`.

## How to reproduce locally

```bash
cd ~/Developer/xrpl-hive
./bin/xrpl-hive --sim propagation --client rxrpl,rippled_2.3.0
```

The rxrpl Dockerfile (`clients/rxrpl/Dockerfile`) is currently
pinned to `tag=fix/shamap-wire-type` so that the in-flight wire
format work is exercised. Adjust to a merged ref once #29-31 land.
