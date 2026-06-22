use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::view::apply_view::ApplyView;
use crate::view::read_view::ReadView;

const PREV_TXN_PLACEHOLDER: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// rippled's per-page cap (dirNodeMaxEntries); a fuller page is split.
const MAX_PAGE_ENTRIES: usize = 32;

/// Locate the existing NFToken page that should hold `nftoken_id` for `owner`:
/// the lowest page key >= the token's candidate key, bounded by the owner's
/// page range. `None` when the owner has no suitable page yet.
pub fn find_owner_page(
    view: &dyn ReadView,
    owner: &AccountId,
    nftoken_id: &Hash256,
) -> Option<Hash256> {
    // A page key is the exclusive upper bound of the low-96 sort keys it holds
    // (the next page's first token), so a token belongs to the page with the
    // smallest key strictly greater than its candidate.
    let candidate = keylet::nftoken_page(owner, nftoken_id);
    let max = keylet::nftoken_page_max(owner);
    let key = view.succ(&candidate)?;
    let same_owner = key.as_bytes()[..20] == owner.as_bytes()[..20];
    if key <= max && same_owner {
        Some(key)
    } else {
        None
    }
}

/// Insert an NFToken object into `owner`'s pages, keeping each page sorted by
/// NFTokenID. Returns `true` when a brand-new page was created (so the caller
/// adjusts the owner reserve). Page splitting at 32 entries is not yet modeled.
pub fn insert_token(
    view: &mut dyn ApplyView,
    owner: &AccountId,
    nftoken_id: &Hash256,
    entry: Value,
) -> Result<bool, TransactionResult> {
    let id_hex = nftoken_id.to_string().to_uppercase();
    match find_owner_page(view, owner, nftoken_id) {
        Some(page_key) => {
            let bytes = view.read(&page_key).ok_or(TransactionResult::TefInternal)?;
            let mut page: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
            let mut tokens: Vec<Value> = page
                .get("NFTokens")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            if tokens.len() >= MAX_PAGE_ENTRIES {
                return split_page_and_insert(view, owner, &page_key, &mut page, tokens, entry);
            }

            let pos = tokens
                .iter()
                .position(|t| nftoken_entry_id(t) > id_hex)
                .unwrap_or(tokens.len());
            tokens.insert(pos, entry);
            page["NFTokens"] = Value::Array(tokens);
            let nb = serde_json::to_vec(&page).map_err(|_| TransactionResult::TefInternal)?;
            view.update(page_key, nb)
                .map_err(|_| TransactionResult::TefInternal)?;
            Ok(false)
        }
        None => {
            let page_key = keylet::nftoken_page_max(owner);
            let page = serde_json::json!({
                "LedgerEntryType": "NFTokenPage",
                "NFTokens": [entry],
                "PreviousTxnID": PREV_TXN_PLACEHOLDER,
                "PreviousTxnLgrSeq": 0,
            });
            let nb = serde_json::to_vec(&page).map_err(|_| TransactionResult::TefInternal)?;
            view.insert(page_key, nb)
                .map_err(|_| TransactionResult::TefInternal)?;
            Ok(true)
        }
    }
}

