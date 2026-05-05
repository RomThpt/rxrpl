use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_ledger::Ledger;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Decode raw state bytes (binary or JSON) to a JSON Value.
///
/// Handles both XRPL binary format and legacy JSON format.
pub fn decode_state_value(data: &[u8]) -> Result<Value, RpcServerError> {
    rxrpl_ledger::sle_codec::decode_state(data)
        .map_err(|e| RpcServerError::Internal(format!("failed to decode state: {e}")))
}

/// Read a state entry from the ledger and decode it as JSON.
pub fn read_state_as_json(ledger: &Ledger, key: &Hash256) -> Result<Option<Value>, RpcServerError> {
    let Some(data) = ledger.get_state(key) else {
        return Ok(None);
    };
    decode_state_value(data).map(Some)
}

/// Result type for paginated directory walks.
pub type WalkResult = Result<(Vec<(Hash256, Value)>, Option<String>), RpcServerError>;

/// Resolved ledger reference for RPC handlers.
pub enum LedgerRef<'a> {
    /// Read guard on the current open ledger.
    Current(tokio::sync::RwLockReadGuard<'a, Ledger>),
    /// Clone of a closed/validated ledger.
    Closed(Box<Ledger>),
}

impl<'a> std::ops::Deref for LedgerRef<'a> {
    type Target = Ledger;
    fn deref(&self) -> &Ledger {
        match self {
            LedgerRef::Current(guard) => guard,
            LedgerRef::Closed(ledger) => ledger.as_ref(),
        }
    }
}

/// Resolve which ledger to use based on `ledger_index` param.
///
/// Supports: `"current"` (default), `"closed"`, `"validated"`, or a numeric index.
pub async fn resolve_ledger<'a>(
    params: &Value,
    ctx: &'a Arc<ServerContext>,
) -> Result<LedgerRef<'a>, RpcServerError> {
    let ledger_index = params
        .get("ledger_index")
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| "current".to_string());

    match ledger_index.as_str() {
        "current" => {
            let ledger = ctx
                .ledger
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;
            Ok(LedgerRef::Current(ledger.read().await))
        }
        "closed" | "validated" => {
            let closed = ctx
                .closed_ledgers
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;
            let closed = closed.read().await;
            let ledger = closed
                .back()
                .ok_or_else(|| RpcServerError::Internal("no closed ledger yet".into()))?;
            Ok(LedgerRef::Closed(Box::new(ledger.clone())))
        }
        index => {
            let seq: u32 = index.parse().map_err(|_| {
                RpcServerError::InvalidParams(format!("invalid ledger_index: {index}"))
            })?;

            // Check current open ledger
            if let Some(ref l) = ctx.ledger {
                let l = l.read().await;
                if l.header.sequence == seq {
                    return Ok(LedgerRef::Current(l));
                }
            }

            // Search closed ledgers
            let closed = ctx
                .closed_ledgers
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;
            let closed = closed.read().await;
            let ledger = closed
                .iter()
                .find(|l| l.header.sequence == seq)
                .ok_or_else(|| RpcServerError::InvalidParams(format!("ledger {seq} not found")))?;
            Ok(LedgerRef::Closed(Box::new(ledger.clone())))
        }
    }
}

/// Extract and decode the `account` field from params.
pub fn require_account_id(params: &Value) -> Result<AccountId, RpcServerError> {
    let account = params
        .get("account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'account' field".into()))?;

    decode_account_id(account).map_err(|_| RpcServerError::AccountMalformed)
}

/// Walk an account's owner directory, returning ledger entries with pagination.
///
/// Returns `(entries, next_marker)` where each entry is `(Hash256, Value)`.
pub fn walk_owner_directory(
    ledger: &Ledger,
    account_id: &AccountId,
    limit: usize,
    marker: Option<&str>,
) -> WalkResult {
    let root = keylet::owner_dir(account_id);

    let dir_data = match ledger.get_state(&root) {
        Some(data) => data,
        None => return Ok((Vec::new(), None)),
    };

    let marker_hash = if let Some(m) = marker {
        Some(
            Hash256::from_str(m)
                .map_err(|e| RpcServerError::InvalidParams(format!("invalid marker: {e}")))?,
        )
    } else {
        None
    };

    let mut entries: Vec<(Hash256, Value)> = Vec::new();
    let mut next_marker = None;
    let mut found_marker = marker_hash.is_none();
    let mut page = 0u64;

    loop {
        let page_key = keylet::dir_node(&root, page);
        let page_data = if page == 0 {
            Some(dir_data)
        } else {
            ledger.get_state(&page_key)
        };

        let page_json: Value = if let Some(data) = page_data {
            decode_state_value(data)?
        } else {
            break;
        };

        if let Some(indexes) = page_json.get("Indexes").and_then(|v| v.as_array()) {
            for idx_val in indexes {
                let idx_str = idx_val
                    .as_str()
                    .ok_or_else(|| RpcServerError::Internal("invalid index in directory".into()))?;
                let idx_hash = Hash256::from_str(idx_str).map_err(|e| {
                    RpcServerError::Internal(format!("invalid hash in directory: {e}"))
                })?;

                if !found_marker {
                    if idx_hash == marker_hash.unwrap() {
                        found_marker = true;
                    }
                    continue;
                }

                if entries.len() >= limit {
                    next_marker = Some(entries.last().unwrap().0.to_string());
                    return Ok((entries, next_marker));
                }

                if let Some(entry_data) = ledger.get_state(&idx_hash) {
                    let entry: Value = decode_state_value(entry_data)?;
                    entries.push((idx_hash, entry));
                }
            }
        }

        // Check for next page
        match page_json.get("IndexNext").and_then(|v| v.as_u64()) {
            Some(next) if next != 0 => page = next,
            _ => break,
        }
    }

    Ok((entries, next_marker))
}

