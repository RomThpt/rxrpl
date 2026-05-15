# Spec — Fix close_time monotonicity divergence with rippled (hive)

Date: 2026-05-14
Branch: `fix/close-time-monotonicity`
Status: Draft → in implementation

## TL;DR

rxrpl networked mode produces ledger hashes that diverge from rippled's when
two consecutive ledgers close within the same `close_time_resolution` window
(typically 10 s on a network with 80 % UNL agreement). The root cause is that
`run_consensus_round` selects `effective_close_time` from rounded peer/local
candidates **without enforcing `close_time > parent_close_time`**. rippled
enforces this invariant via `effCloseTime` (`xrpld/consensus/LedgerTiming.h`),
so the same chain produces strictly different ledger headers at every "fast"
close.

Symptom captured empirically on hive `consensus` simulator (2026-05-14):

| seq | rippled hash | rxrpl hash | diff |
|---|---|---|---|
| 4 | `437674F049…AC8C5DF` | `437674F049…AC8C5DF` | match ✓ |
| 5 | `EC28DB5578…F2095D` | `22C2DF1EAE…CB086F9` | **diverge** |

CLOSE_DUMP for rxrpl seq=5: `parent_close_time=832077840 close_time=832077840`.
Equal — the clamp `>= parent + 1` is missing. rippled's seq=5 was built
3 s after seq=4 → it rounds to a different 10 s bucket
(`832077850 > 832077840`), producing a different header hash.

Once any single ledger hash diverges, the next `LedgerHashes` skip-list SLE
diverges (different parent_hash inserted), so `account_hash` diverges, so the
next ledger hash diverges. The chain cannot reconverge without catchup-adopt.

## Evidence

### Hive `consensus` rerun on commit `97d9e76` (current main)

- Test: `mixed-validator-hash-agreement`, target seq ≥ 10 in 120 s.
- Result: **FAIL** — `timeout waiting for ledger seq >= 10`.
- Cluster: 1 rxrpl + 1 rippled stock (no `genesis_amendments_disabled`).

rippled built ledgers
(`/Users/romt/Developer/xrpl-hive/workspace/logs/rippled/client-cd48d18c…log`):
```
12:43:55 Built ledger #3:  37052210D50CE255…
12:44:03 Built ledger #4:  437674F049C38C3C81FD7586DA41157C333A5765AC895BBBCE5856F42AC8C5DF
12:44:06 Built ledger #5:  EC28DB5578D47867A2D4B8AFFE18B40AF601140DD2D30832B6E46D0CE8F2095D
12:44:22 Built ledger #6:  8D1399387D88862B…
12:44:38 Built ledger #7:  FF7F3D28C07FF889…
…
```

rxrpl CLOSE_DUMP (consensus simulator log, same run):
```
12:44:02 seq=4 hash=437674F049C38C3C81FD7586DA41157C333A5765AC895BBBCE5856F42AC8C5DF
         parent_close_time=832077820 close_time=832077840  ← +20 s, OK
12:44:04 seq=5 hash=22C2DF1EAE7048C7A649E61C13C4A8015057729A013AE6BF6035FC99ACB086F9
         parent_close_time=832077840 close_time=832077840  ← equal — BUG
12:44:06 seq=6 hash=CB4A111F8B0D1D27…
         parent_close_time=0          close_time=832077840
```

rxrpl seq=5 close_time is identical to parent_close_time. rippled's
seq=5 wall-clock close happened ~3 s later, rounding to the next bucket.
rxrpl never advances, so every "fast" close adopts the parent's bucket.

### Hive `propagation` on same commit

- Result: **FAIL** (`account not found on rippled` 1 ms after `tesSUCCESS`).
- Underlying chain converged at seq=10 (both 746B7D9B…) because there was
  a 15 s wrong-prev-ledger recovery between #1 and #9 that naturally bumped
  buckets. So the close_time bug doesn't fire when close cadence is slow,
  only when it's fast.

### Hive `smoke` standalone

- Result: **PASS** (3/3). Confirms the codec / RPC layer is healthy;
  the bug is consensus-side close-time handling, not encoding.

## Comparison with rippled

In rippled (`xrpld/consensus/LedgerTiming.h`):

```cpp
inline NetClock::time_point
effCloseTime(
    NetClock::time_point closeTime,
    NetClock::duration resolution,
    NetClock::time_point priorCloseTime)
{
    using namespace std::chrono_literals;
    if (closeTime == NetClock::time_point{})
        return closeTime;
    return std::max<NetClock::time_point>(
        roundCloseTime(closeTime, resolution),
        priorCloseTime + 1s);
}
```

This clamp is applied at every Ledger close: the new close_time is
strictly greater than the parent's close_time by at least one second,
even if the rounded bucket would otherwise be equal.

rxrpl has the exact equivalent function — `rxrpl_consensus::eff_close_time`
(`crates/consensus/src/engine.rs:1420`) — but `run_consensus_round` does
not call it. Instead it picks from a chain of candidates each of which
is rounded but not clamped:

