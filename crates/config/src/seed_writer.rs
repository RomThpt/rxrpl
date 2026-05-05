//! Validator seed file writer.
//!
//! Creates a new seed file at the given path with strict Unix permissions
//! (`0o600`). The file is created with `O_EXCL` so an existing file at the
//! target path causes a hard failure rather than a silent overwrite.
//!
//! On Unix the file is opened with mode `0o600` directly via
//! [`std::os::unix::fs::OpenOptionsExt::mode`], so the permissions are set
//! atomically by the kernel — no umask race window.
//!
//! On non-Unix platforms a warning is logged; operators must secure the file
//! with platform-native ACLs.
//!
//! Contents written: 32 lowercase hex characters followed by a single `\n`,
//! matching the format accepted by [`crate::seed_loader::load_seed_file`].

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use rxrpl_crypto::Seed;

use crate::error::ConfigError;

const SEED_HEX_LEN: usize = 32;

/// Atomically create `path` with mode `0o600` and write `seed` as 32 hex chars.
///
/// Fails (does not overwrite) if the path already exists.
pub fn write_seed_file(path: &Path, seed: &Seed) -> Result<(), ConfigError> {
    let mut hex = [0u8; SEED_HEX_LEN + 1];
    encode_hex(seed.as_bytes(), &mut hex[..SEED_HEX_LEN]);
    hex[SEED_HEX_LEN] = b'\n';

    let mut file = open_exclusive(path)?;
    let write_res = file.write_all(&hex).and_then(|_| file.sync_all());

    // Best-effort scrub of the temporary hex buffer.
    for b in hex.iter_mut() {
        *b = 0;
    }

    write_res.map_err(|err| ConfigError::SeedFileUnreadable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;

    verify_mode(path)?;
    Ok(())
}

#[cfg(unix)]
fn open_exclusive(path: &Path) -> Result<std::fs::File, ConfigError> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| ConfigError::SeedFileUnreadable {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })
}

#[cfg(not(unix))]
fn open_exclusive(path: &Path) -> Result<std::fs::File, ConfigError> {
    tracing::warn!(
        path = %path.display(),
        "validator seed file permissions cannot be enforced on this platform; \
         secure the file with platform-native ACLs"
    );
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| ConfigError::SeedFileUnreadable {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })
}

#[cfg(unix)]
fn verify_mode(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|err| ConfigError::SeedFileUnreadable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;
    let mode = meta.mode() & 0o777;
    if mode != 0o600 {
        return Err(ConfigError::SeedFilePermissionDenied {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_mode(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

fn encode_hex(bytes: &[u8], out: &mut [u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    debug_assert_eq!(out.len(), bytes.len() * 2);
    for (i, b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed_loader::load_seed_file;

    #[cfg(unix)]
    #[test]
    fn write_then_read_round_trips() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed");
        let seed = Seed::from_bytes([0x5au8; 16]);

        write_seed_file(&path, &seed).unwrap();

        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "seed file must be created with mode 0o600");

        let loaded = load_seed_file(&path).unwrap();
        assert_eq!(loaded.as_bytes(), seed.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_to_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed");
        std::fs::write(&path, b"existing").unwrap();

        let seed = Seed::from_bytes([0u8; 16]);
        let err = write_seed_file(&path, &seed).unwrap_err();
        assert!(
            matches!(err, ConfigError::SeedFileUnreadable { .. }),
            "got {err:?}"
        );
        // Original contents must be preserved.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after, b"existing");
    }
}
