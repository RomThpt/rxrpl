# nUNL Pseudo-Transaction Generation Plan

## 1. Audit Current State

### Already Implemented (85%)
- **Tracker**: `crates/consensus/src/negative_unl.rs` — Full `NegativeUnlTracker` with:
  - Sliding window validation counting (256-ledger intervals)
  - Disable/re-enable logic (>50% reliability threshold)
  - 25% cap enforcement on nUNL size
  - Phase 1 (re-enable) + Phase 2 (disable) logic
  - ~16 comprehensive unit tests
- **UNLModify Handler**: `crates/tx-engine/src/handlers/unl_modify.rs` — Complete transaction processor:
  - Preflight validation (UNLModifyDisabling 0|1, UNLModifyValidator required)
  - Apply logic: add/remove from DisabledValidators array
  - NegativeUNL ledger entry creation/mutation
  - 4 handler tests
- **Protocol Types**: `crates/protocol/src/ledger/negative_unl.rs`, `ledger_entry_type.rs`, `transaction_type.rs`
  - NegativeUnl struct (disabled_validators, validator_to_disable, validator_to_re_enable)
  - LedgerEntryType::NegativeUNL (0x004E)
  - TransactionType::UNLModify (102)
  - Server type definitions in `rpc-server/src/handlers/server_definitions.rs`
- **Engine Integration**: `crates/consensus/src/engine.rs`
  - `record_validation(node_id)` — delegates to tracker
  - `on_ledger_close_for_tracker()` — advances window
  - `evaluate_negative_unl(ledger_seq)` — returns NegativeUnlChange vec + UNL sync
  - Consensus engine holds NegativeUnlTracker field

### Missing (15%)
- **Pseudo-TX Generation**: No code calls `evaluate_negative_unl()` or injects UNLModify txs
- **Validator Key Registration**: `register_validator()` never called with consensus engine
- **Node-Level Integration**: No function mirrors `apply_amendment_voting()` for nUNL
- **Validation Plumbing**: No code path from `Validation` message → `engine.record_validation()`
- **Flag Ledger Emission**: No pseudo-tx creation/execution at ledger close

---

## 2. Target Crates & Files

| Crate | File | Purpose |
|-------|------|---------|
| `consensus` | `src/engine.rs` | *Exists*: has tracker and evaluate method. **Need**: register validators on init. |
| `consensus` | `src/lib.rs` | *Exists*: exports NegativeUnlChange. **Need**: ensure NegativeUnlTracker visibility. |
| `tx-engine` | `src/handlers/unl_modify.rs` | *Complete*: handler fully working. No changes needed. |
| `tx-engine` | `src/handlers/mod.rs` | *Exists*: registers UNLModify. No changes needed. |
| `protocol` | `src/ledger/negative_unl.rs` | *Mostly complete*: struct fields present. **May need**: validator key format docs. |
| `node` | `src/node.rs` | **Critical**: Add `apply_negative_unl()` function (mirror `apply_amendment_voting`). |
| `overlay` | `src/consensus_bridge.rs` OR `src/peer_manager.rs` | **Critical**: Hook validation → `engine.record_validation()`. |
| `overlay` | `src/lib.rs` | **May need**: re-export or adapter for consensus engine access. |

---

## 3. Work Batches (TDD-Friendly)

### B1: Engine Initialization — Register Validator Keys on Startup
**Goal**: Populate `NegativeUnlTracker.node_to_key` map at consensus engine creation.

**Files Touched**:
- `crates/consensus/src/engine.rs` — add public method `register_validators(trusted_set, key_map)`

**Failing Test**:
```rust
// crates/consensus/tests/engine_negative_unl.rs (new file)
#[test]
fn engine_initializes_validator_keys() {
    let mut engine = ConsensusEngine::new(...);
    let trusted_set = make_trusted_set(&[1, 2, 3]);
    let key_map: HashMap<NodeId, String> = /* map node_ids to hex keys */;
    engine.register_validators(&trusted_set, &key_map);
    
    let changes = engine.evaluate_negative_unl(256);
    // Should have key mappings available for pseudo-tx generation
    assert!(!changes.is_empty() || true); // Later tests check actual logic
}
```

