//! Host functions exposed to WASM hooks via the wasmi linker.

use std::sync::{Arc, Mutex};

use wasmi::{Caller, Engine, Linker};

use crate::context::{HookContext, MAX_EMITTED_TXNS, MAX_SLOTS};

/// Gas cost per host function call.
const HOST_CALL_GAS: u64 = 100;
/// Gas cost per byte for state operations.
const STATE_BYTE_GAS: u64 = 1;
/// Gas cost for emitting a transaction.
const EMIT_GAS: u64 = 500;
/// Gas cost for slot operations.
const SLOT_GAS: u64 = 200;

// Error codes returned to WASM.
/// Memory access out of bounds.
const OUT_OF_BOUNDS: i64 = -1;
/// Invalid argument provided.
const INVALID_ARGUMENT: i64 = -2;
/// Maximum emitted transactions exceeded.
const TOO_MANY_EMITTED: i64 = -3;
/// Slot is empty (no data loaded).
const SLOT_EMPTY: i64 = -4;
/// Requested field does not exist.
const DOESNT_EXIST: i64 = -5;
/// No free slots available.
#[allow(dead_code)]
const NO_FREE_SLOTS: i64 = -6;
/// Invalid slot number.
const INVALID_SLOT: i64 = -7;
/// Gas budget exhausted.
const OUT_OF_GAS: i64 = -10;

/// Helper: get WASM memory from a caller, returning None if unavailable.
fn get_memory(caller: &Caller<()>) -> Option<wasmi::Memory> {
    match caller.get_export("memory") {
        Some(wasmi::Extern::Memory(mem)) => Some(mem),
        _ => None,
    }
}

/// Helper: write bytes to WASM memory at the given pointer.
/// Returns the number of bytes written, or OUT_OF_BOUNDS on failure.
fn write_to_wasm(caller: &mut Caller<()>, memory: wasmi::Memory, ptr: i32, data: &[u8]) -> i64 {
    let start = ptr as usize;
    let end = start + data.len();
    let mem_data = memory.data_mut(caller);
    if end > mem_data.len() {
        return OUT_OF_BOUNDS;
    }
    mem_data[start..end].copy_from_slice(data);
    data.len() as i64
}