/// Parse a currency/issuer pair from a JSON value.
///
/// Returns `(currency_bytes, issuer_account_id)`.
pub fn parse_currency_issuer(val: &Value) -> Result<([u8; 20], AccountId), RpcServerError> {
    let currency = val
        .get("currency")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'currency' field".into()))?;

    let mut currency_bytes = [0u8; 20];
    if currency == "XRP" {
        // XRP is all zeros
    } else if currency.len() == 3 {
        // Standard currency code: 3 ASCII chars at offset 12
        currency_bytes[12] = currency.as_bytes()[0];
        currency_bytes[13] = currency.as_bytes()[1];
        currency_bytes[14] = currency.as_bytes()[2];
    } else if currency.len() == 40 {
        // Hex currency code
        let decoded = hex::decode(currency)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid currency hex: {e}")))?;
        currency_bytes.copy_from_slice(&decoded);
    } else {
        return Err(RpcServerError::InvalidParams(
            "invalid currency format".into(),
        ));
    }

    let issuer = if currency == "XRP" {
        AccountId([0u8; 20])
    } else {
        let issuer_str = val.get("issuer").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcServerError::InvalidParams("missing 'issuer' for non-XRP currency".into())
        })?;
        decode_account_id(issuer_str)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid issuer: {e}")))?
    };

    Ok((currency_bytes, issuer))
}

/// Helper to derive a seed + key_type from common secret params.
///
/// Supports `secret` (passphrase), `seed` (encoded seed string), `seed_hex`, `passphrase`.
pub fn derive_seed_from_params(
    params: &Value,
) -> Result<(rxrpl_crypto::Seed, rxrpl_crypto::KeyType), RpcServerError> {
    let key_type_str = params
        .get("key_type")
        .and_then(|v| v.as_str())
        .unwrap_or("secp256k1");

    let key_type = match key_type_str {
        "secp256k1" => rxrpl_crypto::KeyType::Secp256k1,
        "ed25519" => rxrpl_crypto::KeyType::Ed25519,
        _ => {
            return Err(RpcServerError::InvalidParams(format!(
                "invalid key_type: {key_type_str}"
            )));
        }
    };

    // rippled rejects requests carrying more than one of secret/seed/seed_hex/passphrase.
    let provided_secret_kinds = ["secret", "seed", "seed_hex", "passphrase"]
        .iter()
        .filter(|k| {
            params
                .get(**k)
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
        })
        .count();
    if provided_secret_kinds > 1 {
        return Err(RpcServerError::InvalidParams(
            "Cannot specify more than one of 'secret', 'seed', 'seed_hex', or 'passphrase'.".into(),
        ));
    }

    // Try encoded seed first
    if let Some(seed_str) = params.get("seed").and_then(|v| v.as_str()) {
        let (entropy, _seed_kt) = rxrpl_codec::address::seed::decode_seed(seed_str)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid seed: {e}")))?;
        return Ok((rxrpl_crypto::Seed::from_bytes(entropy), key_type));
    }

    // Try seed_hex
    if let Some(seed_hex) = params.get("seed_hex").and_then(|v| v.as_str()) {
        let bytes = hex::decode(seed_hex)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid seed_hex: {e}")))?;
        if bytes.len() != 16 {
            return Err(RpcServerError::InvalidParams(
                "seed_hex must be 16 bytes".into(),
            ));
        }
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&bytes);
        return Ok((rxrpl_crypto::Seed::from_bytes(arr), key_type));
    }

    // `secret` is rippled-compatible: it expects a base58 family seed
    // (e.g. `snoPBrXtMeMyMHUVTgbuqAfg1SUTb` for the genesis account).
    // `passphrase` is hashed instead. Try seed-decode first for `secret`,
    // and only fall through to passphrase hashing if the input isn't a
    // valid family seed (preserves the legacy behavior for callers that
    // really meant a passphrase).
    if let Some(secret_str) = params.get("secret").and_then(|v| v.as_str()) {
        if let Ok((entropy, _kt)) = rxrpl_codec::address::seed::decode_seed(secret_str) {
            return Ok((rxrpl_crypto::Seed::from_bytes(entropy), key_type));
        }
        // Fall back to passphrase hashing for non-base58 strings.
        return Ok((rxrpl_crypto::Seed::from_passphrase(secret_str), key_type));
    }

    if let Some(passphrase) = params.get("passphrase").and_then(|v| v.as_str()) {
        return Ok((rxrpl_crypto::Seed::from_passphrase(passphrase), key_type));
    }

    Err(RpcServerError::InvalidParams(
        "must provide 'secret', 'seed', 'seed_hex', or 'passphrase'".into(),
    ))
}
