# Plan: Validator List v2 + Cascade Trust (Initiative D)

**Status**: 0% complete | **Target batches**: 5 | **Primary risk**: backward-compat with v1 | **Biggest blocker**: cascade depth/revocation coordination

## 1. Audit Summary

### Current State (v1 only)
- `crates/overlay/src/validator_list.rs` (685 LOC): parses & verifies v1 VL blobs with ephemeral-key signature verification
- `crates/overlay/src/vl_fetcher.rs` (324 LOC): periodically fetches VL from HTTP endpoints, calls `verify_and_parse()`, publishes trusted keys to consensus
- `crates/overlay/src/manifest.rs` (859 LOC): parses STObject manifests (sequence, master/ephemeral keys, domain), rejects revocation (seq==u32::MAX)
- `crates/consensus/src/unl.rs`: maps validator master keys → NodeIds for quorum computation; no time-based filtering
- **Version field**: parsed but ignored; wire format currently only supports `version: 1`

### What Exists
- Publisher master key registration + signing-key rotation + revocation handling (T39, commit 28a2e6d)
- Manifest-store with tracking of revoked publishers
- Per-validator master-key extraction from blob
- Tests: signature verification, stale-sequence detection, rotation attestation, revocation cascade (3 tests in validator_list.rs)

### What's Missing (v2 + cascade)
1. **Blob v2 format**: multiple `blobs_v2` entries with `effective_start` + `effective_expiration` windows; each VL instance has scope
2. **Effective time windows**: parsing & enforcing `effective_start ≤ now < effective_expiration` for each blob; stale/future blobs must be filtered
3. **Cascade resolution**: when a VL references other publishers as "delegates", fetch & verify those publishers' VLs to build transitive trust chain
4. **Signature chain verification**: verify each delegate publisher's signature over the delegation payload using their registered ephemeral key
5. **Publisher key derivation from validator manifest**: each validator can specify a distinct publisher key (not global); blob-level override needed

---

## 2. Target Crates & Files

| Crate | File | Role |
|-------|------|------|
| `overlay` | `src/validator_list.rs` | Core v2 blob parser; effective-time filter; cascade resolver |
| `overlay` | `src/vl_fetcher.rs` | HTTP fetch loop; error handling for stale/future blobs |
| `overlay` | `src/manifest.rs` | *No change required* (manifests are v1-only) |
| `consensus` | `src/unl.rs` | Consume effective-time metadata; mark VLs with scope windows |
| `config` | `src/types.rs` | *Minimal*: cascade-depth-limit config (e.g., max 3 delegations) |
| `tests` | `crates/overlay/tests/vl_v2_cascade.rs` | **New** integration tests; fixture generators |

---

## 3. Batch Plan (B1..B5)

### B1: V2 Parser – Multiple Blobs + Effective Windows
**Goal**: Parse rippled v2 blob array; extract & filter by `effective_start`/`effective_expiration`.  
**Files**: `crates/overlay/src/validator_list.rs` (modify parse_and_parse)

**Changes**:
- Extend `VlPayload` struct to parse optional `blobs_v2: [{ effective_start, effective_expiration, blob, signature }]` array (v2) alongside v1's single blob
- Parse `version` field; route to appropriate parser based on version ≥ 2
- Add `SystemTime`-aware filter: for each blob, check `effective_start ≤ now < effective_expiration`; reject if outside window
- Return list of `(ValidatorListData, effective_window: (u64, u64))` instead of single VL

**Types**:
- `struct BlobV2Entry { effective_start: u64, effective_expiration: u64, blob: Vec<u8>, signature: Vec<u8> }`
- `struct ValidatorListDataV2 { base: ValidatorListData, effective_start: u64, effective_expiration: u64, publisher_id: PublicKey }`
- Enum `ValidatorListVersion { V1(ValidatorListData), V2(Vec<ValidatorListDataV2>) }`

**First failing test**: `test_parse_vl_v2_multiple_blobs_filters_by_time()`
```rust
// Fixture: 3 blobs; time=100; blob1: start=50 end=150 (valid), blob2: start=0 end=50 (expired), blob3: start=150 end=200 (future)
// Expected: only blob1 in result
```

**Verify command**:
```bash
cd /Users/romt/Developer/rxrpl/.worktrees/vl-v2-cascade
cargo test -p rxrpl-overlay test_parse_vl_v2_multiple_blobs_filters_by_time -- --nocapture
```

---

### B2: Effective-Time Filtering in Consensus UNL
**Goal**: Filter validator set by time windows when building trusted set.  
**Files**: `crates/consensus/src/unl.rs` (add method); `crates/overlay/src/validator_list.rs` (expose windows)

**Changes**:
- Add `TrustedValidatorList::update_from_validator_keys_v2(vls: &[ValidatorListDataV2])` 
- For each VL, check `now >= effective_start && now < effective_expiration`; only add validators from non-expired VLs
- Store `(PublicKey → expiration_unix)` map so quorum-recompute can purge expired validators after `effective_expiration` elapses
- In consensus, when computing effective_size, exclude expired validators

