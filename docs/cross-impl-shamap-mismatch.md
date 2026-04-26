# Cross-impl SHAMap incompatibility with rippled

## Symptom

When rxrpl peers with a real rippled node and tries to catchup via
`liAS_NODE` GetLedger requests, `feed_nodes` accepts the response from
the wire but `add_raw_node` integrates 0 nodes per round. After 21
rounds the incremental sync is purged, fallback `GetObjectByHash`
returns the same nodes, and the ledger is never adopted.

Reproducer: `xrpl-hive simulators/propagation` with `rxrpl + rippled`
on the same private network.

## Root cause

rxrpl's SHAMap node hashing and wire format diverged from rippled.
There are three distinct, compounding issues.

### 1. Leaf hash byte order is reversed

Rippled (`SHAMapAccountStateLeafNode.h:45`,
`SHAMapTxPlusMetaLeafNode.h:45`):

```cpp
hash_ = sha512Half(HashPrefix::leafNode, item_->slice(), item_->key());
//                                       ^^^^^^ data    ^^^^^ key
```

rxrpl (`crates/shamap/src/leaf_node.rs:106-119`):

```rust
fn hash_account_state(item: &SHAMapItem) -> Hash256 {
    sha512_half(&[&prefix, item.key().as_bytes(), item.data()])
    //                     ^^^^^ key              ^^^^^ data  -- reversed
}
```

Same reversal in `hash_tx_with_meta`. `hash_tx_no_meta` is correct
(no key in hash).

Consequence: any non-empty SHAMap built by rxrpl has a different root
hash than what rippled would compute for the same items. A ledger
built by rxrpl can never match a ledger built by rippled.

### 2. Wire format trailing byte is wireType, not depth

Rippled `SHAMapTreeNode::makeFromWire` (`SHAMapTreeNode.cpp:78-106`)
treats the trailing byte as a `wireType` enum:

```
wireTypeTransaction          = 0  // leaf, payload = data
wireTypeAccountState         = 1  // leaf, payload = data || key
wireTypeInner                = 2  // full inner, payload = 16*32 bytes
wireTypeCompressedInner      = 3  // sparse inner, payload = N*(hash[32]||branch[1])
wireTypeTransactionWithMeta  = 4  // leaf, payload = data || key
```

rxrpl `crates/overlay/src/ledger_sync.rs:269-275` and
`crates/overlay/src/peer_manager.rs:2266` treat the trailing byte as
"depth". The byte is appended on send and stripped on receive
symmetrically in rxrpl-to-rxrpl, so the protocol works between rxrpl
nodes. It does not match rippled.

Consequence:

- rxrpl strips the wireType byte expecting depth, then mis-routes
  inner vs leaf based on `raw.len() == 512` rather than the wireType.
  Compressed inner (wireType 3) is never recognized.
- rxrpl's outbound wire would fail strict parsing on rippled
  (rippled would refuse the unknown trailing-byte value if it were
  outside `0..=4`; for small ledgers the depth value happens to fall
  inside `[0..64]` which overlaps wireType `[0..4]` only sometimes).

### 3. Storage payload byte order

Rippled wire payload for a leaf is `data || key`. rxrpl's
`deserialize_node` (`crates/shamap/src/node_store.rs:34-39`) treats
stored bytes as `key (32) || data`. Even if the hash matched, a
subsequent `child_with_store` traversal would deserialize the leaf
with key and data swapped.

For inner nodes, rippled wire payload is 16*32 bytes for full inner,
matching rxrpl's storage layout. Compressed inner (wireType 3) needs
to be expanded into a 16-slot structure before storing.

## Why rxrpl-to-rxrpl works

Both sides use the same (incorrect) leaf hash order and the same
"depth byte" wire convention. The SHAMap is internally consistent.
The bug is invisible until you try to interop with rippled.

## Required fix (all three)

1. **Leaf hash order**: switch `hash_account_state` and
   `hash_tx_with_meta` to `prefix || data || key` to match
   `HashPrefix::leafNode` and `HashPrefix::txNode` semantics in
   rippled. This is a breaking change for any rxrpl-stored ledger.

2. **Wire format**:
   - On send: append the correct `wireType` byte (1=AccountState,
     4=TxWithMeta, 0=Tx, 2=Inner, 3=CompressedInner) instead of depth.
   - On receive: dispatch on the trailing byte, not on payload size.
     Reorder leaf payload to canonical `key || data` for storage,
     expand compressed inner into 16-slot full inner.

3. **Storage payload**: leaf nodes must be stored as `key || data`
   (current rxrpl convention is fine — just convert on receive from
   the wire).

## Out of scope for the cross-impl-catchup PR

These changes touch the SHAMap hash semantics, the ledger root hash,
the wire codec, and every test fixture that hard-codes a SHAMap hash.
A focused PR per concern is required:

- PR A: leaf hash order fix + test fixture rebase
- PR B: wire-format wireType handling + compressed inner support
- PR C: liAS_NODE round-trip integration test against rippled
  standalone via xrpl-hive

## References

- Rippled wire format constants:
  `include/xrpl/shamap/SHAMapTreeNode.h:17-21`
- Rippled leaf hash:
  `include/xrpl/shamap/SHAMapAccountStateLeafNode.h:45`
- Rippled `makeFromWire`:
  `src/libxrpl/shamap/SHAMapTreeNode.cpp:78-106`
- rxrpl leaf hash:
  `crates/shamap/src/leaf_node.rs:106-119`
- rxrpl wire receive:
  `crates/overlay/src/ledger_sync.rs:267-307`
- rxrpl wire send:
  `crates/overlay/src/peer_manager.rs:2258-2275`