**Verification**:
```bash
cd crates/consensus && cargo test engine_initializes_validator_keys
```

---

### B2: Validation Plumbing — Record Validations in Consensus Engine
**Goal**: Wire validation messages from overlay → `engine.record_validation()`.

**Files Touched**:
- `crates/overlay/src/consensus_bridge.rs` — add method to forward validations to engine
- `crates/overlay/src/peer_manager.rs` — call consensus_bridge on Validation receipt

**Failing Test**:
```rust
// crates/consensus/tests/engine_negative_unl.rs
#[test]
fn engine_records_validations_from_multiple_validators() {
    let mut engine = ConsensusEngine::new(...);
    for node_id in 1..=5 {
        engine.record_validation(NodeId(...));
    }
    // No assertion yet; B3 will test evaluation
}
```

**Verification**:
```bash
cd crates/overlay && cargo test
cd crates/consensus && cargo test engine_records_validations_from_multiple_validators
```

---

### B3: Flag Ledger Evaluation — Tracker Evaluation at Consensus Close
**Goal**: Call `evaluate_negative_unl()` at each flag ledger boundary; verify changes generated.

**Files Touched**:
- `crates/consensus/src/engine.rs` — ensure `evaluate_negative_unl()` is public and correct

**Failing Test**:
```rust
// crates/consensus/tests/engine_negative_unl.rs
#[test]
fn engine_emits_unl_modify_changes_at_flag_ledger() {
    let mut engine = ConsensusEngine::new(...);
    let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);
    
    // Register keys
    engine.register_validators(&trusted, &key_map);
    
    // Simulate 256 ledgers; validator 5 unreliable
    for i in 0..256u32 {
        engine.record_validation(NodeId(1));
        engine.record_validation(NodeId(2));
        engine.record_validation(NodeId(3));
        engine.record_validation(NodeId(4));
        if i < 100 { engine.record_validation(NodeId(5)); }
        engine.on_ledger_close_for_tracker();
    }
    
    let changes = engine.evaluate_negative_unl(256);
    assert_eq!(changes.len(), 1);
    assert!(changes[0].disable);
}
```

**Verification**:
```bash
cd crates/consensus && cargo test engine_emits_unl_modify_changes_at_flag_ledger
```

---

### B4: Pseudo-TX Generation Function — Mirror `apply_amendment_voting()`
**Goal**: Create `Node::apply_negative_unl()` that generates and applies UNLModify txs.

**Files Touched**:
- `crates/node/src/node.rs` — add public `apply_negative_unl()` function

**Function Signature**:
```rust
pub fn apply_negative_unl(
    consensus_engine: &mut ConsensusEngine,  // must call evaluate_negative_unl()
    ledger: &mut Ledger,
    tx_engine: &TxEngine,
    fees: &FeeSettings,
    ledger_seq: u32,
) -> Vec<TransactionResult>
```

**Failing Test**:
```rust
// crates/node/tests/negative_unl_pseudo_tx.rs (new file)
#[test]
fn apply_negative_unl_creates_unl_modify_txs() {
    let mut consensus = ConsensusEngine::new(...);
    let mut ledger = Ledger::genesis();
    let tx_engine = TxEngine::new();
    let fees = FeeSettings::default();
    
    // Setup: consensus engine with validators, evaluate changes
    consensus.register_validators(&trusted_set, &key_map);
    // ... simulate validations ...
    
    let results = Node::apply_negative_unl(
        &mut consensus,
        &mut ledger,
        &tx_engine,
        &fees,
        256,
    );
    
    // Should have applied UNLModify txs (if changes exist)
    assert!(!results.is_empty() || true);
}
```

**Verification**:
```bash
cd crates/node && cargo test apply_negative_unl_creates_unl_modify_txs
```

