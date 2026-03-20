//! Typed read/write access to ledger entries stored as JSON bytes in the SHAMap.
//!
//! Ledger entries are stored in the state map as JSON bytes. This module provides
//! convenience functions to read/write them as typed protocol structs.

use rxrpl_primitives::Hash256;
use rxrpl_protocol::ledger::{AccountRoot, LedgerObjectKind};
use rxrpl_shamap::SHAMap;

use crate::error::LedgerError;

/// Read a ledger object from the state map and deserialize it.
///
/// Returns `None` if the key is not present in the map.
pub fn read_ledger_object(
    map: &SHAMap,
    key: &Hash256,
) -> Result<Option<LedgerObjectKind>, LedgerError> {
    let Some(bytes) = map.get(key) else {
        return Ok(None);
    };
    let value = crate::sle_codec::decode_state(bytes)
        .map_err(|e| LedgerError::Codec(e.to_string()))?;
    let obj: LedgerObjectKind =
        serde_json::from_value(value).map_err(|e| LedgerError::Codec(e.to_string()))?;
    Ok(Some(obj))
}

/// Read an `AccountRoot` from the state map.
///
/// Returns `None` if the key is not present. Returns an error if the entry
/// exists but is not an `AccountRoot`.
pub fn read_account_root(map: &SHAMap, key: &Hash256) -> Result<Option<AccountRoot>, LedgerError> {
    let Some(obj) = read_ledger_object(map, key)? else {
        return Ok(None);
    };
    match obj {
        LedgerObjectKind::AccountRoot(ar) => Ok(Some(ar)),
        _ => Err(LedgerError::Codec(
            "expected AccountRoot, found different ledger entry type".into(),
        )),
    }
}

/// Serialize a ledger object to XRPL binary and write it to the state map.
pub fn write_ledger_object(
    map: &mut SHAMap,
    key: Hash256,
    obj: &LedgerObjectKind,
) -> Result<(), LedgerError> {
    let json_bytes = serde_json::to_vec(obj).map_err(|e| LedgerError::Codec(e.to_string()))?;
    let binary = crate::sle_codec::encode_sle(&json_bytes)
        .map_err(|e| LedgerError::Codec(e.to_string()))?;
    map.put(key, binary)?;
    Ok(())
}

/// Wrap an `AccountRoot` in `LedgerObjectKind` and write it to the state map.
pub fn write_account_root(
    map: &mut SHAMap,
    key: Hash256,
    account: &AccountRoot,
) -> Result<(), LedgerError> {
    let obj = LedgerObjectKind::AccountRoot(account.clone());
    write_ledger_object(map, key, &obj)
}

/// Delete a ledger object from the state map.
pub fn delete_ledger_object(map: &mut SHAMap, key: &Hash256) -> Result<(), LedgerError> {
    map.delete(key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_protocol::ledger::common::CommonLedgerFields;
    use std::str::FromStr;

    fn test_key() -> Hash256 {
        Hash256::from_str("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap()
    }

    fn sample_account_root() -> AccountRoot {
        AccountRoot {
            common: CommonLedgerFields {
                flags: Some(0),
                ..Default::default()
            },
            account: "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh".into(),
            balance: "1000000000".into(),
            sequence: 1,
            owner_count: 0,
            account_txn_id: None,
            domain: None,
            email_hash: None,
            message_key: None,
            regular_key: None,
            tick_size: None,
            transfer_rate: None,
            nftoken_minter: None,
        }
    }

    #[test]
    fn write_and_read_account_root() {
        let mut map = SHAMap::account_state();
        let key = test_key();
        let ar = sample_account_root();

        write_account_root(&mut map, key, &ar).unwrap();
        let result = read_account_root(&map, &key).unwrap().unwrap();

        assert_eq!(result.account, ar.account);
        assert_eq!(result.balance, ar.balance);
        assert_eq!(result.sequence, ar.sequence);
        assert_eq!(result.owner_count, ar.owner_count);
    }

    #[test]
    fn read_missing_key_returns_none() {
        let map = SHAMap::account_state();
        let key = test_key();

        let result = read_ledger_object(&map, &key).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn write_read_ledger_object_kind_round_trip() {
        let mut map = SHAMap::account_state();
        let key = test_key();
        let ar = sample_account_root();
        let obj = LedgerObjectKind::AccountRoot(ar.clone());

        write_ledger_object(&mut map, key, &obj).unwrap();
        let result = read_ledger_object(&map, &key).unwrap().unwrap();

        match result {
            LedgerObjectKind::AccountRoot(read_ar) => {
                assert_eq!(read_ar.account, ar.account);
                assert_eq!(read_ar.balance, ar.balance);
            }
            _ => panic!("expected AccountRoot variant"),
        }
    }
}