/// Split a full (32-entry) NFToken page in two and insert the new entry.
/// rippled keeps the upper half in the original page and moves the lower half
/// into a freshly created page keyed at the boundary (the low-96 bits of the
/// first token of the upper half), then threads the doubly-linked page chain.
fn split_page_and_insert(
    view: &mut dyn ApplyView,
    owner: &AccountId,
    orig_key: &Hash256,
    orig_page: &mut Value,
    tokens: Vec<Value>,
    entry: Value,
) -> Result<bool, TransactionResult> {
    let mid = MAX_PAGE_ENTRIES / 2;
    let boundary_id = entry_nftoken_id(&tokens[mid])
        .ok_or(TransactionResult::TefInternal)?
        .to_string();
    let boundary_bytes = hex::decode(&boundary_id).map_err(|_| TransactionResult::TefInternal)?;
    let boundary_hash =
        Hash256::from_slice(&boundary_bytes).map_err(|_| TransactionResult::TefInternal)?;
    let new_key = keylet::nftoken_page(owner, &boundary_hash);
    let new_key_hex = new_key.to_string().to_uppercase();
    let orig_key_hex = orig_key.to_string().to_uppercase();

    let mut lower: Vec<Value> = tokens[..mid].to_vec();
    let mut upper: Vec<Value> = tokens[mid..].to_vec();
    let entry_id = nftoken_entry_id(&entry);
    let target = if entry_id < boundary_id.to_uppercase() {
        &mut lower
    } else {
        &mut upper
    };
    let pos = target
        .iter()
        .position(|t| nftoken_entry_id(t) > entry_id)
        .unwrap_or(target.len());
    target.insert(pos, entry);

    let old_prev = orig_page
        .get("PreviousPageMin")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // New lower page: links back to the old predecessor and forward to the
    // original page, which now holds the upper half.
    let mut new_page = serde_json::json!({
        "LedgerEntryType": "NFTokenPage",
        "NFTokens": lower,
        "NextPageMin": orig_key_hex,
        "PreviousTxnID": PREV_TXN_PLACEHOLDER,
        "PreviousTxnLgrSeq": 0,
    });
    if let Some(p) = &old_prev {
        new_page["PreviousPageMin"] = Value::String(p.clone());
    }
    let nb = serde_json::to_vec(&new_page).map_err(|_| TransactionResult::TefInternal)?;
    view.insert(new_key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;

    orig_page["NFTokens"] = Value::Array(upper);
    orig_page["PreviousPageMin"] = Value::String(new_key_hex.clone());
    let ob = serde_json::to_vec(orig_page).map_err(|_| TransactionResult::TefInternal)?;
    view.update(*orig_key, ob)
        .map_err(|_| TransactionResult::TefInternal)?;

    if let Some(p) = old_prev {
        if let Ok(pbytes) = hex::decode(&p) {
            if let Ok(pk) = Hash256::from_slice(&pbytes) {
                if let Some(pb) = view.read(&pk) {
                    if let Ok(mut prev_page) = serde_json::from_slice::<Value>(&pb) {
                        prev_page["NextPageMin"] = Value::String(new_key_hex);
                        if let Ok(d) = serde_json::to_vec(&prev_page) {
                            view.update(pk, d)
                                .map_err(|_| TransactionResult::TefInternal)?;
                        }
                    }
                }
            }
        }
    }

    Ok(true)
}

/// The NFTokenID held by an sfNFToken page entry (`{"NFToken": {...}}`).
pub fn entry_nftoken_id(entry: &Value) -> Option<&str> {
    entry
        .get("NFToken")
        .and_then(|n| n.get("NFTokenID"))
        .and_then(|v| v.as_str())
}

fn nftoken_entry_id(entry: &Value) -> String {
    entry_nftoken_id(entry)
        .map(|s| s.to_uppercase())
        .unwrap_or_default()
}

/// Lightly mix the taxon with the token sequence so an issuer's NFTs spread
/// across pages instead of clustering. rippled's cipher is its own inverse.
pub fn cipher_taxon(taxon: u32, token_seq: u32) -> u32 {
    taxon ^ token_seq.wrapping_mul(384_160_001).wrapping_add(2_459)
}

/// Generate an NFTokenID matching rippled's 32-byte layout:
/// flags(2) + transfer_fee(2) + issuer(20) + scrambled_taxon(4) + token_seq(4).
pub fn generate_nftoken_id(
    flags: u16,
    transfer_fee: u16,
    issuer_hex: &str,
    taxon: u32,
    token_seq: u32,
) -> String {
    let scrambled = cipher_taxon(taxon, token_seq);
    format!("{flags:04X}{transfer_fee:04X}{issuer_hex}{scrambled:08X}{token_seq:08X}")
}

/// Parse an NFTokenID into (flags, transfer_fee, issuer_hex, taxon, token_seq).
/// The taxon is unscrambled to its original value.
pub fn parse_nftoken_id(id: &str) -> Result<(u16, u16, String, u32, u32), TransactionResult> {
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(TransactionResult::TemMalformed);
    }

    let flags = u16::from_str_radix(&id[0..4], 16).map_err(|_| TransactionResult::TemMalformed)?;
    let transfer_fee =
        u16::from_str_radix(&id[4..8], 16).map_err(|_| TransactionResult::TemMalformed)?;
    let issuer_hex = id[8..48].to_string();
    let scrambled_taxon =
        u32::from_str_radix(&id[48..56], 16).map_err(|_| TransactionResult::TemMalformed)?;
    let token_seq =
        u32::from_str_radix(&id[56..64], 16).map_err(|_| TransactionResult::TemMalformed)?;
    let taxon = cipher_taxon(scrambled_taxon, token_seq);

    Ok((flags, transfer_fee, issuer_hex, taxon, token_seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_parse_roundtrip() {
        let issuer = "B5F762798A53D543A014CAF8B297CFF8F2F937E8";
        let id = generate_nftoken_id(0x0008, 500, issuer, 1337, 1);
        assert_eq!(id.len(), 64);

        let (flags, fee, parsed_issuer, taxon, seq) = parse_nftoken_id(&id).unwrap();
        assert_eq!(flags, 0x0008);
        assert_eq!(fee, 500);
        assert_eq!(parsed_issuer, issuer);
        assert_eq!(taxon, 1337);
        assert_eq!(seq, 1);
    }

    #[test]
    fn nftoken_id_matches_mainnet() {
        // From mainnet ledger 105093054: issuer rJiohLVy, taxon 2, seq 0x0642D947.
        let id = generate_nftoken_id(
            0x0019,
            0x0BB8,
            "C3E4F7A333009F82AD6AC3B730E4F226CB216122",
            2,
            0x0642_D947,
        );
        assert_eq!(
            id,
            "00190BB8C3E4F7A333009F82AD6AC3B730E4F226CB2161221028D9E00642D947"
        );
    }

    #[test]
    fn parse_invalid_length() {
        assert!(parse_nftoken_id("ABCDEF").is_err());
    }

    #[test]
    fn parse_invalid_hex() {
        let bad = "ZZZZZZZZ00000000B5F762798A53D543A014CAF8B297CFF8F2F937E800000001";
        assert!(parse_nftoken_id(bad).is_err());
    }
}