**Types**:
- `struct TimeBoundValidator { master_key: PublicKey, expires_at_unix: u64 }`

**First failing test**: `test_unl_filters_expired_validators()`
```rust
// Fixture: 2 validators; expiry_1 = 100, expiry_2 = 200; time=150
// Expected: effective_size() = 1 (only validator 2 counts)
```

**Verify command**:
```bash
cargo test -p rxrpl-consensus test_unl_filters_expired_validators -- --nocapture
```

---

### B3: Cascade Resolution – Delegation Chain
**Goal**: Follow delegation references from primary VL to secondary publishers; resolve transitive trust.  
**Files**: `crates/overlay/src/validator_list.rs` (new fn); `crates/overlay/src/vl_fetcher.rs` (fetch loop)

**Changes**:
- Add `delegates: Option<Vec<PublicKey>>` field to v2 blob structure
- Implement `fn resolve_cascade(publisher_pk, depth_limit, manifest_store, fetcher_client) → Result<Vec<ValidatorListDataV2>>` 
- For each delegate PK, call `fetcher_client.get(delegate_publisher_url)` (URL derived from manifest `sfDomain`), verify blob against delegate's ephemeral key, collect validators
- Enforce `depth ≤ config.validators.cascade_depth_max` (default 3); reject cycles via `visited_publishers: HashSet<PublicKey>`
- On revocation, mark cascaded VLs as stale (delegate revoked → all descendants dropped)

**Types**:
- `struct CascadeBlob { primary_pk: PublicKey, delegates: Vec<PublicKey>, depth: usize }`
- `fn resolve_cascade(...) → Result<Vec<ValidatorListDataV2>, CascadeError>`
- `enum CascadeError { DepthExceeded, CyclicDelegate, DelegateFetchFailed, DelegateRevoked }`

**First failing test**: `test_cascade_resolves_one_level()`
```rust
// Fixture: primary VL with delegates=[delegate_pk_1]; delegate's URL cached in manifest domain
// Expected: cascade() returns validators from both primary and delegate
```

**Verify command**:
```bash
cargo test -p rxrpl-overlay test_cascade_resolves_one_level -- --nocapture
```

---

### B4: Signature & Manifest Verification Chain
**Goal**: Verify blob signatures for cascaded blobs using delegates' ephemeral keys; link manifest rotation to cascade.  
**Files**: `crates/overlay/src/validator_list.rs`; `crates/overlay/src/manifest.rs` (register ephemeral key for delegates)

**Changes**:
- When verifying cascaded blob: extract delegate's current ephemeral key from `manifest_store.get_signing_pk(delegate_pk)`; verify blob signature against that key
- On delegate manifest rotation (T39 `rotate_publisher_signing_key`), invalidate all cascaded VLs from that delegate; require re-fetch
- Add `fn verify_cascade_signature(blob_bytes, delegate_ephemeral_pk, signature) → bool` (same logic as V1 blob sig, reusing `verify_blob_signature`)
- Test: forge bad cascade signature; must reject with `CascadeError::SignatureInvalid`

**Types**:
- `enum CascadeError { ... SignatureInvalid, ... }`

**First failing test**: `test_cascade_rejects_tampered_delegate_signature()`
```rust
// Fixture: cascade blob with valid delegate PK but tampered signature
// Expected: verify_cascade_signature() returns false; cascade resolver returns Err(SignatureInvalid)
```

**Verify command**:
```bash
cargo test -p rxrpl-overlay test_cascade_rejects_tampered_delegate_signature -- --nocapture
```

---

### B5: Integration + Backward-Compat Tests
**Goal**: v1 still works; v2 with/without cascade; stale-blob handling; graceful downgrade.  
**Files**: `crates/overlay/tests/vl_v2_cascade.rs` (new file); `crates/node/src/node.rs` (integrate fetcher)

**Changes**:
- New test file with fixtures for: v1-only, v2 single-blob, v2 multi-blob, v2 with cascade, mixed v1+v2
- Test backward-compat: v1 fetcher continues to work; VL version=1 blob parsed identically to current behavior
- Test stale-blob rejection: when all v2 blobs are expired, node halts new validator additions; uses stale UNL until fresh blob arrives
- Test cascade+rotation: when delegate's signing key rotates, old cached cascade VLs are invalidated; new fetch succeeds with new key
- Test cascade revocation: when primary publisher revokes, all cascaded VLs drop immediately
- Integration: `VlFetcher::run()` detects version in wire payload; routes to v1 or v2 parser

**Test matrix** (minimum 8 tests):
| Test | Scenario | Expected |
|------|----------|----------|
| `test_v1_backward_compat` | version=1 blob | parse identically to current |
| `test_v2_single_blob` | version=2, 1 blob, in-window | accepted |
| `test_v2_all_blobs_expired` | version=2, all expired | stale-UNL used |
| `test_v2_cascade_one_level` | version=2 + delegates | merged validator set |
| `test_v2_cascade_depth_exceeded` | version=2 + 4-level cascade | rejected |
| `test_cascade_delegate_revoked` | cascade + delegate revocation | cascaded VLs dropped |
| `test_cascade_rotation_invalidates_cache` | v2 cascade + delegate key rotation | re-fetch required |
| `test_mixed_v1_v2_publishers` | 2 publishers: one v1, one v2 | both work |

