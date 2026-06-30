//! Owner directory maintenance helpers.
//!
//! Each `AccountRoot` references a `DirectoryNode` page list keyed at
//! `owner_dir(account)`. Per-owner ledger objects (Check, Escrow, Offer,
//! PayChannel, etc.) must be linked into this directory so that
//! `account_objects` and friends can enumerate them.
//!
//! This implementation supports a single root page (≤31 entries — well
//! aligned with rippled's per-page split threshold). Filling the page
//! returns `TecDirFull`; a multi-page implementation can be layered on
//! top later without changing call-sites.

use rxrpl_codec::address::classic::encode_account_id;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::view::apply_view::ApplyView;

/// rippled's `dirNodeMaxEntries`.
const MAX_ENTRIES_PER_PAGE: usize = 32;

fn u64_hex(n: u64) -> String {
    format!("{n:016X}")
}

fn read_u64_field(obj: &Value, field: &str) -> u64 {
    obj.get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(s, 16).ok())
        .unwrap_or(0)
}

fn dir_page(obj: &Value) -> Vec<String> {
    obj.get("Indexes")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_uppercase()))
                .collect()
        })
        .unwrap_or_default()
}

/// Insert `entry_key` into a paginated directory rooted at `root_key`, mirroring
/// rippled's `ApplyView::dirAdd`. Owner directories sort each touched page
/// (`sorted = true`); book directories preserve insertion order. `describe` is
/// the type-specific field set written onto newly created pages. Returns the
/// page number the entry landed in — the `OwnerNode` / `BookNode` value.
pub fn dir_insert(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    entry_key: &Hash256,
    sorted: bool,
    describe: &[(&str, Value)],
) -> Result<u64, TransactionResult> {
    let entry_hex = entry_key.to_string().to_uppercase();

    // With the `fixPreviousTxnID` amendment a freshly created directory page is
    // threaded (records `sfPreviousTxnID` / `sfPreviousTxnLgrSeq`, filled by
    // central stamping). Pre-amendment ledgers leave directory nodes unthreaded,
    // so the field must be omitted entirely when replaying them.
    let thread_dirs = view.thread_directories();

    let make_page = |indexes: Vec<String>, extra: &[(&str, Value)]| -> Value {
        let mut m = serde_json::Map::new();
        m.insert("LedgerEntryType".into(), "DirectoryNode".into());
        m.insert("Flags".into(), Value::from(0u32));
        m.insert("RootIndex".into(), root_key.to_string().into());
        if thread_dirs {
            m.insert(
                "PreviousTxnID".into(),
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            );
        }
        for (k, v) in describe {
            m.insert((*k).to_string(), v.clone());
        }
        for (k, v) in extra {
            m.insert((*k).to_string(), v.clone());
        }
        m.insert(
            "Indexes".into(),
            Value::Array(indexes.into_iter().map(Value::from).collect()),
        );
        Value::Object(m)
    };
    let put = |view: &mut dyn ApplyView, key: Hash256, v: &Value, insert: bool| {
        let bytes = serde_json::to_vec(v).map_err(|_| TransactionResult::TefInternal)?;
        if insert {
            view.insert(key, bytes)
        } else {
            view.update(key, bytes)
        }
        .map_err(|_| TransactionResult::TefInternal)
    };

    let Some(root_bytes) = view.read(root_key) else {
        put(view, *root_key, &make_page(vec![entry_hex], &[]), true)?;
        return Ok(0);
    };
    let root: Value =
        serde_json::from_slice(&root_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let last_page = read_u64_field(&root, "IndexPrevious");
    let node_key = keylet::dir_node(root_key, last_page);
    let mut node: Value = if last_page == 0 {
        root.clone()
    } else {
        let b = view.read(&node_key).ok_or(TransactionResult::TefInternal)?;
        serde_json::from_slice(&b).map_err(|_| TransactionResult::TefInternal)?
    };
    let mut indexes = dir_page(&node);

    if indexes.iter().any(|h| h == &entry_hex) {
        return Ok(last_page);
    }

    if indexes.len() < MAX_ENTRIES_PER_PAGE {
        indexes.push(entry_hex);
        if sorted {
            indexes.sort();
        }
        node["Indexes"] = Value::Array(indexes.into_iter().map(Value::from).collect());
        put(view, node_key, &node, false)?;
        return Ok(last_page);
    }

    // Page full: link a new page at the end of the chain.
    let new_page = last_page.wrapping_add(1);
    if last_page == 0 {
        node["IndexNext"] = u64_hex(new_page).into();
        node["IndexPrevious"] = u64_hex(new_page).into();
        put(view, node_key, &node, false)?;
    } else {
        node["IndexNext"] = u64_hex(new_page).into();
        put(view, node_key, &node, false)?;
        let mut root_mut = root;
        root_mut["IndexPrevious"] = u64_hex(new_page).into();
        put(view, *root_key, &root_mut, false)?;
    }
    let extra: Vec<(&str, Value)> = if new_page != 1 {
        vec![("IndexPrevious", u64_hex(new_page - 1).into())]
    } else {
        vec![]
    };
    put(
        view,
        keylet::dir_node(root_key, new_page),
        &make_page(vec![entry_key.to_string().to_uppercase()], &extra),
        true,
    )?;
    Ok(new_page)
}

/// Remove `entry_key` from a paginated directory, walking the page chain from
/// the root. Empties non-root pages are unlinked and erased; an empty root with
/// no successor is erased. No-op if the entry is absent.
pub fn dir_remove(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    let entry_hex = entry_key.to_string().to_uppercase();
    let mut page = 0u64;
    loop {
        let page_key = keylet::dir_node(root_key, page);
        let Some(bytes) = view.read(&page_key) else {
            return Ok(());
        };
        let node: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let next = read_u64_field(&node, "IndexNext");
        if dir_page(&node).iter().any(|h| h == &entry_hex) {
            return dir_remove_page(view, root_key, page, entry_key);
        }
        if next == 0 {
            return Ok(());
        }
        page = next;
    }
}

/// Remove `entry_key` from a specific directory page located by its page number,
/// mirroring rippled's hinted `dirRemove`. The owning SLE records the page it
/// landed in (`OwnerNode` / `DestinationNode` / `BookNode`), so the chain need
/// not be walked from the root — essential when only the touched page is loaded.
pub fn dir_remove_page(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    page: u64,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    dir_remove_page_impl(view, root_key, page, entry_key, false)
}

/// As `dir_remove_page`, but when the removal empties the whole directory the
/// root page is kept (left with an empty `Indexes`) instead of erased. rippled
/// chooses this per call site (`dirRemove` `keepRoot`); e.g. DIDDelete and
/// Escrow finish/cancel keep the empty owner/destination root while OfferCancel
/// deletes it.
fn dir_remove_page_impl(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    page: u64,
    entry_key: &Hash256,
    keep_root: bool,
) -> Result<(), TransactionResult> {
    let entry_hex = entry_key.to_string().to_uppercase();
    let order_preserving = view.sorted_directories();
    let page_key = keylet::dir_node(root_key, page);
    let Some(bytes) = view.read(&page_key) else {
        return Ok(());
    };
    let mut node: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let mut indexes = dir_page(&node);
    let next = read_u64_field(&node, "IndexNext");
    let Some(pos) = indexes.iter().position(|h| h == &entry_hex) else {
        return Ok(());
    };
    // SortedDirectories preserves relative order on removal; the legacy
    // dirDelete swapped the entry with the last and popped. Gating on the
    // amendment replays both eras' page ordering byte-for-byte.
    if order_preserving {
        indexes.remove(pos);
    } else {
        indexes.swap_remove(pos);
    }
    if indexes.is_empty() && page != 0 {
        // Unlink this page from the chain, then erase it.
        let prev = read_u64_field(&node, "IndexPrevious");
        relink(view, root_key, prev, next)?;
        view.erase(&page_key)
            .map_err(|_| TransactionResult::TefInternal)?;
        // If that emptied the whole directory, drop the now-empty root too
        // (unless the caller asked to keep it).
        if !keep_root {
            if let Some(b) = view.read(root_key) {
                if let Ok(root) = serde_json::from_slice::<Value>(&b) {
                    if dir_page(&root).is_empty() && read_u64_field(&root, "IndexNext") == 0 {
                        view.erase(root_key)
                            .map_err(|_| TransactionResult::TefInternal)?;
                    }
                }
            }
        }
    } else if indexes.is_empty() && page == 0 && next == 0 && !keep_root {
        view.erase(&page_key)
            .map_err(|_| TransactionResult::TefInternal)?;
    } else {
        node["Indexes"] = Value::Array(indexes.into_iter().map(Value::from).collect());
        let nb = serde_json::to_vec(&node).map_err(|_| TransactionResult::TefInternal)?;
        view.update(page_key, nb)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    Ok(())
}

/// Repair the `IndexNext`/`IndexPrevious` links around a removed page.
fn relink(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    prev: u64,
    next: u64,
) -> Result<(), TransactionResult> {
    let patch = |view: &mut dyn ApplyView, page: u64, field: &str, val: u64| {
        let key = keylet::dir_node(root_key, page);
        if let Some(b) = view.read(&key) {
            if let Ok(mut n) = serde_json::from_slice::<Value>(&b) {
                // A non-root page always carries its links, so IndexPrevious=0
                // (pointing back at the root) is kept; only the root page drops
                // a zero link (single-page directory).
                if val == 0 && page == 0 {
                    n.as_object_mut().map(|o| o.remove(field));
                } else {
                    n[field] = u64_hex(val).into();
                }
                if let Ok(nb) = serde_json::to_vec(&n) {
                    let _ = view.update(key, nb);
                }
            }
        }
    };
    patch(view, prev, "IndexNext", next);
    patch(view, next, "IndexPrevious", prev);
    Ok(())
}

/// Add an entry to the account's (sorted, paginated) owner directory. Returns
/// the page number (`OwnerNode`) the entry landed in.
pub fn add_to_owner_dir(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    entry_key: &Hash256,
) -> Result<u64, TransactionResult> {
    let root_key = keylet::owner_dir(account_id);
    let describe = [("Owner", Value::from(encode_account_id(account_id)))];
    // With SortedDirectories the owner directory is kept sorted; before the
    // amendment rippled appended, leaving legacy pages in insertion order.
    let sorted = view.sorted_directories();
    dir_insert(view, &root_key, entry_key, sorted, &describe)
}

/// Add an entry to a per-NFToken buy/sell offer directory. Unlike owner and
/// book directories, a freshly created NFToken offer page is tagged with the
/// directory kind in sfFlags — `lsfNFTokenSellOffers` (2) for sell books and
/// `lsfNFTokenBuyOffers` (1) for buy books (mirrors rippled NFTokenUtils) — and
/// is threaded (records PreviousTxnID); the placeholder here is filled by the
/// engine's central stamping. New pages also carry the NFTokenID. Pages follow
/// the owner-directory sort discipline.
pub fn add_to_nft_offer_dir(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    nftoken_id_hex: &str,
    entry_key: &Hash256,
    is_sell: bool,
) -> Result<u64, TransactionResult> {
    let dir_flags: u32 = if is_sell { 2 } else { 1 };
    let describe = [
        ("Flags", Value::from(dir_flags)),
        ("NFTokenID", Value::from(nftoken_id_hex)),
        (
            "PreviousTxnID",
            Value::from("0000000000000000000000000000000000000000000000000000000000000000"),
        ),
    ];
    let sorted = view.sorted_directories();
    dir_insert(view, root_key, entry_key, sorted, &describe)
}

/// Append an entry to a book directory page. `describe` carries the book's
/// `ExchangeRate` / `TakerPays*` / `TakerGets*` fields for new pages. Returns
/// the `BookNode` page the entry landed in.
pub fn add_to_book_dir(
    view: &mut dyn ApplyView,
    root_key: &Hash256,
    entry_key: &Hash256,
    describe: &[(&str, Value)],
) -> Result<u64, TransactionResult> {
    dir_insert(view, root_key, entry_key, false, describe)
}

/// Remove an entry from the account's owner directory (paginated).
pub fn remove_from_owner_dir(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    dir_remove(view, &keylet::owner_dir(account_id), entry_key)
}

/// As `remove_from_owner_dir`, but keeps the owner-directory root page (empty)
/// if the removal empties the directory. Matches rippled call sites that pass
/// `keepRoot = true` (e.g. DIDDelete, Escrow finish/cancel).
pub fn remove_from_owner_dir_keep_root(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    let root_key = keylet::owner_dir(account_id);
    let entry_hex = entry_key.to_string().to_uppercase();
    let mut page = 0u64;
    loop {
        let page_key = keylet::dir_node(&root_key, page);
        let Some(bytes) = view.read(&page_key) else {
            return Ok(());
        };
        let node: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let next = read_u64_field(&node, "IndexNext");
        if dir_page(&node).iter().any(|h| h == &entry_hex) {
            return dir_remove_page_impl(view, &root_key, page, entry_key, true);
        }
        if next == 0 {
            return Ok(());
        }
        page = next;
    }
}

/// Remove an entry from a known page of the account's owner directory, using the
/// page hint recorded on the owning SLE (`OwnerNode` / `DestinationNode`).
pub fn remove_from_owner_dir_page(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    page: u64,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    dir_remove_page(view, &keylet::owner_dir(account_id), page, entry_key)
}

/// Collect every entry key listed in an account's owner directory, walking the
/// page chain from the root. Entries are returned in directory order (page by
/// page). Used by AMMDelete and AccountDelete to enumerate an account's holdings.
pub fn collect_owner_dir_entries(
    view: &dyn crate::view::read_view::ReadView,
    account_id: &AccountId,
) -> Vec<String> {
    let root_key = keylet::owner_dir(account_id);
    let mut out = Vec::new();
    let mut page = 0u64;
    loop {
        let page_key = keylet::dir_node(&root_key, page);
        let Some(bytes) = view.read(&page_key) else {
            break;
        };
        let Ok(node) = serde_json::from_slice::<Value>(&bytes) else {
            break;
        };
        out.extend(dir_page(&node));
        let next = read_u64_field(&node, "IndexNext");
        if next == 0 {
            break;
        }
        page = next;
    }
    out
}

/// Consume the transaction's sequence proxy: either bump the account
/// `Sequence`, or — when a `TicketSequence` is present — consume the Ticket
/// SLE (remove it from the owner directory, erase it, and decrement
/// `OwnerCount`) instead. Mirrors rippled's `Transactor::consumeSeqProxy`.
///
/// `check_seq_proxy` has already validated that the ticket exists at preclaim;
/// the `exists` guard here keeps apply self-consistent if it was consumed
/// earlier in the same ledger.
pub fn consume_seq_or_ticket(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    account_obj: &mut Value,
    tx: &Value,
) -> Result<(), TransactionResult> {
    match tx.get("TicketSequence").and_then(|v| v.as_u64()) {
        Some(ticket_seq) => {
            let ticket_key = keylet::ticket(account_id, ticket_seq as u32);
            let Some(ticket_bytes) = view.read(&ticket_key) else {
                return Err(TransactionResult::TefNoTicket);
            };
            // rippled consumes the ticket via a HINTED directory removal using
            // the page recorded on the Ticket SLE's `OwnerNode` (`dirRemove` with
            // the known page), not a walk from the root. This is essential for a
            // large owner directory whose intermediate pages are not loaded (e.g.
            // the single-tx oracle seeds only touched pages): a root walk stops at
            // the first absent page and would leave the consumed ticket's index
            // stranded on its high-numbered page. `OwnerNode` is default-dropped
            // when zero, so its absence means the ticket lives on the root page —
            // fall back to the walk, which finds it there immediately.
            let owner_node = serde_json::from_slice::<Value>(&ticket_bytes)
                .ok()
                .and_then(|t| {
                    t.get("OwnerNode")
                        .and_then(|v| v.as_str())
                        .and_then(|s| u64::from_str_radix(s, 16).ok())
                });
            match owner_node {
                Some(page) => remove_from_owner_dir_page(view, account_id, page, &ticket_key)?,
                None => remove_from_owner_dir(view, account_id, &ticket_key)?,
            }
            view.erase(&ticket_key)
                .map_err(|_| TransactionResult::TefInternal)?;
            crate::helpers::adjust_owner_count(account_obj, -1);
            decrement_ticket_count(account_obj);
        }
        None => crate::helpers::increment_sequence(account_obj),
    }
    Ok(())
}

/// Decrement `TicketCount` on an account root when a ticket is consumed,
/// dropping the field entirely when the last ticket is burned (mirrors
/// rippled's `makeFieldAbsent` on the final ticket).
fn decrement_ticket_count(account_obj: &mut Value) {
    let Some(count) = account_obj.get("TicketCount").and_then(|v| v.as_u64()) else {
        return;
    };
    if count <= 1 {
        if let Some(obj) = account_obj.as_object_mut() {
            obj.remove("TicketCount");
        }
    } else {
        account_obj["TicketCount"] = Value::from(count - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_codec::address::classic::decode_account_id;
    use rxrpl_ledger::Ledger;

    const ACCT: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn id() -> AccountId {
        decode_account_id(ACCT).unwrap()
    }

    fn entry(byte: u8) -> Hash256 {
        let mut bytes = [0u8; 32];
        bytes[31] = byte;
        Hash256::from(bytes)
    }

    fn fresh_sandbox() -> (Ledger, FeeSettings) {
        let ledger = Ledger::genesis();
        (ledger, FeeSettings::default())
    }

    #[test]
    fn add_creates_root_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        let e = entry(1);
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();

        let dir_bytes = sandbox.read(&keylet::owner_dir(&account)).unwrap();
        let dir: Value = serde_json::from_slice(&dir_bytes).unwrap();
        assert_eq!(dir["Indexes"][0], e.to_string());
    }

    #[test]
    fn add_appends_to_existing_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        add_to_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();
        add_to_owner_dir(&mut sandbox, &account, &entry(2)).unwrap();

        let dir_bytes = sandbox.read(&keylet::owner_dir(&account)).unwrap();
        let dir: Value = serde_json::from_slice(&dir_bytes).unwrap();
        assert_eq!(dir["Indexes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn add_is_idempotent() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        let e = entry(7);
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();

        let dir: Value =
            serde_json::from_slice(&sandbox.read(&keylet::owner_dir(&account)).unwrap()).unwrap();
        assert_eq!(dir["Indexes"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn remove_clears_entry_and_keeps_others() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        add_to_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();
        add_to_owner_dir(&mut sandbox, &account, &entry(2)).unwrap();
        remove_from_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();

        let dir: Value =
            serde_json::from_slice(&sandbox.read(&keylet::owner_dir(&account)).unwrap()).unwrap();
        assert_eq!(dir["Indexes"].as_array().unwrap().len(), 1);
        assert_eq!(dir["Indexes"][0], entry(2).to_string());
    }

    #[test]
    fn remove_last_erases_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        let e = entry(9);
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();
        remove_from_owner_dir(&mut sandbox, &account, &e).unwrap();

        assert!(sandbox.read(&keylet::owner_dir(&account)).is_none());
    }

    #[test]
    fn remove_non_existent_is_noop() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        remove_from_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();
        // Still nothing in the directory; no panic.
        assert!(sandbox.read(&keylet::owner_dir(&account)).is_none());
    }

    #[test]
    fn full_page_spills_into_new_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        // First MAX_ENTRIES_PER_PAGE entries fill the root page (page 0).
        for i in 0..MAX_ENTRIES_PER_PAGE {
            let page = add_to_owner_dir(&mut sandbox, &account, &entry(i as u8)).unwrap();
            assert_eq!(page, 0);
        }
        // The next entry spills into a freshly linked page (page 1).
        let page = add_to_owner_dir(&mut sandbox, &account, &entry(0xff)).unwrap();
        assert_eq!(page, 1);
    }

    #[test]
    fn legacy_appends_and_swaps_on_remove() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view); // SortedDirectories off (default)

        let account = id();
        // Out-of-order insert: legacy keeps insertion order (append).
        for b in [3u8, 1, 2] {
            add_to_owner_dir(&mut sandbox, &account, &entry(b)).unwrap();
        }
        let dir = |s: &Sandbox| -> Vec<String> {
            let v: Value =
                serde_json::from_slice(&s.read(&keylet::owner_dir(&account)).unwrap()).unwrap();
            dir_page(&v)
        };
        assert_eq!(
            dir(&sandbox),
            [entry(3), entry(1), entry(2)].map(|e| e.to_string().to_uppercase())
        );
        // Legacy removal swaps the gap with the last entry.
        remove_from_owner_dir(&mut sandbox, &account, &entry(3)).unwrap();
        assert_eq!(
            dir(&sandbox),
            [entry(2), entry(1)].map(|e| e.to_string().to_uppercase())
        );
    }

    #[test]
    fn sorted_directories_sorts_and_preserves_order_on_remove() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);
        sandbox.set_sorted_directories(true);

        let account = id();
        for b in [3u8, 1, 2] {
            add_to_owner_dir(&mut sandbox, &account, &entry(b)).unwrap();
        }
        let dir = |s: &Sandbox| -> Vec<String> {
            let v: Value =
                serde_json::from_slice(&s.read(&keylet::owner_dir(&account)).unwrap()).unwrap();
            dir_page(&v)
        };
        // Modern owner dir is kept sorted ascending.
        assert_eq!(
            dir(&sandbox),
            [entry(1), entry(2), entry(3)].map(|e| e.to_string().to_uppercase())
        );
        // Order-preserving removal shifts rather than swapping with the last.
        remove_from_owner_dir(&mut sandbox, &account, &entry(2)).unwrap();
        assert_eq!(
            dir(&sandbox),
            [entry(1), entry(3)].map(|e| e.to_string().to_uppercase())
        );
    }

    #[test]
    fn child_sandbox_inherits_sorted_flag() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);
        sandbox.set_sorted_directories(true);
        assert!(sandbox.child().sorted_directories());
    }

    #[test]
    fn hinted_remove_targets_the_named_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        for i in 0..MAX_ENTRIES_PER_PAGE {
            add_to_owner_dir(&mut sandbox, &account, &entry(i as u8)).unwrap();
        }
        let spilled = entry(0xff);
        assert_eq!(
            add_to_owner_dir(&mut sandbox, &account, &spilled).unwrap(),
            1
        );

        // Remove directly via the page hint; page 1 held a single entry so it
        // unlinks without walking from the root.
        remove_from_owner_dir_page(&mut sandbox, &account, 1, &spilled).unwrap();
        let page1 = keylet::dir_node(&keylet::owner_dir(&account), 1);
        assert!(sandbox.read(&page1).is_none());
    }

    #[test]
    fn consume_ticket_decrements_ticket_count_and_drops_last() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        for seq in [3u32, 4] {
            let key = keylet::ticket(&id(), seq);
            let ticket = serde_json::json!({
                "LedgerEntryType": "Ticket", "Account": ACCT, "TicketSequence": seq, "Flags": 0,
            });
            sandbox
                .insert(key, serde_json::to_vec(&ticket).unwrap())
                .unwrap();
        }

        let mut acct = account_obj(5, 2);
        acct["TicketCount"] = Value::from(2u32);

        let tx = serde_json::json!({ "Account": ACCT, "TicketSequence": 3 });
        consume_seq_or_ticket(&mut sandbox, &id(), &mut acct, &tx).unwrap();
        assert_eq!(acct["TicketCount"], serde_json::json!(1));

        let tx = serde_json::json!({ "Account": ACCT, "TicketSequence": 4 });
        consume_seq_or_ticket(&mut sandbox, &id(), &mut acct, &tx).unwrap();
        assert!(acct.get("TicketCount").is_none());
    }

    fn account_obj(seq: u32, owner_count: u32) -> Value {
        serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ACCT,
            "Balance": "1000000",
            "Sequence": seq,
            "OwnerCount": owner_count,
            "Flags": 0,
        })
    }

    #[test]
    fn consume_plain_sequence_increments_seq() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);
        let mut acct = account_obj(5, 2);
        let tx = serde_json::json!({ "Account": ACCT, "Sequence": 5 });

        consume_seq_or_ticket(&mut sandbox, &id(), &mut acct, &tx).unwrap();

        assert_eq!(acct["Sequence"], serde_json::json!(6));
        assert_eq!(acct["OwnerCount"], serde_json::json!(2));
    }

    #[test]
    fn consume_existing_ticket_erases_and_decrements_owner_count() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let ticket_key = keylet::ticket(&id(), 3);
        let ticket = serde_json::json!({
            "LedgerEntryType": "Ticket", "Account": ACCT, "TicketSequence": 3, "Flags": 0,
        });
        sandbox
            .insert(ticket_key, serde_json::to_vec(&ticket).unwrap())
            .unwrap();

        let mut acct = account_obj(5, 2);
        let tx = serde_json::json!({ "Account": ACCT, "TicketSequence": 3 });

        consume_seq_or_ticket(&mut sandbox, &id(), &mut acct, &tx).unwrap();

        // Sequence unchanged, owner count decremented, ticket gone.
        assert_eq!(acct["Sequence"], serde_json::json!(5));
        assert_eq!(acct["OwnerCount"], serde_json::json!(1));
        assert!(!sandbox.exists(&ticket_key));
    }

    #[test]
    fn consume_missing_ticket_errors() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);
        let mut acct = account_obj(5, 2);
        let tx = serde_json::json!({ "Account": ACCT, "TicketSequence": 3 });

        let err = consume_seq_or_ticket(&mut sandbox, &id(), &mut acct, &tx).unwrap_err();
        assert_eq!(err, TransactionResult::TefNoTicket);
    }
}