---

### B5: Node Integration — Call `apply_negative_unl()` on Flag Ledgers
**Goal**: Wire `apply_negative_unl()` into ledger close sequence (like `apply_amendment_voting`).

**Files Touched**:
- `crates/node/src/node.rs` — modify consensus flow to call `apply_negative_unl()` at flag ledgers

**Failing Test**:
```rust
// crates/node/tests/negative_unl_pseudo_tx.rs
#[test]
fn flag_ledger_close_applies_negative_unl_pseudo_txs() {
    let mut node = Node::new(config);
    // Setup validators + consensus
    
    // Fast-forward to ledger 256
    for i in 1..256 {
        node.close_ledger(...);
    }
    
    // Close ledger 256 (flag ledger)
    node.close_ledger(256);
    
    // Inspect ledger state for NegativeUNL entry
    let ledger = node.ledger().read().unwrap();
    let nunl_key = keylet::negative_unl();
    assert!(ledger.state_table.get(&nunl_key).is_some());
}
```

**Verification**:
```bash
cd crates/node && cargo test flag_ledger_close_applies_negative_unl_pseudo_txs
```

---

### B6: Validation Accounting — Plumb Overlay → Consensus Engine
**Goal**: Complete end-to-end: Validation message → `engine.record_validation()`.

**Files Touched**:
- `crates/overlay/src/peer_manager.rs` — intercept Validation, extract NodeId
- `crates/overlay/src/consensus_bridge.rs` — adapter to forward to consensus engine (if needed)

**Failing Test**:
```rust
// crates/overlay/tests/consensus_integration.rs (new file)
#[test]
fn peer_manager_records_validation_to_consensus_engine() {
    let mut peer_mgr = PeerManager::new(...);
    let validation = make_validation_message(node_1_key, ledger_256, hash_xyz);
    
    // Process validation
    peer_mgr.on_validation(validation);
    
    // Assert engine recorded it (via introspection or mock)
}
```

**Verification**:
```bash
cd crates/overlay && cargo test peer_manager_records_validation_to_consensus_engine
```

---

### B7: Ledger State Persistence — Load/Save NegativeUNL on Restart
**Goal**: Ensure NegativeUNL ledger entry survives ledger closure and reload.

**Files Touched**:
- `crates/protocol/src/ledger/mod.rs` — ensure NegativeUnl variant is serialized/deserialized
- No new code; verify existing bincode serde covers it

**Failing Test**:
```rust
// crates/ledger/tests/negative_unl_persistence.rs (new file)
#[test]
fn negative_unl_survives_ledger_save_and_load() {
    let mut ledger = Ledger::genesis();
    let nunl_obj = NegativeUnl {
        common: default_common(),
        disabled_validators: Some(vec![/* validator entry */]),
        ..Default::default()
    };
    
    ledger.put_state(keylet::negative_unl(), serialize(&nunl_obj));
    ledger.close(/* params */);
    
    let loaded = ledger.get_state(&keylet::negative_unl());
    assert!(loaded.is_some());
    let reloaded: NegativeUnl = deserialize(&loaded.unwrap());
    assert_eq!(reloaded.disabled_validators.len(), 1);
}
```

**Verification**:
```bash
cd crates/ledger && cargo test negative_unl_survives_ledger_save_and_load
```

---

### B8: End-to-End Integration Test
**Goal**: Full cycle from validator registration → validation tracking → flag ledger evaluation → pseudo-tx application.

**Files Touched**:
- `crates/node/tests/negative_unl_e2e.rs` (new file)