**Verify command**:
```bash
cargo test -p rxrpl-overlay vl_v2_cascade -- --nocapture
cargo test -p rxrpl-node node_integration_vl_v2 -- --nocapture
```

---

## 4. Risk Assessment

### High Risk
1. **Backward-compat with v1 publishers**: existing rippled nodes may not upgrade to v2 immediately; must support both indefinitely
   - *Mitigation*: version detection + dual parser; fallback if v2 unparseable
2. **Cascade depth attacks**: malicious delegate could create circular references or deep chains
   - *Mitigation*: hard limit (default 3); cycle detection via `visited_publishers: HashSet`
3. **Time-based expiration race**: `effective_start ≤ now < effective_expiration` computed at fetch time; clock skew on node causes valid blob to be rejected
   - *Mitigation*: add 5-minute grace window; log warnings at 95% TTL

### Medium Risk
1. **Manifest rotation during cascade fetch**: delegate's ephemeral key rotates mid-fetch; signature verification fails inconsistently
   - *Mitigation*: atomic read of ephemeral key from store; re-try once on mismatch
2. **Cascade URL resolution**: domain not in manifest; requires out-of-band config
   - *Mitigation*: explicit cascade-url mapping in config; fall back to HTTPS://<domain>/ standard path
3. **Memory amplification**: cascade resolver makes N HTTP requests; N = validator count + cascade depth
   - *Mitigation*: serialize cascade fetches; enforce per-fetch timeout; limit total blob size across all cascades

### Low Risk
1. **Tests only cover happy path**: edge cases like partial cascade failures, mixed-version cascades
   - *Mitigation*: B5 test matrix covers; add chaos-injection tests later

---

## 5. Open Questions

1. **Cascade URL encoding**: rippled stores delegate URLs in manifest `sfDomain`; is this reliable? Do we need explicit config?
2. **Revocation semantics**: if primary publisher revokes, do cascaded validators become untrusted immediately, or only after TTL?
3. **Quorum recompute frequency**: effective-time filtering is time-dependent; should consensus engine recompute quorum periodically (e.g., every 10 ledgers)?
4. **Wire-level v2 negotiation**: do we advertise v2 support in P2P handshake, or just silently accept v2 blobs?
5. **Ledger state for VL metadata**: should effective-start/expiration windows be stored in ledger, or ephemeral-only?

---

## 6. Out of Scope

- **Amendment voting for v2 support**: v2 blobs are fetched; no consensus/ledger change required
- **Negative UNL v2**: negative-UNL remains v1-only; cascade does not create negative entries
- **Multi-signature cascade**: each blob has single publisher; multi-sig not supported in v2
- **Time-sync requirements**: assumes system clock is NTP-synchronized (standard assumption); no on-chain time-lock mechanism
- **Cascade over TLS client certificate auth**: only HTTPS GET supported; no mTLS
- **Historical VL archival**: old blobs discarded after TTL; no ledger-based VL history

---

## 7. Completion Estimate

| Batch | LOC Delta | Tests | Days |
|-------|-----------|-------|------|
| B1 | +180 | 3 | 1 |
| B2 | +60 | 2 | 0.5 |
| B3 | +200 | 3 | 1.5 |
| B4 | +100 | 2 | 1 |
| B5 | +250 | 8 | 2 |
| **Total** | **+790** | **18** | **6** |

**Final state**: ~1900 LOC in overlay/validator_list.rs + related; 26 new integration tests (18 + 8 existing regression suite).

---

## 8. Success Criteria

✓ V1 blobs continue to parse & work (regression suite all green)  
✓ V2 single-blob (no cascade) accepted & validators added to consensus UNL  
✓ V2 multi-blob with effective-time filtering: expired blobs ignored  
✓ Cascade chain resolves to depth limit; cycles rejected  
✓ Delegate signature verification enforced; tampered blobs rejected  
✓ Cascade invalidation on delegate revocation  
✓ RPC `validator_list_sites` returns per-blob windows (added to status struct)  
✓ All 8-test matrix (B5) green  
✓ Backward-compat: mainnet rippled + rxrpl mixed UNL still converge  

---

## Appendix: rippled v2 Blob Format Reference

```json
{
  "version": 2,
  "public_key": "ED2677...",
  "manifest": "<base64>",
  "blobs_v2": [
    {
      "effective_start": 700000000,
      "effective_expiration": 700100000,
      "blob": "<base64 JSON with validators + delegates>",
      "signature": "<hex>"
    },
    { ... }
  ]
}
```

**Blob JSON (v2)**:
```json
{
  "sequence": 5,
  "expiration": 700100000,
  "validators": [
    { "validation_public_key": "ED1234...", "manifest": "<base64>" }
  ],
  "delegates": [
    "ED5678..."  // optional; references other publishers' PKs
  ]
}
```