```rust
// crates/node/src/node.rs:2297
let effective_close_time = consensus
    .accepted_close_time()
    .or_else(|| consensus.rounded_close_time())
    .or_else(|| consensus.latest_peer_close_time())
    .unwrap_or_else(|| {
        let res = consensus.adaptive_close_time().resolution();
        rxrpl_consensus::round_close_time(pending_close_time, res)
    });
```

None of `accepted_close_time`, `rounded_close_time`,
`latest_peer_close_time`, nor the local fallback enforce
`> parent.close_time`. So any rapid close produces an equal-bucket header.

## Fix

Wrap the final `effective_close_time` in `eff_close_time` against the
parent ledger's `close_time`:

```rust
// crates/node/src/node.rs (replace lines 2298-2309)
let parent_close_time = ledger.read().await.header.close_time;
let resolution = consensus.adaptive_close_time().resolution();
let raw_close_time = consensus
    .accepted_close_time()
    .or_else(|| consensus.rounded_close_time())
    .or_else(|| consensus.latest_peer_close_time())
    .unwrap_or_else(|| rxrpl_consensus::round_close_time(pending_close_time, resolution));
let effective_close_time =
    rxrpl_consensus::eff_close_time(raw_close_time, resolution, parent_close_time);
```

Notes:
- The parent ledger's `close_time` field is the previous *validated*
  ledger's close_time, which is exactly the right anchor (matches rippled,
  which uses `prevLedger.closeTime`).
- `eff_close_time(0, …, _) == 0` is preserved (the "untrusted close_time"
  sentinel from rippled). This shouldn't fire in networked mode but
  matches rippled's semantics.
- The same wrap should be applied at every other ledger close path. Audit
  list:
  - `run_consensus_round` — the main networked close path (this fix).
  - `close_ledger` (`crates/node/src/node.rs:2942`) — used by RPC `ledger_accept`
    in standalone. Already correct because `close_time` passed in by callers
    is wall-clock and parent_close_time clamp is rarely needed in solo, but
    apply the wrap there too for safety and parity.
- `Ledger::close` itself does not need a clamp — it just stores the
  value. Defense in depth is OK but not required if all call sites use
  `eff_close_time`.

## Regression test

Add a deterministic unit test that exercises the bug path:

```rust
// crates/node/src/node.rs (tests module)
#[test]
fn close_time_strictly_greater_than_parent_on_fast_close() {
    let parent_close_time = 832_077_840;
    let raw = 832_077_842; // wall-clock 2 s past parent; rounds to 832_077_840
    let resolution = 10;
    let eff = rxrpl_consensus::eff_close_time(raw, resolution, parent_close_time);
    assert!(eff > parent_close_time,
        "close_time must be > parent_close_time, got eff={} parent={}",
        eff, parent_close_time);
}
```

And an integration assertion in `crates/node/tests/cross_impl_close_time.rs`:
simulate two consecutive closes 1 s apart, assert each header has
strictly increasing `close_time`.

## Out of scope (separate followups)

1. **Genesis matching with `genesis_amendments_disabled`.** Currently
   `Node::new_standalone` (used by `cmd_network_run`) calls
   `genesis_with_funded_account` which writes FeeSettings + Amendments.
   This matches stock rippled (hive). For xrpl-confluence
   (`genesis_amendments_disabled = true`) it does NOT match. A separate
   config flag should pick the right genesis at startup. Not blocking
   this fix because hive's rippled and rxrpl's current genesis already
   agree (we verified seq=1 hash `E158C218…` in both).
2. **Catchup-adopt cadence.** Even after this fix, `wrong_prev_ledger`
   recovery takes ~15 s per round when networks initially desync. This
   is acceptable for now; reducing it requires faster `StatusChange`
   gossip and is a perf-only change.
3. **propagation test polling.** The `cross-impl-payment` simulator
   queries `account_info` immediately after `submit` returns
   `tesSUCCESS`. The submit returns once the tx is in the local open
   ledger; propagation + apply on the remote node takes ~10–20 ms.
   The test should retry-with-backoff (or query `tx` first to confirm
   propagation). That is an xrpl-hive change, not a rxrpl change.

## Validation plan

1. Apply the fix on a `fix/close-time-monotonicity` branch.
2. Run `cargo test -p rxrpl-node -p rxrpl-consensus`.
3. Rebuild rxrpl docker image via xrpl-hive.
4. Re-run:
   - `./bin/xrpl-hive --sim smoke --client rxrpl` — expect 3/3.
   - `./bin/xrpl-hive --sim consensus --client rxrpl,rippled` — expect 1/1.
   - `./bin/xrpl-hive --sim propagation --client rxrpl,rippled` — may still
     fail due to test polling timing (out of scope #3) but log should
     show byte-identical hashes from seq=2 onward.
5. Cross-validate against xrpl-confluence kurtosis cluster (no regression).