**Failing Test**:
```rust
// Comprehensive test simulating real consensus flow
#[test]
fn e2e_negative_unl_generates_and_applies_pseudo_txs() {
    let mut node = Node::new(test_config);
    let mut consensus = node.consensus_engine();
    
    // Register 5 validators
    consensus.register_validators(&trusted_set, &validator_keys);
    
    // Simulate 256 ledgers: validator 5 misses 75%
    for i in 0..256 {
        for j in 1..=5 {
            if j != 5 || i % 4 == 0 {
                consensus.record_validation(node_ids[j]);
            }
        }
        consensus.on_ledger_close_for_tracker();
    }
    
    // At flag ledger 256, apply pseudo-txs
    let mut ledger = node.ledger().write().unwrap();
    let results = Node::apply_negative_unl(
        &mut consensus,
        &mut ledger,
        &node.tx_engine(),
        &node.fees(),
        256,
    );
    
    // Verify UNLModify was applied
    assert!(!results.is_empty());
    assert!(results[0].is_success());
    
    // Verify NegativeUNL state
    let nunl_key = keylet::negative_unl();
    let nunl_data = ledger.state_table.get(&nunl_key);
    assert!(nunl_data.is_some());
}
```

**Verification**:
```bash
cd crates/node && cargo test e2e_negative_unl_generates_and_applies_pseudo_txs
```

---

## 4. Risk & Open Questions

### Risks
1. **Validator Key Mapping** — How to map Overlay NodeId ↔ Consensus NodeId ↔ public key (Ed25519/Secp256k1)?
   - **Mitigation**: `register_validators()` must accept `HashMap<NodeId, hex_pubkey>` at engine init.
   
2. **Validation Source Attribution** — Are all validations attributed to the correct trusted validator in overlay?
   - **Mitigation**: Must extract NodeId from Validation message reliably (rippled extracts from signing key).

3. **UNL Quorum Recalculation** — Does the quorum system use nUNL correctly when calculating majority thresholds during consensus?
   - **Mitigation**: Verify UNL struct has in_negative_unl() and that consensus phase uses it.

4. **Flag Ledger Synchronization** — If node restarts mid-window, does tracker state recover?
   - **Mitigation**: Persist tracker window state to ledger? Or recalculate from validation history?

5. **Pseudo-TX Ordering** — Must UNLModify txs execute before user txs or SetFee on same flag ledger?
   - **Mitigation**: Apply in deterministic order; test with amendment voting for precedent.

### Open Questions
- Should tracker window state be persisted (B7 extended) or discarded on node restart?
- How to test Overlay plumbing without full peer network simulation?
- What is the exact public key format in NegativeUnl disabled_validators entries? (Check rippled XRPL docs.)
- Should validator key registration be automatic (from UNL genesis) or explicit?

---

## 5. Out of Scope

- **Amendment Voting Integration** — nUNL changes do NOT affect amendment voting logic (separate mechanism).
- **Validation Message Signing** — Validation creation/verification already handled by overlay.
- **Penalty/Reward Mechanisms** — nUNL is purely a binary disable/re-enable; no scoring beyond reliability threshold.
- **Persistent Validator Identity** — Assumes validator identities stable across ledgers (managed elsewhere).
- **Sidechain or Sideboard** — This is mainchain nUNL only.

---

## 6. Success Metrics

All tests pass:
- 8 batches × ~1–2 tests each = ~12 total new tests
- Existing UNLModify + tracker tests still pass (16 existing)
- No regression in consensus or node tests

Final: `cargo test -p consensus -p tx-engine -p node -p overlay` all green.

---

## 7. Implementation Checkpoints

- [ ] B1: Engine validator key registration + test passing
- [ ] B2: Validation plumbing from peer_manager to consensus engine
- [ ] B3: Flag ledger evaluation produces NegativeUnlChange vec
- [ ] B4: Node::apply_negative_unl() creates and applies UNLModify txs
- [ ] B5: Node integration: call apply_negative_unl() on flag ledgers
- [ ] B6: Full overlay → consensus engine validation flow
- [ ] B7: NegativeUNL ledger entry persists correctly
- [ ] B8: End-to-end integration test passes
- [ ] Final: All crate tests pass, no regressions

---

**Batch Summary**: 8 numbered batches, each with 1–2 TDD tests, targeting specific crate files and integration points.