/// Register all hook host functions into a wasmi `Linker`.
///
/// The `context` is shared via `Arc<Mutex<_>>` so that host functions
/// can mutate it during execution.
pub fn register_host_functions(
    engine: &Engine,
    context: Arc<Mutex<HookContext>>,
) -> Result<Linker<()>, wasmi::Error> {
    let mut linker = Linker::<()>::new(engine);

    // accept(code: i32) -> i64
    // Terminates hook execution with an acceptance result.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "accept",
        move |_caller: Caller<()>, code: i32| -> i64 {
            let mut hook_ctx = ctx.lock().unwrap();
            let _ = hook_ctx.consume_gas(HOST_CALL_GAS);
            code as i64
        },
    )?;

    // rollback(code: i32) -> i64
    // Terminates hook execution with a rollback result.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "rollback",
        move |_caller: Caller<()>, code: i32| -> i64 {
            let mut hook_ctx = ctx.lock().unwrap();
            let _ = hook_ctx.consume_gas(HOST_CALL_GAS);
            // Negative indicates rollback
            -(code as i64)
        },
    )?;

    // state_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i64
    // Writes a value into the hook's state map.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "state_set",
        move |caller: Caller<()>, key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32| -> i64 {
            let mut hook_ctx = ctx.lock().unwrap();
            let gas_cost = HOST_CALL_GAS + (key_len as u64 + val_len as u64) * STATE_BYTE_GAS;
            if hook_ctx.consume_gas(gas_cost).is_err() {
                return OUT_OF_GAS;
            }

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            let data = memory.data(&caller);
            let key_start = key_ptr as usize;
            let key_end = key_start + key_len as usize;
            let val_start = val_ptr as usize;
            let val_end = val_start + val_len as usize;

            if key_end > data.len() || val_end > data.len() {
                return OUT_OF_BOUNDS;
            }

            let key = data[key_start..key_end].to_vec();
            let val = data[val_start..val_end].to_vec();
            hook_ctx.state.insert(key, val);
            0
        },
    )?;

    // state_get(key_ptr: i32, key_len: i32, val_ptr: i32, val_max: i32) -> i64
    // Reads a value from the hook's state map into WASM memory.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "state_get",
        move |mut caller: Caller<()>,
              key_ptr: i32,
              key_len: i32,
              val_ptr: i32,
              val_max: i32|
              -> i64 {
            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            // Read key from WASM memory
            let data = memory.data(&caller);
            let key_start = key_ptr as usize;
            let key_end = key_start + key_len as usize;

            if key_end > data.len() {
                return OUT_OF_BOUNDS;
            }

            let key = data[key_start..key_end].to_vec();

            // Look up value and consume gas
            let value = {
                let mut hook_ctx = ctx.lock().unwrap();
                if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                    return OUT_OF_GAS;
                }
                match hook_ctx.state.get(&key) {
                    Some(v) => v.clone(),
                    None => return DOESNT_EXIST,
                }
            };

            let copy_len = value.len().min(val_max as usize);
            let val_start = val_ptr as usize;
            let val_end = val_start + copy_len;

            let data_mut = memory.data_mut(&mut caller);
            if val_end > data_mut.len() {
                return OUT_OF_BOUNDS;
            }
            data_mut[val_start..val_end].copy_from_slice(&value[..copy_len]);
            copy_len as i64
        },
    )?;

    // otxn_type() -> i64
    // Returns the transaction type code of the originating transaction.
    let ctx = context.clone();
    linker.func_wrap("env", "otxn_type", move |_caller: Caller<()>| -> i64 {
        let mut hook_ctx = ctx.lock().unwrap();
        if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
            return OUT_OF_GAS;
        }
        hook_ctx.otxn_type as i64
    })?;

    // otxn_hash(write_ptr: i32) -> i64
    // Writes the 32-byte originating transaction hash to WASM memory.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "otxn_hash",
        move |mut caller: Caller<()>, write_ptr: i32| -> i64 {
            let hash_bytes = {
                let mut hook_ctx = ctx.lock().unwrap();
                if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                    return OUT_OF_GAS;
                }
                hook_ctx.tx_hash.as_bytes().to_vec()
            };

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            write_to_wasm(&mut caller, memory, write_ptr, &hash_bytes)
        },
    )?;

    // otxn_account(write_ptr: i32) -> i64
    // Writes the 20-byte originating transaction account to WASM memory.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "otxn_account",
        move |mut caller: Caller<()>, write_ptr: i32| -> i64 {
            let account = {
                let mut hook_ctx = ctx.lock().unwrap();
                if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                    return OUT_OF_GAS;
                }
                hook_ctx.otxn_account
            };

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            write_to_wasm(&mut caller, memory, write_ptr, &account)
        },
    )?;

    // otxn_amount(write_ptr: i32) -> i64
    // Writes the 8-byte amount (in drops) to WASM memory as big-endian i64.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "otxn_amount",
        move |mut caller: Caller<()>, write_ptr: i32| -> i64 {
            let amount = {
                let mut hook_ctx = ctx.lock().unwrap();
                if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                    return OUT_OF_GAS;
                }
                hook_ctx.otxn_amount
            };

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            let bytes = amount.to_be_bytes();
            write_to_wasm(&mut caller, memory, write_ptr, &bytes)
        },
    )?;

    // otxn_field(field_id: i32, write_ptr: i32, write_len: i32) -> i64
    // Reads an arbitrary field from the originating transaction into WASM memory.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "otxn_field",
        move |mut caller: Caller<()>, field_id: i32, write_ptr: i32, write_len: i32| -> i64 {
            if field_id < 0 || write_len < 0 {
                return INVALID_ARGUMENT;
            }

            let value = {
                let mut hook_ctx = ctx.lock().unwrap();
                if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                    return OUT_OF_GAS;
                }
                match hook_ctx.otxn_fields.get(&(field_id as u32)) {
                    Some(v) => v.clone(),
                    None => return DOESNT_EXIST,
                }
            };

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            let copy_len = value.len().min(write_len as usize);
            write_to_wasm(&mut caller, memory, write_ptr, &value[..copy_len])
        },
    )?;

    // slot(slot_no: i32, keylet_ptr: i32, keylet_len: i32) -> i64
    // Loads a ledger entry (identified by keylet bytes) into a slot.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "slot",
        move |caller: Caller<()>, slot_no: i32, keylet_ptr: i32, keylet_len: i32| -> i64 {
            if slot_no < 0 || (slot_no as usize) >= MAX_SLOTS {
                return INVALID_SLOT;
            }
            if keylet_len < 0 {
                return INVALID_ARGUMENT;
            }

            let mut hook_ctx = ctx.lock().unwrap();
            if hook_ctx.consume_gas(SLOT_GAS).is_err() {
                return OUT_OF_GAS;
            }

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            let data = memory.data(&caller);
            let start = keylet_ptr as usize;
            let end = start + keylet_len as usize;
            if end > data.len() {
                return OUT_OF_BOUNDS;
            }

            // Store the keylet data as the slot content.
            // In a full implementation this would look up the ledger entry;
            // here we store the raw keylet bytes as a placeholder.
            let keylet_data = data[start..end].to_vec();
            hook_ctx.slot_data[slot_no as usize] = Some(keylet_data);
            slot_no as i64
        },
    )?;

    // slot_subfield(parent_slot: i32, field_id: i32, new_slot: i32) -> i64
    // Extracts a subfield from a parent slot into a new slot.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "slot_subfield",
        move |_caller: Caller<()>, parent_slot: i32, field_id: i32, new_slot: i32| -> i64 {
            if parent_slot < 0
                || (parent_slot as usize) >= MAX_SLOTS
                || new_slot < 0
                || (new_slot as usize) >= MAX_SLOTS
            {
                return INVALID_SLOT;
            }
            if field_id < 0 {
                return INVALID_ARGUMENT;
            }

            let mut hook_ctx = ctx.lock().unwrap();
            if hook_ctx.consume_gas(SLOT_GAS).is_err() {
                return OUT_OF_GAS;
            }

            let parent_data = match &hook_ctx.slot_data[parent_slot as usize] {
                Some(d) => d.clone(),
                None => return SLOT_EMPTY,
            };

            // In a full implementation, this would parse the serialized object
            // and extract the subfield. For now, we store the parent data
            // tagged with the field_id as a simple representation.
            let field_tag = (field_id as u32).to_be_bytes();
            let mut subfield_data = field_tag.to_vec();
            subfield_data.extend_from_slice(&parent_data);
            hook_ctx.slot_data[new_slot as usize] = Some(subfield_data);
            new_slot as i64
        },
    )?;

    // slot_size(slot_no: i32) -> i64
    // Returns the size in bytes of the data in a slot.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "slot_size",
        move |_caller: Caller<()>, slot_no: i32| -> i64 {
            if slot_no < 0 || (slot_no as usize) >= MAX_SLOTS {
                return INVALID_SLOT;
            }

            let mut hook_ctx = ctx.lock().unwrap();
            if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                return OUT_OF_GAS;
            }

            match &hook_ctx.slot_data[slot_no as usize] {
                Some(d) => d.len() as i64,
                None => SLOT_EMPTY,
            }
        },
    )?;

    // slot_type(slot_no: i32, flags: i32) -> i64
    // Returns type information about the data in a slot.
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "slot_type",
        move |_caller: Caller<()>, slot_no: i32, _flags: i32| -> i64 {
            if slot_no < 0 || (slot_no as usize) >= MAX_SLOTS {
                return INVALID_SLOT;
            }

            let mut hook_ctx = ctx.lock().unwrap();
            if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                return OUT_OF_GAS;
            }

            match &hook_ctx.slot_data[slot_no as usize] {
                Some(d) => {
                    // Return the length as a simple type indicator.
                    // A full implementation would parse the serialized type.
                    d.len() as i64
                }
                None => SLOT_EMPTY,
            }
        },
    )?;

    // emit(write_ptr: i32, write_len: i32, read_ptr: i32, read_len: i32) -> i64
    // Emits a transaction from the hook. The emitted transaction is read from
    // WASM memory at [read_ptr..read_ptr+read_len]. The 32-byte emission hash
    // is written to [write_ptr..write_ptr+write_len].
    let ctx = context.clone();
    linker.func_wrap(
        "env",
        "emit",
        move |mut caller: Caller<()>,
              write_ptr: i32,
              write_len: i32,
              read_ptr: i32,
              read_len: i32|
              -> i64 {
            if read_len < 0 || write_len < 0 {
                return INVALID_ARGUMENT;
            }

            let mut hook_ctx = ctx.lock().unwrap();
            if hook_ctx.consume_gas(EMIT_GAS).is_err() {
                return OUT_OF_GAS;
            }

            if hook_ctx.emitted_txns.len() >= MAX_EMITTED_TXNS {
                return TOO_MANY_EMITTED;
            }

            let memory = match get_memory(&caller) {
                Some(mem) => mem,
                None => return OUT_OF_BOUNDS,
            };

            // Read the emitted transaction from WASM memory
            let data = memory.data(&caller);
            let r_start = read_ptr as usize;
            let r_end = r_start + read_len as usize;
            if r_end > data.len() {
                return OUT_OF_BOUNDS;
            }

            let emitted_tx = data[r_start..r_end].to_vec();

            // Compute a simple hash of the emitted tx for the emission hash.
            // A full implementation would use SHA-512 half.
            let mut emission_hash = [0u8; 32];
            for (i, byte) in emitted_tx.iter().enumerate() {
                emission_hash[i % 32] ^= byte;
            }

            hook_ctx.emitted_txns.push(emitted_tx);
            drop(hook_ctx);

            // Write emission hash to WASM memory
            let copy_len = 32.min(write_len as usize);
            write_to_wasm(&mut caller, memory, write_ptr, &emission_hash[..copy_len])
        },
    )?;

    Ok(linker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    fn make_context() -> Arc<Mutex<HookContext>> {
        Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])))
    }

    #[test]
    fn register_succeeds() {
        let engine = Engine::default();
        let ctx = make_context();
        let linker = register_host_functions(&engine, ctx);
        assert!(linker.is_ok());
    }

    #[test]
    fn otxn_type_returns_type_code() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        ctx.lock().unwrap().otxn_type = 42;

        // Build a WASM module that calls otxn_type and returns the result
        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_type" (func $otxn_type (result i64)))
                (func $hook (export "hook") (param i32) (result i64)
                    call $otxn_type
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn otxn_hash_writes_to_memory() {
        let engine = Engine::default();
        let mut hash_bytes = [0u8; 32];
        hash_bytes[0] = 0xAA;
        hash_bytes[31] = 0xBB;
        let hash = Hash256::from(hash_bytes);

        let ctx = Arc::new(Mutex::new(HookContext::new(hash, [0u8; 20])));

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_hash" (func $otxn_hash (param i32) (result i64)))
                (memory (export "memory") 1)
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    call $otxn_hash
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 32); // 32 bytes written

        // Verify memory contents
        let memory = instance.get_memory(&store, "memory").unwrap();
        let data = memory.data(&store);
        assert_eq!(data[0], 0xAA);
        assert_eq!(data[31], 0xBB);
    }

    #[test]
    fn otxn_account_writes_to_memory() {
        let engine = Engine::default();
        let mut acct = [0u8; 20];
        acct[0] = 0x11;
        acct[19] = 0x22;
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        ctx.lock().unwrap().otxn_account = acct;

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_account" (func $otxn_account (param i32) (result i64)))
                (memory (export "memory") 1)
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    call $otxn_account
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 20);

        let memory = instance.get_memory(&store, "memory").unwrap();
        let data = memory.data(&store);
        assert_eq!(data[0], 0x11);
        assert_eq!(data[19], 0x22);
    }

    #[test]
    fn otxn_amount_writes_to_memory() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        ctx.lock().unwrap().otxn_amount = 1_000_000;

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_amount" (func $otxn_amount (param i32) (result i64)))
                (memory (export "memory") 1)
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    call $otxn_amount
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 8); // 8 bytes written

        let memory = instance.get_memory(&store, "memory").unwrap();
        let data = memory.data(&store);
        let amount = i64::from_be_bytes(data[0..8].try_into().unwrap());
        assert_eq!(amount, 1_000_000);
    }

    #[test]
    fn otxn_field_returns_data() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        ctx.lock()
            .unwrap()
            .otxn_fields
            .insert(100, vec![0xDE, 0xAD]);

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_field" (func $otxn_field (param i32 i32 i32) (result i64)))
                (memory (export "memory") 1)
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 100
                    i32.const 0
                    i32.const 64
                    call $otxn_field
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 2); // 2 bytes written

        let memory = instance.get_memory(&store, "memory").unwrap();
        let data = memory.data(&store);
        assert_eq!(data[0], 0xDE);
        assert_eq!(data[1], 0xAD);
    }

    #[test]
    fn otxn_field_missing_returns_doesnt_exist() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_field" (func $otxn_field (param i32 i32 i32) (result i64)))
                (memory (export "memory") 1)
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 999
                    i32.const 0
                    i32.const 64
                    call $otxn_field
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, DOESNT_EXIST);
    }

    #[test]
    fn slot_and_slot_size() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));

        // WASM: write 4 bytes to memory, call slot(0, 0, 4), then call slot_size(0)
        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "slot" (func $slot (param i32 i32 i32) (result i64)))
                (import "env" "slot_size" (func $slot_size (param i32) (result i64)))
                (memory (export "memory") 1)
                (data (i32.const 0) "\DE\AD\BE\EF")
                (func $hook (export "hook") (param i32) (result i64)
                    ;; Load 4 bytes into slot 0
                    i32.const 0
                    i32.const 0
                    i32.const 4
                    call $slot
                    drop
                    ;; Get size of slot 0
                    i32.const 0
                    call $slot_size
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 4); // slot contains 4 bytes
    }

    #[test]
    fn slot_size_empty_returns_error() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "slot_size" (func $slot_size (param i32) (result i64)))
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    call $slot_size
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, SLOT_EMPTY);
    }

    #[test]
    fn slot_invalid_number_returns_error() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "slot_size" (func $slot_size (param i32) (result i64)))
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 99
                    call $slot_size
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, INVALID_SLOT);
    }

    #[test]
    fn slot_subfield_works() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        // Pre-load slot 0 with some data
        ctx.lock().unwrap().slot_data[0] = Some(vec![0x01, 0x02, 0x03]);

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "slot_subfield" (func $slot_subfield (param i32 i32 i32) (result i64)))
                (import "env" "slot_size" (func $slot_size (param i32) (result i64)))
                (func $hook (export "hook") (param i32) (result i64)
                    ;; Extract subfield from slot 0, field_id=5, into slot 1
                    i32.const 0
                    i32.const 5
                    i32.const 1
                    call $slot_subfield
                    drop
                    ;; Return size of slot 1
                    i32.const 1
                    call $slot_size
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        // 4 bytes (field_id tag) + 3 bytes (parent data) = 7
        assert_eq!(result, 7);
    }

    #[test]
    fn emit_stores_transaction() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));

        // WASM: write some tx data to memory, call emit
        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "emit" (func $emit (param i32 i32 i32 i32) (result i64)))
                (memory (export "memory") 1)
                (data (i32.const 100) "\AA\BB\CC\DD\EE")
                (func $hook (export "hook") (param i32) (result i64)
                    ;; emit(write_ptr=0, write_len=32, read_ptr=100, read_len=5)
                    i32.const 0
                    i32.const 32
                    i32.const 100
                    i32.const 5
                    call $emit
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx.clone()).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        // Should return 32 (bytes of hash written)
        assert_eq!(result, 32);

        // Verify emitted transaction was stored
        let hook_ctx = ctx.lock().unwrap();
        assert_eq!(hook_ctx.emitted_txns.len(), 1);
        assert_eq!(hook_ctx.emitted_txns[0], vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    }

    #[test]
    fn emit_respects_max_limit() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        // Fill up emitted txns to the limit
        {
            let mut hook_ctx = ctx.lock().unwrap();
            hook_ctx.gas_remaining = u64::MAX; // plenty of gas
            for _ in 0..MAX_EMITTED_TXNS {
                hook_ctx.emitted_txns.push(vec![0x00]);
            }
        }

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "emit" (func $emit (param i32 i32 i32 i32) (result i64)))
                (memory (export "memory") 1)
                (data (i32.const 100) "\AA")
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    i32.const 32
                    i32.const 100
                    i32.const 1
                    call $emit
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, TOO_MANY_EMITTED);
    }

    #[test]
    fn gas_metering_for_new_functions() {
        let engine = Engine::default();
        // Give very little gas
        let ctx = Arc::new(Mutex::new(HookContext::with_gas(
            Hash256::default(),
            [0u8; 20],
            50, // Less than HOST_CALL_GAS (100)
        )));

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "otxn_type" (func $otxn_type (result i64)))
                (func $hook (export "hook") (param i32) (result i64)
                    call $otxn_type
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, OUT_OF_GAS);
    }

    #[test]
    fn slot_gas_cost_is_higher() {
        let engine = Engine::default();
        // Give enough for HOST_CALL_GAS but not SLOT_GAS
        let ctx = Arc::new(Mutex::new(HookContext::with_gas(
            Hash256::default(),
            [0u8; 20],
            150, // > HOST_CALL_GAS (100) but < SLOT_GAS (200)
        )));

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "slot" (func $slot (param i32 i32 i32) (result i64)))
                (memory (export "memory") 1)
                (data (i32.const 0) "\AA\BB")
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    i32.const 0
                    i32.const 2
                    call $slot
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, OUT_OF_GAS);
    }

    #[test]
    fn slot_type_returns_data() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(Hash256::default(), [0u8; 20])));
        ctx.lock().unwrap().slot_data[3] = Some(vec![0x01, 0x02, 0x03, 0x04, 0x05]);

        let wasm = wat::parse_str(
            r#"
            (module
                (import "env" "slot_type" (func $slot_type (param i32 i32) (result i64)))
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 3
                    i32.const 0
                    call $slot_type
                )
            )
            "#,
        )
        .expect("valid WAT");

        let module = wasmi::Module::new(&engine, &wasm).unwrap();
        let linker = register_host_functions(&engine, ctx).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        let hook_fn = instance.get_typed_func::<i32, i64>(&store, "hook").unwrap();
        let result = hook_fn.call(&mut store, 0).unwrap();
        assert_eq!(result, 5); // length of data in slot
    }
}
