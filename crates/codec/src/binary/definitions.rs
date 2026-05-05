use std::collections::HashMap;

use once_cell::sync::Lazy;
use serde::Deserialize;
/// Embedded definitions.json from the XRPL protocol.
const DEFINITIONS_JSON: &str = include_str!("../../data/definitions.json");

#[derive(Debug, Deserialize)]
struct RawDefinitions {
    #[serde(rename = "TYPES")]
    types: HashMap<String, i32>,
    #[serde(rename = "FIELDS")]
    fields: Vec<(String, RawFieldInfo)>,
    #[serde(rename = "TRANSACTION_TYPES")]
    transaction_types: HashMap<String, i32>,
    #[serde(rename = "LEDGER_ENTRY_TYPES")]
    ledger_entry_types: HashMap<String, i32>,
    #[serde(rename = "TRANSACTION_RESULTS")]
    transaction_results: HashMap<String, i32>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawFieldInfo {
    nth: i32,
    #[serde(rename = "isVLEncoded")]
    is_vl_encoded: bool,
    #[serde(rename = "isSerialized")]
    is_serialized: bool,
    #[serde(rename = "isSigningField")]
    is_signing_field: bool,
    #[serde(rename = "type")]
    field_type: String,
}

/// Parsed field definition with type code and field code.
#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub nth: i32,
    pub type_code: i32,
    pub field_type: String,
    pub is_vl_encoded: bool,
    pub is_serialized: bool,
    pub is_signing_field: bool,
}

impl FieldDef {
    /// Canonical sort key: (type_code, nth).
    pub fn sort_key(&self) -> (i32, i32) {
        (self.type_code, self.nth)
    }
}

/// Global definitions loaded from the embedded JSON.
pub struct Definitions {
    pub fields_by_name: HashMap<String, FieldDef>,
    pub fields_by_header: HashMap<(i32, i32), FieldDef>,
    pub type_codes: HashMap<String, i32>,
    pub transaction_types: HashMap<String, i32>,
    pub transaction_type_names: HashMap<i32, String>,
    pub ledger_entry_types: HashMap<String, i32>,
    pub ledger_entry_type_names: HashMap<i32, String>,
    pub transaction_results: HashMap<String, i32>,
}

pub static DEFINITIONS: Lazy<Definitions> = Lazy::new(|| {
    let raw: RawDefinitions =
        serde_json::from_str(DEFINITIONS_JSON).expect("valid definitions.json");

    let mut fields_by_name = HashMap::new();
    let mut fields_by_header = HashMap::new();

    for (name, info) in &raw.fields {
        let type_code = raw.types.get(&info.field_type).copied().unwrap_or(-1);
        let def = FieldDef {
            name: name.clone(),
            nth: info.nth,
            type_code,
            field_type: info.field_type.clone(),
            is_vl_encoded: info.is_vl_encoded,
            is_serialized: info.is_serialized,
            is_signing_field: info.is_signing_field,
        };
        fields_by_name.insert(name.clone(), def.clone());
        if type_code >= 0 && info.nth >= 0 {
            fields_by_header.insert((type_code, info.nth), def);
        }
    }

    let transaction_type_names: HashMap<i32, String> = raw
        .transaction_types
        .iter()
        .map(|(k, v)| (*v, k.clone()))
        .collect();

    let ledger_entry_type_names: HashMap<i32, String> = raw
        .ledger_entry_types
        .iter()
        .map(|(k, v)| (*v, k.clone()))
        .collect();

    Definitions {
        fields_by_name,
        fields_by_header,
        type_codes: raw.types,
        transaction_types: raw.transaction_types,
        transaction_type_names,
        ledger_entry_types: raw.ledger_entry_types,
        ledger_entry_type_names,
        transaction_results: raw.transaction_results,
    }
});

/// Look up a field definition by name.
pub fn get_field(name: &str) -> Option<&'static FieldDef> {
    DEFINITIONS.fields_by_name.get(name)
}

/// Look up a field definition by (type_code, field_code) header.
pub fn get_field_by_header(type_code: i32, field_code: i32) -> Option<&'static FieldDef> {
    DEFINITIONS.fields_by_header.get(&(type_code, field_code))
}

/// Look up a transaction type code by name.
pub fn get_transaction_type_code(name: &str) -> Option<i32> {
    DEFINITIONS.transaction_types.get(name).copied()
}

/// Look up a transaction type name by code.
pub fn get_transaction_type_name(code: i32) -> Option<&'static str> {
    DEFINITIONS
        .transaction_type_names
        .get(&code)
        .map(|s| s.as_str())
}

/// Look up a ledger entry type code by name.
pub fn get_ledger_entry_type_code(name: &str) -> Option<i32> {
    DEFINITIONS.ledger_entry_types.get(name).copied()
}

/// Look up a ledger entry type name by code.
pub fn get_ledger_entry_type_name(code: i32) -> Option<&'static str> {
    DEFINITIONS
        .ledger_entry_type_names
        .get(&code)
        .map(|s| s.as_str())
}

/// Look up a granular permission code by name (PermissionDelegation amendment).
/// Granular permission values live in 65537..u32::MAX so they cannot collide with
/// `txType + 1` permission encodings.
pub fn get_granular_permission_code(name: &str) -> Option<u32> {
    match name {
        "TrustlineAuthorize" => Some(65537),
        "TrustlineFreeze" => Some(65538),
        "TrustlineUnfreeze" => Some(65539),
        "AccountDomainSet" => Some(65540),
        "AccountEmailHashSet" => Some(65541),
        "AccountMessageKeySet" => Some(65542),
        "AccountTransferRateSet" => Some(65543),
        "AccountTickSizeSet" => Some(65544),
        "PaymentMint" => Some(65545),
        "PaymentBurn" => Some(65546),
        "MPTokenIssuanceLock" => Some(65547),
        "MPTokenIssuanceUnlock" => Some(65548),
        _ => None,
    }
}

/// Resolve a `PermissionValue` JSON string to its UInt32 wire encoding.
/// Either a granular permission name or a transaction type name (encoded as
/// `txType + 1`), matching rippled's STParsedJSON behavior.
pub fn resolve_permission_value(name: &str) -> Option<u32> {
    if let Some(v) = get_granular_permission_code(name) {
        return Some(v);
    }
    get_transaction_type_code(name).map(|c| (c as u32) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_load() {
        assert!(!DEFINITIONS.fields_by_name.is_empty());
        assert!(!DEFINITIONS.transaction_types.is_empty());
    }

    #[test]
    fn known_fields_exist() {
        let account = get_field("Account").unwrap();
        assert_eq!(account.field_type, "AccountID");
        assert!(account.is_serialized);

        let amount = get_field("Amount").unwrap();
        assert_eq!(amount.field_type, "Amount");
    }

    #[test]
    fn transaction_types() {
        assert_eq!(get_transaction_type_code("Payment"), Some(0));
        assert_eq!(get_transaction_type_name(0), Some("Payment"));
    }
}
