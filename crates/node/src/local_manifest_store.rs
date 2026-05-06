//! Persistence for the **local** validator manifest (B4).
//!
//! Tiny JSON sidecar at `<data_dir>/local_manifest.json`. Holds:
//!   - `sequence`: last published manifest sequence
//!   - `raw_bytes_hex`: signed STObject bytes (hex)
//!
//! On boot, Node reads this to decide the next manifest sequence —
//! always strictly greater than what we last published, so peers
//! accept the new manifest as fresh and replace the stale ephemeral.
//!
//! No schema migration concerns: we own the file format end-to-end and
//! a missing/corrupt file degrades to "use config sequence, default 1".

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedManifest {
    pub sequence: u32,
    pub raw_bytes_hex: String,
    /// Wall-clock seconds since the UNIX epoch when this manifest was last
    /// persisted. Used by `validator_info` RPC to surface the rotation age
    /// to operators. Legacy files without this field load as 0.
    #[serde(default)]
    pub last_rotated_unix: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistedManifestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

fn path_for(data_dir: &Path) -> PathBuf {
    data_dir.join("local_manifest.json")
}

/// Read the persisted manifest if it exists. Returns `Ok(None)` when the
/// file is absent (first boot); a corrupt file returns `Err`.
pub fn load(data_dir: &Path) -> Result<Option<PersistedManifest>, PersistedManifestError> {
    let path = path_for(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let parsed: PersistedManifest = serde_json::from_slice(&bytes)?;
    Ok(Some(parsed))
}

/// Write the manifest atomically (write to `.tmp`, fsync, rename).
pub fn save(data_dir: &Path, manifest: &PersistedManifest) -> Result<(), PersistedManifestError> {
    std::fs::create_dir_all(data_dir)?;
    let final_path = path_for(data_dir);
    let tmp_path = final_path.with_extension("tmp");
    let stamped = PersistedManifest {
        last_rotated_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        ..manifest.clone()
    };
    let json = serde_json::to_vec_pretty(&stamped)?;
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let m = PersistedManifest {
            sequence: 7,
            raw_bytes_hex: "deadbeef".into(),
            last_rotated_unix: 0,
        };

        save(dir.path(), &m).unwrap();
        let loaded = load(dir.path()).unwrap().expect("present after save");

        assert_eq!(loaded.sequence, m.sequence);
        assert_eq!(loaded.raw_bytes_hex, m.raw_bytes_hex);
    }

    #[test]
    fn persisted_manifest_round_trips_last_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        save(
            dir.path(),
            &PersistedManifest {
                sequence: 3,
                raw_bytes_hex: "ab".into(),
                last_rotated_unix: 0,
            },
        )
        .unwrap();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let loaded = load(dir.path()).unwrap().unwrap();
        assert!(
            loaded.last_rotated_unix >= before && loaded.last_rotated_unix <= after,
            "last_rotated_unix {} not in [{before}, {after}]",
            loaded.last_rotated_unix,
        );
    }

    #[test]
    fn persisted_manifest_reads_legacy_file_without_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("local_manifest.json"),
            br#"{"sequence":4,"raw_bytes_hex":"cd"}"#,
        )
        .unwrap();
        let loaded = load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.sequence, 4);
        assert_eq!(loaded.raw_bytes_hex, "cd");
        assert_eq!(loaded.last_rotated_unix, 0);
    }

    /// Saving twice replaces atomically (no `.tmp` left behind, no corrupt
    /// half-written file).
    #[test]
    fn save_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let m1 = PersistedManifest {
            sequence: 1,
            raw_bytes_hex: "11".into(),
            last_rotated_unix: 0,
        };
        let m2 = PersistedManifest {
            sequence: 2,
            raw_bytes_hex: "22".into(),
            last_rotated_unix: 0,
        };
        save(dir.path(), &m1).unwrap();
        save(dir.path(), &m2).unwrap();
        let loaded = load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.sequence, m2.sequence);
        assert_eq!(loaded.raw_bytes_hex, m2.raw_bytes_hex);
        assert!(
            !dir.path().join("local_manifest.tmp").exists(),
            "no leftover .tmp"
        );
    }

    #[test]
    fn corrupt_file_surfaces_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("local_manifest.json"), b"{ not json }").unwrap();
        assert!(load(dir.path()).is_err());
    }
}
