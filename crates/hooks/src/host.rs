//! Host functions exposed to WASM hooks via the wasmi linker.

use std::sync::{Arc, Mutex};

use wasmi::{Caller, Engine, Linker};

use crate::context::HookContext;

/// Gas cost per host function call.
const HOST_CALL_GAS: u64 = 100;
/// Gas cost per byte for state operations.
const STATE_BYTE_GAS: u64 = 1;

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
    linker.func_wrap("env", "accept", move |_caller: Caller<()>, code: i32| -> i64 {
        let mut hook_ctx = ctx.lock().unwrap();
        let _ = hook_ctx.consume_gas(HOST_CALL_GAS);
        code as i64
    })?;

    // rollback(code: i32) -> i64
    // Terminates hook execution with a rollback result.
    let ctx = context.clone();
    linker.func_wrap("env", "rollback", move |_caller: Caller<()>, code: i32| -> i64 {
        let mut hook_ctx = ctx.lock().unwrap();
        let _ = hook_ctx.consume_gas(HOST_CALL_GAS);
        // Negative indicates rollback
        -(code as i64)
    })?;

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
                return -1;
            }

            let memory = match caller.get_export("memory") {
                Some(wasmi::Extern::Memory(mem)) => mem,
                _ => return -1,
            };

            let data = memory.data(&caller);
            let key_start = key_ptr as usize;
            let key_end = key_start + key_len as usize;
            let val_start = val_ptr as usize;
            let val_end = val_start + val_len as usize;

            if key_end > data.len() || val_end > data.len() {
                return -1;
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
        move |mut caller: Caller<()>, key_ptr: i32, key_len: i32, val_ptr: i32, val_max: i32| -> i64 {
            let memory = match caller.get_export("memory") {
                Some(wasmi::Extern::Memory(mem)) => mem,
                _ => return -1,
            };

            // Read key from WASM memory
            let data = memory.data(&caller);
            let key_start = key_ptr as usize;
            let key_end = key_start + key_len as usize;

            if key_end > data.len() {
                return -1;
            }

            let key = data[key_start..key_end].to_vec();

            // Look up value and consume gas
            let value = {
                let mut hook_ctx = ctx.lock().unwrap();
                if hook_ctx.consume_gas(HOST_CALL_GAS).is_err() {
                    return -1;
                }
                match hook_ctx.state.get(&key) {
                    Some(v) => v.clone(),
                    None => return -1,
                }
            };

            let copy_len = value.len().min(val_max as usize);
            let val_start = val_ptr as usize;
            let val_end = val_start + copy_len;

            let data_mut = memory.data_mut(&mut caller);
            if val_end > data_mut.len() {
                return -1;
            }
            data_mut[val_start..val_end].copy_from_slice(&value[..copy_len]);
            copy_len as i64
        },
    )?;

    Ok(linker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    #[test]
    fn register_succeeds() {
        let engine = Engine::default();
        let ctx = Arc::new(Mutex::new(HookContext::new(
            Hash256::default(),
            [0u8; 20],
        )));
        let linker = register_host_functions(&engine, ctx);
        assert!(linker.is_ok());
    }
}
