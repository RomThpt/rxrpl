//! Hook grants authorization.
//!
//! Grants control which external accounts and hooks can access a hook's
//! state via foreign state read/write operations.

/// A single grant entry authorizing foreign state access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookGrant {
    /// If `Some`, only this account may access foreign state.
    /// If `None`, any account is authorized (subject to `hook_hash`).
    pub authorize: Option<[u8; 20]>,
    /// If `Some`, only this specific hook (by hash) may access state.
    /// If `None`, any hook on the authorized account may access state.
    pub hook_hash: Option<[u8; 32]>,
}

/// Check if a foreign state access is authorized by the hook's grants.
///
/// Returns `true` if access is allowed, `false` otherwise.
/// An empty grants list means no foreign access is permitted.
pub fn is_grant_authorized(
    grants: &[HookGrant],
    requesting_account: &[u8; 20],
    requesting_hook_hash: Option<&[u8; 32]>,
) -> bool {
    if grants.is_empty() {
        return false;
    }
    grants.iter().any(|g| {
        let account_ok = g
            .authorize
            .as_ref()
            .is_none_or(|a| a == requesting_account);
        let hash_ok = g
            .hook_hash
            .as_ref()
            .is_none_or(|h| requesting_hook_hash == Some(h));
        account_ok && hash_ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_grants_blocks_all() {
        let account = [1u8; 20];
        assert!(!is_grant_authorized(&[], &account, None));
    }

    #[test]
    fn wildcard_grant_allows_any() {
        let grant = HookGrant {
            authorize: None,
            hook_hash: None,
        };
        let account = [1u8; 20];
        assert!(is_grant_authorized(&[grant], &account, None));
    }

    #[test]
    fn account_specific_grant() {
        let authorized_account = [1u8; 20];
        let other_account = [2u8; 20];
        let grant = HookGrant {
            authorize: Some(authorized_account),
            hook_hash: None,
        };

        assert!(is_grant_authorized(&[grant.clone()], &authorized_account, None));
        assert!(!is_grant_authorized(&[grant], &other_account, None));
    }

    #[test]
    fn hook_hash_specific_grant() {
        let hook_hash = [0xAA; 32];
        let other_hash = [0xBB; 32];
        let account = [1u8; 20];
        let grant = HookGrant {
            authorize: None,
            hook_hash: Some(hook_hash),
        };

        assert!(is_grant_authorized(&[grant.clone()], &account, Some(&hook_hash)));
        assert!(!is_grant_authorized(&[grant.clone()], &account, Some(&other_hash)));
        // No hook hash provided -> does not match a hash-specific grant
        assert!(!is_grant_authorized(&[grant], &account, None));
    }

    #[test]
    fn combined_account_and_hash() {
        let account = [1u8; 20];
        let hash = [0xCC; 32];
        let grant = HookGrant {
            authorize: Some(account),
            hook_hash: Some(hash),
        };

        assert!(is_grant_authorized(&[grant.clone()], &account, Some(&hash)));
        assert!(!is_grant_authorized(&[grant.clone()], &[2u8; 20], Some(&hash)));
        assert!(!is_grant_authorized(&[grant], &account, Some(&[0xDD; 32])));
    }

    #[test]
    fn multiple_grants_any_match() {
        let grants = vec![
            HookGrant {
                authorize: Some([1u8; 20]),
                hook_hash: None,
            },
            HookGrant {
                authorize: Some([2u8; 20]),
                hook_hash: None,
            },
        ];

        assert!(is_grant_authorized(&grants, &[1u8; 20], None));
        assert!(is_grant_authorized(&grants, &[2u8; 20], None));
        assert!(!is_grant_authorized(&grants, &[3u8; 20], None));
    }
}
