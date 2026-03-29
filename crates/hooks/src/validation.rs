//! WASM binary validation for hooks.

use wasmi::Engine;

/// Maximum allowed WASM binary size: 64 KiB.
pub const MAX_WASM_SIZE: usize = 64 * 1024;

/// Errors that can occur during WASM validation.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("wasm binary exceeds maximum size of {MAX_WASM_SIZE} bytes (got {0})")]
    TooLarge(usize),

    #[error("invalid wasm binary: {0}")]
    InvalidWasm(String),

    #[error("missing required export: hook")]
    MissingHookExport,
}

/// Validate a WASM binary for use as a hook.
///
/// Checks:
/// - Size does not exceed `MAX_WASM_SIZE` (64 KiB)
/// - Binary is a valid WASM module that can be parsed
/// - Module exports a `hook` function
pub fn validate_wasm(code: &[u8]) -> Result<(), ValidationError> {
    if code.len() > MAX_WASM_SIZE {
        return Err(ValidationError::TooLarge(code.len()));
    }

    let engine = Engine::default();
    let module = wasmi::Module::new(&engine, code)
        .map_err(|e| ValidationError::InvalidWasm(e.to_string()))?;

    // Verify the module exports a "hook" function
    let has_hook_export = module
        .exports()
        .any(|export| export.name() == "hook" && export.ty().func().is_some());

    if !has_hook_export {
        return Err(ValidationError::MissingHookExport);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid WASM module that exports a `hook` function returning i64.
    fn minimal_hook_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
                (func $hook (export "hook") (param i32) (result i64)
                    i64.const 0
                )
            )
            "#,
        )
        .expect("valid WAT")
    }

    #[test]
    fn valid_wasm_passes() {
        let wasm = minimal_hook_wasm();
        assert!(validate_wasm(&wasm).is_ok());
    }

    #[test]
    fn rejects_oversized_wasm() {
        let big = vec![0u8; MAX_WASM_SIZE + 1];
        let err = validate_wasm(&big).unwrap_err();
        assert!(matches!(err, ValidationError::TooLarge(_)));
    }

    #[test]
    fn rejects_invalid_wasm() {
        let garbage = b"not a wasm module";
        let err = validate_wasm(garbage).unwrap_err();
        assert!(matches!(err, ValidationError::InvalidWasm(_)));
    }

    #[test]
    fn rejects_missing_hook_export() {
        let wasm = wat::parse_str(
            r#"
            (module
                (func $other (export "other") (result i32)
                    i32.const 42
                )
            )
            "#,
        )
        .expect("valid WAT");
        let err = validate_wasm(&wasm).unwrap_err();
        assert!(matches!(err, ValidationError::MissingHookExport));
    }
}
