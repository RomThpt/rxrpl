use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Maximum number of accepted credentials in a PermissionedDomain.
const MAX_CREDENTIALS: usize = 10;

/// Invariant: PermissionedDomain AcceptedCredentials must be valid.
///
/// - Non-empty
/// - Unique entries
/// - Sorted by (Issuer, CredentialType) lexicographically
/// - At most 10 entries
pub struct ValidPermissionedDomain;

impl ValidPermissionedDomain {
    fn credential_key(entry: &Value) -> Option<(String, String)> {
        let issuer = entry.get("Issuer").and_then(|v| v.as_str())?;
        let cred_type = entry.get("CredentialType").and_then(|v| v.as_str())?;
        Some((issuer.to_string(), cred_type.to_string()))
    }
}

impl InvariantCheck for ValidPermissionedDomain {
    fn name(&self) -> &str {
        "ValidPermissionedDomain"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.inserts.iter().chain(changes.updates.iter()) {
            let obj = match serde_json::from_slice::<Value>(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("PermissionedDomain") {
                continue;
            }

            let creds = match obj.get("AcceptedCredentials").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => {
                    return Err(format!(
                        "PermissionedDomain at {key} missing AcceptedCredentials"
                    ));
                }
            };

            if creds.is_empty() {
                return Err(format!(
                    "PermissionedDomain at {key} has empty AcceptedCredentials"
                ));
            }

            if creds.len() > MAX_CREDENTIALS {
                return Err(format!(
                    "PermissionedDomain at {key} has {} credentials (max {MAX_CREDENTIALS})",
                    creds.len()
                ));
            }

            // Check uniqueness and sort order
            let mut prev: Option<(String, String)> = None;
            for (i, entry) in creds.iter().enumerate() {
                let current = Self::credential_key(entry).ok_or_else(|| {
                    format!(
                        "PermissionedDomain at {key} entry {i} missing Issuer or CredentialType"
                    )
                })?;

                if let Some(ref p) = prev {
                    if current <= *p {
                        return Err(format!(
                            "PermissionedDomain at {key}: AcceptedCredentials not sorted/unique at index {i}"
                        ));
                    }
                }
                prev = Some(current);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;
    use serde_json::json;
    use std::collections::HashMap;

    fn empty_changes() -> SandboxChanges {
        SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: 0,
        }
    }

    fn domain_with_creds(creds: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "PermissionedDomain",
            "AcceptedCredentials": creds,
        }))
        .unwrap()
    }

    #[test]
    fn valid_single_credential_passes() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            domain_with_creds(json!([
                { "Issuer": "rA", "CredentialType": "KYC" }
            ])),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn valid_sorted_credentials_passes() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            domain_with_creds(json!([
                { "Issuer": "rA", "CredentialType": "AML" },
                { "Issuer": "rA", "CredentialType": "KYC" },
                { "Issuer": "rB", "CredentialType": "AML" },
            ])),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn empty_credentials_fails() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), domain_with_creds(json!([])));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn unsorted_credentials_fails() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            domain_with_creds(json!([
                { "Issuer": "rB", "CredentialType": "KYC" },
                { "Issuer": "rA", "CredentialType": "KYC" },
            ])),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn duplicate_credentials_fails() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            domain_with_creds(json!([
                { "Issuer": "rA", "CredentialType": "KYC" },
                { "Issuer": "rA", "CredentialType": "KYC" },
            ])),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn too_many_credentials_fails() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        let mut creds = Vec::new();
        for i in 0..11 {
            creds.push(json!({ "Issuer": format!("r{i:02}"), "CredentialType": "KYC" }));
        }
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), domain_with_creds(json!(creds)));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_permissioned_domain_ignored() {
        let check = ValidPermissionedDomain;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "AccountRoot",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
