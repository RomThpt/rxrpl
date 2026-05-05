//! Validator seed file loader with strict Unix permissions enforcement.
//!
//! On Unix the file is required to have mode `0o600` (owner read/write only).
//! Any group/world bit causes loading to fail with
//! [`ConfigError::SeedFilePermissionDenied`].
//!
//! Symlinks are refused outright to avoid redirection attacks.
//!
//! TOCTOU mitigation: the file is opened first, then permissions are read
//! from the open handle (`fstat`) so an attacker cannot swap modes between
//! the check and the read.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rxrpl_crypto::Seed;

use crate::error::ConfigError;

const REQUIRED_MODE: u32 = 0o600;
const MODE_MASK: u32 = 0o777;

/// Load a validator seed from `path`, enforcing strict permissions.
///
/// Accepted file contents (after trimming trailing whitespace/newlines):
/// - Exactly 32 hex characters (16 bytes), or
/// - Exactly 16 raw binary bytes.
pub fn load_seed_file(path: &Path) -> Result<Seed, ConfigError> {
    // Refuse symlinks before opening, to avoid following an attacker-controlled
    // redirection to a world-readable file.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(ConfigError::SeedFileUnreadable {
                path: path.to_path_buf(),
                reason: "path is a symlink; refusing to follow".into(),
            });
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::SeedFileNotFound(path.to_path_buf()));
        }
        Err(err) => {
            return Err(ConfigError::SeedFileUnreadable {
                path: path.to_path_buf(),
                reason: err.to_string(),
            });
        }
    }

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::SeedFileNotFound(path.to_path_buf()));
        }
        Err(err) => {
            return Err(ConfigError::SeedFileUnreadable {
                path: path.to_path_buf(),
                reason: err.to_string(),
            });
        }
    };

    enforce_permissions(&file, path)?;

    let mut buf = Vec::with_capacity(64);
    file.read_to_end(&mut buf)
        .map_err(|err| ConfigError::SeedFileUnreadable {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;

    let seed = parse_seed_bytes(path, &buf)?;
    // Best-effort scrub of the on-stack buffer.
    for b in buf.iter_mut() {
        *b = 0;
    }
    Ok(seed)
}

#[cfg(unix)]
fn enforce_permissions(file: &File, path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt;

    let meta = file
        .metadata()
        .map_err(|err| ConfigError::SeedFileUnreadable {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
    let mode = meta.mode() & MODE_MASK;
    if mode != REQUIRED_MODE {
        return Err(ConfigError::SeedFilePermissionDenied {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_permissions(_file: &File, path: &Path) -> Result<(), ConfigError> {
    tracing::warn!(
        path = %path.display(),
        "validator seed file permissions cannot be enforced on this platform; \
         secure the file with platform-native ACLs"
    );
    Ok(())
}

fn parse_seed_bytes(path: &Path, buf: &[u8]) -> Result<Seed, ConfigError> {
    let trimmed = trim_ascii_whitespace(buf);

    if trimmed.len() == 16 {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(trimmed);
        return Ok(Seed::from_bytes(bytes));
    }

    if trimmed.len() == 32 && trimmed.iter().all(|b| b.is_ascii_hexdigit()) {
        let hex = std::str::from_utf8(trimmed).map_err(|err| {
            ConfigError::SeedFileInvalidContents(path.to_path_buf(), err.to_string())
        })?;
        let mut bytes = [0u8; 16];
        for i in 0..16 {
            bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|err| {
                ConfigError::SeedFileInvalidContents(path.to_path_buf(), err.to_string())
            })?;
        }
        return Ok(Seed::from_bytes(bytes));
    }

    Err(ConfigError::SeedFileInvalidContents(
        path.to_path_buf(),
        format!(
            "expected 16 raw bytes or 32 hex characters, got {} bytes",
            trimmed.len()
        ),
    ))
}

fn trim_ascii_whitespace(buf: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = buf.len();
    while start < end && buf[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && buf[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &buf[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn write_temp(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed");
        let mut f = File::create(&path).unwrap();
        f.write_all(contents).unwrap();
        f.sync_all().unwrap();
        (dir, path)
    }

    #[test]
    fn missing_file_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist");
        let err = load_seed_file(&path).unwrap_err();
        matches!(err, ConfigError::SeedFileNotFound(_));
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_rejected() {
        let (_dir, path) = write_temp(b"00112233445566778899aabbccddeeff\n");
        chmod(&path, 0o644);
        let err = load_seed_file(&path).unwrap_err();
        match err {
            ConfigError::SeedFilePermissionDenied { mode, .. } => {
                assert_eq!(mode, 0o644);
            }
            other => panic!("expected SeedFilePermissionDenied, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_is_rejected() {
        let (_dir, path) = write_temp(b"00112233445566778899aabbccddeeff");
        chmod(&path, 0o640);
        let err = load_seed_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::SeedFilePermissionDenied { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn mode_0600_hex_seed_loads() {
        let (_dir, path) = write_temp(b"00112233445566778899aabbccddeeff\n");
        chmod(&path, 0o600);
        let seed = load_seed_file(&path).unwrap();
        assert_eq!(
            seed.as_bytes(),
            &[
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn mode_0600_raw_16_byte_seed_loads() {
        let raw = [0xa5u8; 16];
        let (_dir, path) = write_temp(&raw);
        chmod(&path, 0o600);
        let seed = load_seed_file(&path).unwrap();
        assert_eq!(seed.as_bytes(), &raw);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_contents_rejected() {
        let (_dir, path) = write_temp(b"not a seed");
        chmod(&path, 0o600);
        let err = load_seed_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::SeedFileInvalidContents(_, _)));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        let mut f = File::create(&target).unwrap();
        f.write_all(b"00112233445566778899aabbccddeeff").unwrap();
        chmod(&target, 0o600);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = load_seed_file(&link).unwrap_err();
        assert!(matches!(err, ConfigError::SeedFileUnreadable { .. }));
    }
}
