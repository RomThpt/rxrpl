use std::cmp::Ordering;

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
                .position(|t| compare_token_ids(&nftoken_entry_id(t), &id_hex) == Ordering::Greater)
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
            // sfFlags is a common SoeRequired field on every SLE, so rippled
            // always serializes Flags=0 on an NFTokenPage.
            let page = serde_json::json!({
                "LedgerEntryType": "NFTokenPage",
                "Flags": 0,
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
    let target = if compare_token_ids(&entry_id, &boundary_id.to_uppercase()) == Ordering::Less {
        &mut lower
    } else {
        &mut upper
    };
    let pos = target
        .iter()
        .position(|t| compare_token_ids(&nftoken_entry_id(t), &entry_id) == Ordering::Greater)
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
        "Flags": 0,
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

/// Read an NFTokenPage SLE as JSON.
fn read_page(view: &dyn ApplyView, key: &Hash256) -> Result<Value, TransactionResult> {
    let bytes = view.read(key).ok_or(TransactionResult::TefInternal)?;
    serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)
}

/// Write an NFTokenPage SLE back.
fn write_page(
    view: &mut dyn ApplyView,
    key: &Hash256,
    page: &Value,
) -> Result<(), TransactionResult> {
    let data = serde_json::to_vec(page).map_err(|_| TransactionResult::TefInternal)?;
    view.update(*key, data)
        .map_err(|_| TransactionResult::TefInternal)
}

/// The NFTokens array of a page (empty when absent).
fn page_tokens(page: &Value) -> Vec<Value> {
    page.get("NFTokens")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Parse a page-chain link field (`PreviousPageMin`/`NextPageMin`) — stored as
/// 64-char uppercase hex — into a page key.
fn page_link(page: &Value, field: &str) -> Option<Hash256> {
    let s = page.get(field).and_then(|v| v.as_str())?;
    Hash256::from_slice(&hex::decode(s).ok()?).ok()
}

fn page_key_hex(key: &Hash256) -> String {
    key.to_string().to_uppercase()
}

/// Like [`read_page`] but treats an absent entry as `None` rather than an
/// error, for the directory walk where a missing successor is normal.
fn read_page_opt(view: &dyn ApplyView, key: &Hash256) -> Option<Value> {
    serde_json::from_slice(&view.read(key)?).ok()
}

/// rippled `nft::repairNFTokenDirectoryLinks`: walk `owner`'s NFTokenPage
/// directory and repair the doubly-linked `PreviousPageMin`/`NextPageMin`
/// chain. If the final page lost its canonical `nftoken_page_max` index, its
/// contents are moved into a page keyed there. Returns whether anything was
/// actually repaired.
pub fn repair_directory_links(
    view: &mut dyn ApplyView,
    owner: &AccountId,
) -> Result<bool, TransactionResult> {
    let mut did_repair = false;
    let min = keylet::nftoken_page_min(owner);
    let last = keylet::nftoken_page_max(owner);

    let first_key = view.succ(&min).filter(|k| *k <= last).unwrap_or(last);
    let Some(mut page) = read_page_opt(view, &first_key) else {
        return Ok(did_repair);
    };
    let mut page_key = first_key;

    if page_key == last {
        let obj = page.as_object_mut().unwrap();
        let had = obj.remove("NextPageMin").is_some() | obj.remove("PreviousPageMin").is_some();
        if had {
            did_repair = true;
            write_page(view, &page_key, &page)?;
        }
        return Ok(did_repair);
    }

    if page.get("PreviousPageMin").is_some() {
        did_repair = true;
        page.as_object_mut().unwrap().remove("PreviousPageMin");
        write_page(view, &page_key, &page)?;
    }

    loop {
        let next_key = view.succ(&page_key).filter(|k| *k <= last).unwrap_or(last);
        let Some(mut next_page) = read_page_opt(view, &next_key) else {
            // The last real page is not at the canonical index: move its
            // contents into a fresh page keyed by `last` (carrying only the
            // tokens and previous link, never a next link) and rethread its
            // previous page.
            did_repair = true;
            let mut relocated = serde_json::json!({
                "LedgerEntryType": "NFTokenPage",
                "NFTokens": Value::Array(page_tokens(&page)),
            });
            if let Some(prev_key) = page_link(&page, "PreviousPageMin") {
                relocated["PreviousPageMin"] = Value::String(page_key_hex(&prev_key));
                let mut new_prev = read_page(view, &prev_key)?;
                new_prev["NextPageMin"] = Value::String(page_key_hex(&last));
                write_page(view, &prev_key, &new_prev)?;
            }
            view.erase(&page_key)
                .map_err(|_| TransactionResult::TefInternal)?;
            let data =
                serde_json::to_vec(&relocated).map_err(|_| TransactionResult::TefInternal)?;
            view.insert(last, data)
                .map_err(|_| TransactionResult::TefInternal)?;
            return Ok(did_repair);
        };

        if page_link(&page, "NextPageMin") != Some(next_key) {
            did_repair = true;
            page["NextPageMin"] = Value::String(page_key_hex(&next_key));
            write_page(view, &page_key, &page)?;
        }
        if page_link(&next_page, "PreviousPageMin") != Some(page_key) {
            did_repair = true;
            next_page["PreviousPageMin"] = Value::String(page_key_hex(&page_key));
            write_page(view, &next_key, &next_page)?;
        }

        if next_key == last {
            if next_page.get("NextPageMin").is_some() {
                did_repair = true;
                next_page.as_object_mut().unwrap().remove("NextPageMin");
                write_page(view, &next_key, &next_page)?;
            }
            return Ok(did_repair);
        }

        page = next_page;
        page_key = next_key;
    }
}

/// rippled `nft::mergePages`: if the linked pages `lo` (lower key) and `hi`
/// (higher key) hold few enough tokens combined to fit one page, merge `lo`'s
/// tokens into `hi`, rethread the chain around `lo`, and erase `lo`. Returns
/// whether the merge actually happened.
fn merge_pages(
    view: &mut dyn ApplyView,
    lo_key: &Hash256,
    hi_key: &Hash256,
) -> Result<bool, TransactionResult> {
    // Either neighbour may be absent: rippled inspects the full directory, but
    // here only the SLEs this tx actually touches are available. A merge that
    // would have happened leaves both pages in the tx metadata (one modified,
    // one deleted); so if a neighbour can't be read, rippled didn't merge it
    // and neither do we.
    let (Some(lo_bytes), Some(hi_bytes)) = (view.read(lo_key), view.read(hi_key)) else {
        return Ok(false);
    };
    let lo: Value =
        serde_json::from_slice(&lo_bytes).map_err(|_| TransactionResult::TefInternal)?;
    let mut hi: Value =
        serde_json::from_slice(&hi_bytes).map_err(|_| TransactionResult::TefInternal)?;
    let mut tokens = page_tokens(&lo);
    let hi_tokens = page_tokens(&hi);
    if tokens.len() + hi_tokens.len() > MAX_PAGE_ENTRIES {
        return Ok(false);
    }
    tokens.extend(hi_tokens);
    tokens.sort_by(|a, b| compare_token_ids(&nftoken_entry_id(a), &nftoken_entry_id(b)));
    hi["NFTokens"] = Value::Array(tokens);

    // `hi` loses its back-link to `lo`; rethread it to `lo`'s predecessor.
    if let Some(p0_key) = page_link(&lo, "PreviousPageMin") {
        let mut p0 = read_page(view, &p0_key)?;
        p0["NextPageMin"] = Value::String(page_key_hex(hi_key));
        write_page(view, &p0_key, &p0)?;
        hi["PreviousPageMin"] = Value::String(page_key_hex(&p0_key));
    } else if let Some(obj) = hi.as_object_mut() {
        obj.remove("PreviousPageMin");
    }
    write_page(view, hi_key, &hi)?;
    view.erase(lo_key)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(true)
}

/// Remove `nftoken_id` from `owner`'s NFToken pages, mirroring rippled's
/// `nft::removeToken`: drop the token from its page, then consolidate the
/// doubly linked page chain (merging adjacent pages whose combined size fits in
/// one page, erasing emptied pages and fixing `PreviousPageMin`/`NextPageMin`
/// links). `fix_page_links` enables the fixNFTokenPageLinks last-page handling.
/// Returns the removed `{"NFToken": {...}}` object plus the owner-count delta
/// (0, -1 or -2) the caller must apply, or `None` when `owner` doesn't hold it.
pub fn remove_token(
    view: &mut dyn ApplyView,
    owner: &AccountId,
    nftoken_id: &Hash256,
    fix_page_links: bool,
) -> Result<Option<(Value, i64)>, TransactionResult> {
    let Some(curr_key) = find_owner_page(view, owner, nftoken_id) else {
        return Ok(None);
    };
    let id_hex = nftoken_id.to_string().to_uppercase();

    let mut curr = read_page(view, &curr_key)?;
    let mut tokens = page_tokens(&curr);
    let Some(pos) = tokens.iter().position(|t| nftoken_entry_id(t) == id_hex) else {
        return Ok(None);
    };
    let removed = tokens.remove(pos);

    let prev_key = page_link(&curr, "PreviousPageMin");
    let next_key = page_link(&curr, "NextPageMin");

    // The page still holds tokens: write it back, then try to consolidate it
    // with either neighbour (a merge may absorb a whole page).
    if !tokens.is_empty() {
        curr["NFTokens"] = Value::Array(tokens);
        write_page(view, &curr_key, &curr)?;

        let mut delta: i64 = 0;
        if let Some(pk) = &prev_key {
            if merge_pages(view, pk, &curr_key)? {
                delta -= 1;
            }
        }
        if let Some(nk) = &next_key {
            if merge_pages(view, &curr_key, nk)? {
                delta -= 1;
            }
        }
        return Ok(Some((removed, delta)));
    }

    // The page is now empty.
    if let Some(pk) = &prev_key {
        // fixNFTokenPageLinks: an emptied *last* page is refilled from its
        // predecessor (whose slot is then erased) instead of being removed, so
        // the directory always keeps its max-key page.
        if fix_page_links && curr_key == keylet::nftoken_page_max(owner) {
            let prev = read_page(view, pk)?;
            curr["NFTokens"] = Value::Array(page_tokens(&prev));
            match page_link(&prev, "PreviousPageMin") {
                Some(p0_key) => {
                    curr["PreviousPageMin"] = Value::String(page_key_hex(&p0_key));
                    let mut p0 = read_page(view, &p0_key)?;
                    p0["NextPageMin"] = Value::String(page_key_hex(&curr_key));
                    write_page(view, &p0_key, &p0)?;
                }
                None => {
                    if let Some(obj) = curr.as_object_mut() {
                        obj.remove("PreviousPageMin");
                    }
                }
            }
            write_page(view, &curr_key, &curr)?;
            view.erase(pk).map_err(|_| TransactionResult::TefInternal)?;
            return Ok(Some((removed, -1)));
        }

        // Otherwise unlink the empty page from its predecessor.
        let mut prev = read_page(view, pk)?;
        match &next_key {
            Some(nk) => prev["NextPageMin"] = Value::String(page_key_hex(nk)),
            None => {
                if let Some(obj) = prev.as_object_mut() {
                    obj.remove("NextPageMin");
                }
            }
        }
        write_page(view, pk, &prev)?;
    }

    if let Some(nk) = &next_key {
        let mut next = read_page(view, nk)?;
        match &prev_key {
            Some(pk) => next["PreviousPageMin"] = Value::String(page_key_hex(pk)),
            None => {
                if let Some(obj) = next.as_object_mut() {
                    obj.remove("PreviousPageMin");
                }
            }
        }
        write_page(view, nk, &next)?;
    }

    view.erase(&curr_key)
        .map_err(|_| TransactionResult::TefInternal)?;

    // One page went away; a follow-up merge of the now-adjacent neighbours may
    // remove a second.
    let mut cnt: i64 = 1;
    if let (Some(pk), Some(nk)) = (&prev_key, &next_key) {
        if merge_pages(view, pk, nk)? {
            cnt += 1;
        }
    }
    Ok(Some((removed, -cnt)))
}

/// Order two NFTokenIDs the way rippled's `nft::compareTokens` does: by the
/// low 96 bits (the last 12 bytes, `kPageMask`) first, falling back to the full
/// 256-bit value when those collide. NFT pages keep `sfNFTokens` in exactly
/// this order — note this differs from a plain lexicographic compare because
/// the flags/transfer-fee/issuer bytes that lead the id are ignored by the
/// primary key. Inputs are 64-char uppercase hex; malformed inputs fall back to
/// a full-string compare.
pub fn compare_token_ids(a: &str, b: &str) -> Ordering {
    if a.len() == 64 && b.len() == 64 {
        // Low 96 bits = last 24 hex chars (12 bytes).
        match a[40..].cmp(&b[40..]) {
            Ordering::Equal => a.cmp(b),
            ord => ord,
        }
    } else {
        a.cmp(b)
    }
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
