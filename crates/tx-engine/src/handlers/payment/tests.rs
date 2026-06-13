use super::*;
use crate::fees::FeeSettings;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
use crate::view::ledger_view::LedgerView;
use crate::view::sandbox::Sandbox;
use rxrpl_amendment::Rules;
use rxrpl_ledger::Ledger;

const SRC_ADDRESS: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
const DST_ADDRESS: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

fn setup_ledger_with_account(address: &str, balance: u64) -> Ledger {
    let mut ledger = Ledger::genesis();
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let account = serde_json::json!({
        "LedgerEntryType": "AccountRoot",
        "Account": address,
        "Balance": balance.to_string(),
        "Sequence": 1,
        "OwnerCount": 0,
        "Flags": 0,
    });
    let data = serde_json::to_vec(&account).unwrap();
    ledger.put_state(key, data).unwrap();
    ledger
}

fn add_account(ledger: &mut Ledger, address: &str, balance: u64) {
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let account = serde_json::json!({
        "LedgerEntryType": "AccountRoot",
        "Account": address,
        "Balance": balance.to_string(),
        "Sequence": 1,
        "OwnerCount": 0,
        "Flags": 0,
    });
    ledger
        .put_state(key, serde_json::to_vec(&account).unwrap())
        .unwrap();
}

fn make_payment_tx(account: &str, destination: &str, amount: &str, fee: &str) -> serde_json::Value {
    serde_json::json!({
        "TransactionType": "Payment",
        "Account": account,
        "Destination": destination,
        "Amount": amount,
        "Fee": fee,
    })
}

// -- preflight tests --

#[test]
fn preflight_missing_destination() {
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": SRC_ADDRESS,
        "Amount": "1000000",
    });
    let rules = Rules::new();
    let fees = FeeSettings::default();
    let ctx = PreflightContext {
        tx: &tx,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.preflight(&ctx);
    assert_eq!(result, Err(TransactionResult::TemDstIsObligatory));
}

#[test]
fn preflight_missing_amount() {
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": SRC_ADDRESS,
        "Destination": DST_ADDRESS,
    });
    let rules = Rules::new();
    let fees = FeeSettings::default();
    let ctx = PreflightContext {
        tx: &tx,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.preflight(&ctx);
    assert_eq!(result, Err(TransactionResult::TemBadAmount));
}

#[test]
fn preflight_zero_amount() {
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "0", "10");
    let rules = Rules::new();
    let fees = FeeSettings::default();
    let ctx = PreflightContext {
        tx: &tx,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.preflight(&ctx);
    assert_eq!(result, Err(TransactionResult::TemBadAmount));
}

#[test]
fn preflight_self_payment() {
    let tx = make_payment_tx(SRC_ADDRESS, SRC_ADDRESS, "1000000", "10");
    let rules = Rules::new();
    let fees = FeeSettings::default();
    let ctx = PreflightContext {
        tx: &tx,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.preflight(&ctx);
    assert_eq!(result, Err(TransactionResult::TemBadSend));
}

#[test]
fn preflight_valid() {
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();
    let fees = FeeSettings::default();
    let ctx = PreflightContext {
        tx: &tx,
        rules: &rules,
        fees: &fees,
    };
    assert!(PaymentTransactor.preflight(&ctx).is_ok());
}

// -- preclaim tests --

#[test]
fn preclaim_source_not_found() {
    let ledger = Ledger::genesis();
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();
    let ctx = PreclaimContext {
        tx: &tx,
        view: &view,
        rules: &rules,
    };
    let result = PaymentTransactor.preclaim(&ctx);
    assert_eq!(result, Err(TransactionResult::TerNoAccount));
}

#[test]
fn preclaim_insufficient_balance() {
    let ledger = setup_ledger_with_account(SRC_ADDRESS, 500);
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();
    let ctx = PreclaimContext {
        tx: &tx,
        view: &view,
        rules: &rules,
    };
    let result = PaymentTransactor.preclaim(&ctx);
    assert_eq!(result, Err(TransactionResult::TecUnfundedPayment));
}

#[test]
fn preclaim_valid_with_existing_destination() {
    let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
    add_account(&mut ledger, DST_ADDRESS, 5_000_000);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();
    let ctx = PreclaimContext {
        tx: &tx,
        view: &view,
        rules: &rules,
    };
    assert!(PaymentTransactor.preclaim(&ctx).is_ok());
}

#[test]
fn preclaim_valid_create_account() {
    let ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();
    let ctx = PreclaimContext {
        tx: &tx,
        view: &view,
        rules: &rules,
    };
    assert!(PaymentTransactor.preclaim(&ctx).is_ok());
}

// -- apply tests --

#[test]
fn apply_transfer_to_existing_account() {
    let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
    add_account(&mut ledger, DST_ADDRESS, 5_000_000);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();

    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };

    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify source balance decreased and sequence incremented
    let src_id = decode_account_id(SRC_ADDRESS).unwrap();
    let src_key = keylet::account(&src_id);
    let src_bytes = sandbox.read(&src_key).unwrap();
    let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
    assert_eq!(src["Balance"].as_str().unwrap(), "9000000");
    assert_eq!(src["Sequence"].as_u64().unwrap(), 2);

    // Verify destination balance increased
    let dst_id = decode_account_id(DST_ADDRESS).unwrap();
    let dst_key = keylet::account(&dst_id);
    let dst_bytes = sandbox.read(&dst_key).unwrap();
    let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
    assert_eq!(dst["Balance"].as_str().unwrap(), "6000000");
}

#[test]
fn apply_xrp_payment_with_ticket_consumes_ticket_not_sequence() {
    // Source owns one ticket (OwnerCount=1) and pays via TicketSequence
    // instead of Sequence; the ticket must be consumed and the account
    // Sequence left untouched.
    let mut ledger = Ledger::genesis();
    let src_id = decode_account_id(SRC_ADDRESS).unwrap();
    let ticket_seq = 7u32;
    let src = serde_json::json!({
        "LedgerEntryType": "AccountRoot",
        "Account": SRC_ADDRESS,
        "Balance": "10000000",
        "Sequence": 9,
        "OwnerCount": 1,
        "Flags": 0,
    });
    ledger
        .put_state(keylet::account(&src_id), serde_json::to_vec(&src).unwrap())
        .unwrap();
    let ticket_key = keylet::ticket(&src_id, ticket_seq);
    let ticket = serde_json::json!({
        "LedgerEntryType": "Ticket",
        "Account": SRC_ADDRESS,
        "TicketSequence": ticket_seq,
        "Flags": 0,
    });
    ledger
        .put_state(ticket_key, serde_json::to_vec(&ticket).unwrap())
        .unwrap();
    add_account(&mut ledger, DST_ADDRESS, 5_000_000);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let mut tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    tx["TicketSequence"] = serde_json::json!(ticket_seq);
    let rules = Rules::new();

    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    let src_bytes = sandbox.read(&keylet::account(&src_id)).unwrap();
    let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
    // Sequence unchanged, ticket consumed (OwnerCount back to 0, SLE gone).
    assert_eq!(src["Sequence"].as_u64().unwrap(), 9);
    assert_eq!(src["OwnerCount"].as_u64().unwrap(), 0);
    assert!(!sandbox.exists(&keylet::ticket(&src_id, ticket_seq)));
}

#[test]
fn apply_creates_new_destination_account() {
    // Funding a new account requires sending at least account_reserve.
    let ledger = setup_ledger_with_account(SRC_ADDRESS, 50_000_000);
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "10000000", "10");
    let rules = Rules::new();

    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };

    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify destination was created with correct fields
    let dst_id = decode_account_id(DST_ADDRESS).unwrap();
    let dst_key = keylet::account(&dst_id);
    let dst_bytes = sandbox.read(&dst_key).unwrap();
    let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
    assert_eq!(dst["Balance"].as_str().unwrap(), "10000000");
    assert_eq!(dst["LedgerEntryType"].as_str().unwrap(), "AccountRoot");
    assert_eq!(dst["Sequence"].as_u64().unwrap(), 1);
    assert_eq!(dst["OwnerCount"].as_u64().unwrap(), 0);
}

#[test]
fn apply_below_reserve_fails_to_create_destination() {
    let ledger = setup_ledger_with_account(SRC_ADDRESS, 50_000_000);
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    // 1 drop is way below the 10 XRP reserve.
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1", "10");
    let rules = Rules::new();

    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };

    let result = PaymentTransactor.apply(&mut ctx);
    assert_eq!(result, Err(TransactionResult::TecNoDstInsuf));
}

#[test]
fn apply_insufficient_balance() {
    let ledger = setup_ledger_with_account(SRC_ADDRESS, 500);
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();

    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };

    let result = PaymentTransactor.apply(&mut ctx);
    assert_eq!(result, Err(TransactionResult::TecUnfundedPayment));
}

#[test]
fn apply_source_not_found() {
    let ledger = Ledger::genesis();
    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
    let rules = Rules::new();

    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };

    let result = PaymentTransactor.apply(&mut ctx);
    assert_eq!(result, Err(TransactionResult::TerNoAccount));
}

// -- transfer rate / cross-currency tests --

const ISSUER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
const ALICE: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
const BOB: &str = "rGWrZyQqhTp9Xu7G5Pkayo7bXjH4k4QYpf";
const MM: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";

fn put_account(ledger: &mut Ledger, addr: &str, balance: &str, transfer_rate: Option<u64>) {
    let id = decode_account_id(addr).unwrap();
    let key = keylet::account(&id);
    let mut acct = serde_json::json!({
        "LedgerEntryType": "AccountRoot",
        "Account": addr,
        "Balance": balance,
        "Sequence": 1,
        "OwnerCount": 0,
        "Flags": 0,
    });
    if let Some(rate) = transfer_rate {
        acct["TransferRate"] = serde_json::Value::from(rate);
    }
    ledger
        .put_state(key, serde_json::to_vec(&acct).unwrap())
        .unwrap();
}

/// Insert a RippleState giving `holder` a positive `value` of `currency`
/// from `issuer`.
fn put_trust_line(ledger: &mut Ledger, holder: &str, issuer: &str, currency: &str, value: f64) {
    let holder_id = decode_account_id(holder).unwrap();
    let issuer_id = decode_account_id(issuer).unwrap();
    let cur_bytes = helpers::currency_to_bytes(currency);
    let key = keylet::trust_line(&holder_id, &issuer_id, &cur_bytes);

    let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
    let stored = if holder_is_low { value } else { -value };
    let (low_addr, high_addr) = if holder_is_low {
        (holder, issuer)
    } else {
        (issuer, holder)
    };
    let tl = serde_json::json!({
        "LedgerEntryType": "RippleState",
        "Balance": { "currency": currency, "issuer": issuer, "value": format!("{stored}") },
        "LowLimit": { "currency": currency, "issuer": low_addr, "value": "0" },
        "HighLimit": { "currency": currency, "issuer": high_addr, "value": "1000" },
        "Flags": 0,
    });
    ledger
        .put_state(key, serde_json::to_vec(&tl).unwrap())
        .unwrap();
}

fn iou(currency: &str, issuer: &str, value: &str) -> serde_json::Value {
    serde_json::json!({ "currency": currency, "issuer": issuer, "value": value })
}

fn holder_balance(view: &dyn ReadView, holder: &str, issuer: &str, currency: &str) -> f64 {
    let holder_id = decode_account_id(holder).unwrap();
    let issuer_id = decode_account_id(issuer).unwrap();
    let cur_bytes = helpers::currency_to_bytes(currency);
    let key = keylet::trust_line(&holder_id, &issuer_id, &cur_bytes);
    let bytes = view.read(&key).unwrap();
    let tl: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    compute_holder_balance(&tl, &issuer_id, &holder_id)
}

#[test]
fn apply_iou_transfer_rate_deducts_fee_from_source() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", Some(1_200_000_000));
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "USD", 0.0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("USD", ISSUER, "50"),
        "SendMax": iou("USD", ISSUER, "60"),
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 40.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "USD") - 50.0).abs() < 1e-6);
}

#[test]
fn apply_iou_transfer_rate_send_max_too_low_fails() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", Some(1_200_000_000));
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "USD", 0.0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("USD", ISSUER, "50"),
        "SendMax": iou("USD", ISSUER, "55"),
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx);
    assert_eq!(result, Err(TransactionResult::TecPathPartial));
}

#[test]
fn apply_cross_currency_consumes_offer() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);

    // MM offer: TakerPays USD 50, TakerGets EUR 50.
    let mm_id = decode_account_id(MM).unwrap();
    let offer_key = keylet::offer(&mm_id, 1);
    let offer = serde_json::json!({
        "LedgerEntryType": "Offer",
        "Account": MM,
        "Sequence": 1,
        "TakerPays": iou("USD", ISSUER, "50"),
        "TakerGets": iou("EUR", ISSUER, "50"),
        "Flags": 0,
    });
    ledger
        .put_state(offer_key, serde_json::to_vec(&offer).unwrap())
        .unwrap();

    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let book_root = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
    let dir = serde_json::json!({
        "LedgerEntryType": "DirectoryNode",
        "Indexes": [offer_key.to_string()],
        "IndexNext": 0,
    });
    ledger
        .put_state(book_root, serde_json::to_vec(&dir).unwrap())
        .unwrap();

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("EUR", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    assert!((holder_balance(&sandbox, BOB, ISSUER, "EUR") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 80.0).abs() < 1e-6);
}

// -- multi-hop walker --
//
// A USD→EUR→GBP payment with no direct USD/GBP book walks two book
// crossings chained via an intermediate EUR position. `apply_cross_currency`
// dispatches to `apply_two_hop_payment` when the transaction's `Paths`
// field carries exactly one path with a single currency-change step
// (`type == 0x30`). The hop sequence:
//   * Hop 1 (USD → EUR): consume offers from `book_dir(USD, EUR)` until
//     we've sourced enough EUR to feed the next hop.
//   * Hop 2 (EUR → GBP): consume offers from `book_dir(EUR, GBP)` until
//     the destination's GBP target is met.
// Both hops must complete within `SendMax` worth of source currency or
// the payment fails `TecPathPartial`.
const MM2: &str = "rwUVoVMSURqNyvocPCcvLu3ygJzZyw8qwp";

#[test]
fn apply_cross_currency_two_hop_via_paths_succeeds() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

    // MM1: TakerPays 50 USD, TakerGets 50 EUR.
    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("EUR", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    // MM2: TakerPays 50 EUR, TakerGets 50 GBP.
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm2_offer = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("EUR", ISSUER, "50"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let gbp = helpers::currency_to_bytes("GBP");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let usd_eur_book = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
    let eur_gbp_book = keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id);
    ledger
        .put_state(
            usd_eur_book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    ledger
        .put_state(
            eur_gbp_book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm2_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    // USD → GBP, single intermediate EUR step in Paths.
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        "Paths": [
            [
                { "currency": "EUR", "issuer": ISSUER, "type": 0x30 }
            ]
        ],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Trust-line deltas: Alice -20 USD, MM +20 USD / -20 EUR,
    // MM2 +20 EUR / -20 GBP, Bob +20 GBP.
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "GBP") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
}

// -- N-hop walker (Phase 2) --

const MM3: &str = "rMNBtf9PFe7cbij413s1CLAwejjWYB7VnR";

/// Two MMs both publish their own intermediate-currency offers and the
/// transaction's `Paths` field lists both as alternatives. The walker
/// must try each in order and route through the first viable Path.
///
/// Sets up:
///   * MM1 offers USD/EUR -> EUR/GBP (a usable 2-hop chain)
///   * MM2 offers USD/CHF only (a dead-end alternative; the EUR-step
///     book never gets crossed because CHF/GBP has no offers)
///
/// Asserts that the EUR alternative wins despite being listed second.
#[test]
fn apply_cross_currency_two_hop_picks_viable_alternative_path() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("EUR", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm2_offer = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("EUR", ISSUER, "50"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let gbp = helpers::currency_to_bytes("GBP");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let usd_eur_book = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
    let eur_gbp_book = keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id);
    ledger
        .put_state(
            usd_eur_book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    ledger
        .put_state(
            eur_gbp_book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm2_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    // First Path lists CHF, which has no books wired up — the walker
    // must back-solve to TecPathPartial and try the second (EUR) Path.
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        "Paths": [
            [ { "currency": "CHF", "issuer": ISSUER, "type": 0x30 } ],
            [ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ]
        ],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
}

/// Three intermediate currencies (USD->EUR->JPY->CHF->GBP, hop count 4)
/// exercises the back-solve loop and forward mutation walk past the
/// hard-coded two-hop unfold. Every market-maker holds full inventory
/// of its sell-side currency, so the path is fully liquid.
#[test]
fn apply_cross_currency_four_hop_via_three_intermediates() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);
    put_account(&mut ledger, MM3, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "JPY", 100.0);
    put_trust_line(&mut ledger, MM3, ISSUER, "JPY", 0.0);
    put_trust_line(&mut ledger, MM3, ISSUER, "GBP", 100.0);

    let mut offer_keys = Vec::new();
    for (mm, seq, pays_cur, gets_cur) in [
        (MM, 1, "USD", "EUR"),
        (MM2, 1, "EUR", "JPY"),
        (MM3, 1, "JPY", "GBP"),
    ] {
        let mm_id = decode_account_id(mm).unwrap();
        let key = keylet::offer(&mm_id, seq);
        ledger
            .put_state(
                key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": mm,
                    "Sequence": seq,
                    "TakerPays": iou(pays_cur, ISSUER, "50"),
                    "TakerGets": iou(gets_cur, ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        offer_keys.push((key, pays_cur, gets_cur));
    }
    let issuer_id = decode_account_id(ISSUER).unwrap();
    for (key, pays_cur, gets_cur) in &offer_keys {
        let pays = helpers::currency_to_bytes(pays_cur);
        let gets = helpers::currency_to_bytes(gets_cur);
        let book = keylet::book_dir(&pays, &issuer_id, &gets, &issuer_id);
        ledger
            .put_state(
                book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [key.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
    }

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        "Paths": [[
            { "currency": "EUR", "issuer": ISSUER, "type": 0x30 },
            { "currency": "JPY", "issuer": ISSUER, "type": 0x30 }
        ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Trust-line deltas: ALICE -20 USD, BOB +20 GBP, each MM +20/-20.
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "JPY") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM3, ISSUER, "JPY") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM3, ISSUER, "GBP") - 80.0).abs() < 1e-6);
}

/// Two viable paths reach the same destination at different costs. The
/// walker must dry-run both and commit the cheaper one (lower
/// `src_spent`).
///
/// Setup:
///   * Path A via EUR — MM offers USD/EUR at parity, then MM2 offers
///     EUR/GBP at parity. Cost to deliver 20 GBP: 20 USD.
///   * Path B via JPY — MM offers USD/JPY at 1:16 (16 JPY per USD) and
///     MM3 offers JPY/GBP at 4:1, so 5 USD buys 80 JPY which buys
///     20 GBP. Cost: 5 USD.
///
/// First-viable-wins selection (PR #106) would pick Path A because it
/// is listed first; the quality-ranked dispatch picks Path B.
#[test]
fn apply_cross_currency_picks_cheapest_path_via_quality_ranking() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);
    put_account(&mut ledger, MM3, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    // Path A: USD -> EUR -> GBP at parity (rate 1.0 throughout).
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);
    // Path B: USD -> JPY -> GBP. MM gives 4 JPY for every USD; MM3 gives
    // 1 GBP for every JPY.  Net rate USD->GBP = 4x cheaper than parity.
    put_trust_line(&mut ledger, MM, ISSUER, "JPY", 200.0);
    put_trust_line(&mut ledger, MM3, ISSUER, "JPY", 0.0);
    put_trust_line(&mut ledger, MM3, ISSUER, "GBP", 100.0);

    let issuer_id = decode_account_id(ISSUER).unwrap();
    let mm_id = decode_account_id(MM).unwrap();
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm3_id = decode_account_id(MM3).unwrap();

    // MM offer 1: USD/EUR at 1:1.
    let mm_eur = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_eur,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("EUR", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    // MM2 offer: EUR/GBP at 1:1.
    let mm2_gbp = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_gbp,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("EUR", ISSUER, "50"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    // MM offer 2: USD/JPY at 1:16 (16 JPY per USD).
    let mm_jpy = keylet::offer(&mm_id, 2);
    ledger
        .put_state(
            mm_jpy,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 2,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("JPY", ISSUER, "800"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    // MM3 offer: JPY/GBP at 4:1 (1 GBP per 4 JPY).
    // We want 20 GBP from MM3 in exchange for 80 JPY in.
    let mm3_gbp = keylet::offer(&mm3_id, 1);
    ledger
        .put_state(
            mm3_gbp,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM3,
                "Sequence": 1,
                "TakerPays": iou("JPY", ISSUER, "200"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let gbp = helpers::currency_to_bytes("GBP");
    let jpy = helpers::currency_to_bytes("JPY");

    let usd_eur = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
    let eur_gbp = keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id);
    let usd_jpy = keylet::book_dir(&usd, &issuer_id, &jpy, &issuer_id);
    let jpy_gbp = keylet::book_dir(&jpy, &issuer_id, &gbp, &issuer_id);

    for (book, offer_key) in [
        (usd_eur, mm_eur),
        (eur_gbp, mm2_gbp),
        (usd_jpy, mm_jpy),
        (jpy_gbp, mm3_gbp),
    ] {
        ledger
            .put_state(
                book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [offer_key.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
    }

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        "Paths": [
            [ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ],
            [ { "currency": "JPY", "issuer": ISSUER, "type": 0x30 } ]
        ],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Quality-ranked dispatch picks Path B (JPY pivot).  Alice spends
    // only 5 USD; the EUR path would have cost 20 USD.
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 95.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
    // JPY books were the ones consumed; EUR books should be untouched.
    assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 100.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 0.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "JPY") - 120.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM3, ISSUER, "JPY") - 80.0).abs() < 1e-6);
}

// -- Mixed-issuer hops (Phase 3c) --

const ISSUER2: &str = "rJrxi4Wxev4bnAGVNP9YCdKPdAoKfAmcsi";

/// A Path step with `type == 0x10` carries a currency change but no
/// issuer field; the walker must inherit the issuer from the previous
/// hop (source side here). Hop chain: USD@ISSUER -> EUR@ISSUER.
/// Verifies the inheritance behavior on a single step.
#[test]
fn apply_cross_currency_inherits_issuer_from_source_side() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);

    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("EUR", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let book = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
    ledger
        .put_state(
            book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("EUR", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        // type == 0x10 → currency-only step; issuer is inherited from
        // the source side (ISSUER) and produces the same hop chain as
        // the 0x30 form `{ "currency": "EUR", "issuer": ISSUER }`.
        "Paths": [[ { "currency": "EUR", "type": 0x10 } ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "EUR") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
}

/// Cross-issuer chain: USD@ISSUER -> USD@ISSUER2 (issuer-only step
/// `type == 0x20`, currency inherited from source) -> EUR@ISSUER2
/// (final hop into the destination Amount). Exercises both inheritance
/// directions and the `book_dir(pays_cur, pays_iss, gets_cur, gets_iss)`
/// lookup with `pays_cur == gets_cur` but distinct issuers.
#[test]
fn apply_cross_currency_two_hop_mixed_issuer() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ISSUER2, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER2, "EUR", 0.0);
    // MM accepts USD@ISSUER, gives USD@ISSUER2.
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER2, "USD", 100.0);
    // MM2 accepts USD@ISSUER2, gives EUR@ISSUER2.
    put_trust_line(&mut ledger, MM2, ISSUER2, "USD", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER2, "EUR", 100.0);

    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("USD", ISSUER2, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm2_offer = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER2, "50"),
                "TakerGets": iou("EUR", ISSUER2, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let issuer2_id = decode_account_id(ISSUER2).unwrap();
    let usd_usd2 = keylet::book_dir(&usd, &issuer_id, &usd, &issuer2_id);
    let usd2_eur2 = keylet::book_dir(&usd, &issuer2_id, &eur, &issuer2_id);
    ledger
        .put_state(
            usd_usd2,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    ledger
        .put_state(
            usd2_eur2,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm2_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("EUR", ISSUER2, "20"),
        "SendMax": iou("USD", ISSUER, "20"),
        // Single intermediate USD@ISSUER2 expressed with an issuer-only
        // step (`type == 0x20`); currency stays USD via inheritance. The
        // destination Amount (EUR@ISSUER2) closes the chain — the walker
        // appends it as the final hop boundary automatically.
        "Paths": [[
            { "issuer": ISSUER2, "type": 0x20 }
        ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert!((holder_balance(&sandbox, BOB, ISSUER2, "EUR") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER2, "USD") - 80.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER2, "USD") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER2, "EUR") - 80.0).abs() < 1e-6);
}

// -- DeliverMin / tfPartialPayment (Phase 4) --

const TF_PARTIAL_PAYMENT_TEST: u32 = 0x0002_0000;

/// SendMax is half of what's needed to deliver the full Amount. Without
/// `tfPartialPayment` the walker returns TecPathPartial. With the flag
/// set and a DeliverMin <= the achievable amount, the walker scales the
/// flow down linearly and delivers exactly half.
#[test]
fn apply_cross_currency_partial_payment_with_deliver_min() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("EUR", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm2_offer = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("EUR", ISSUER, "50"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let gbp = helpers::currency_to_bytes("GBP");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    for (book, key) in [
        (
            keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id),
            mm_offer,
        ),
        (
            keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id),
            mm2_offer,
        ),
    ] {
        ledger
            .put_state(
                book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [key.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
    }

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    // Want 20 GBP but only willing to spend 10 USD (need 20 USD for the
    // full target). Partial mode with DeliverMin = 5 GBP allows the
    // walker to deliver 10 GBP (the max within SendMax).
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "10"),
        "DeliverMin": iou("GBP", ISSUER, "5"),
        "Flags": TF_PARTIAL_PAYMENT_TEST,
        "Paths": [[ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Alice spent SendMax (10 USD), Bob received scaled half (10 GBP).
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 90.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 10.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 10.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 90.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 10.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "GBP") - 90.0).abs() < 1e-6);
}

/// Same setup as above but without the `tfPartialPayment` flag: the
/// walker must reject the under-funded payment with TecPathPartial.
#[test]
fn apply_cross_currency_partial_disabled_returns_tec_path_partial() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);
    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "50"),
                "TakerGets": iou("EUR", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm2_offer = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("EUR", ISSUER, "50"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let eur = helpers::currency_to_bytes("EUR");
    let gbp = helpers::currency_to_bytes("GBP");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    for (book, key) in [
        (
            keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id),
            mm_offer,
        ),
        (
            keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id),
            mm2_offer,
        ),
    ] {
        ledger
            .put_state(
                book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [key.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
    }

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "20"),
        "SendMax": iou("USD", ISSUER, "10"),
        // No DeliverMin, no tfPartialPayment.
        "Paths": [[ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx);
    assert_eq!(result, Err(TransactionResult::TecPathPartial));
}

// -- Account-rippling (Phase 3b) --

/// Two pure account-rippling hops chained: USD@ISSUER → MM → MM2,
/// with BOB holding (BOB, MM2, USD). No order books are crossed; each
/// hop just absorbs IOUs into the next account's trust line with the
/// inbound issuer.
#[test]
fn apply_cross_currency_ripple_two_hop_via_paths_succeeds() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM2, MM, "USD", 0.0);
    put_trust_line(&mut ledger, BOB, MM2, "USD", 0.0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("USD", MM2, "30"),
        "SendMax": iou("USD", ISSUER, "30"),
        "Paths": [[
            { "account": MM, "type": 0x01 },
            { "account": MM2, "type": 0x01 }
        ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Alice's USD@ISSUER trust line drained by the source spend; each
    // rippling account picks up that amount against its inbound issuer.
    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 70.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 30.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, MM, "USD") - 30.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, MM2, "USD") - 30.0).abs() < 1e-6);
}

/// Mixed strand: a ripple hop followed by a book hop. ALICE pays USD@A,
/// MM rippling-account absorbs it as (MM, ISSUER, USD), then an offer
/// owned by MM2 sells GBP@ISSUER for USD@MM — crossing the book moves
/// the GBP to BOB.
#[test]
fn apply_cross_currency_ripple_then_book_succeeds() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);

    // Source side trust line + ripple account's inbound trust line.
    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    // Book side: MM2 holds GBP@ISSUER and USD@MM trust lines.
    put_trust_line(&mut ledger, MM2, MM, "USD", 0.0);
    put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);
    // Destination's inbound trust line for GBP@ISSUER.
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);

    // MM2 offers USD@MM ↔ GBP@ISSUER at 1:1.
    let mm2_id = decode_account_id(MM2).unwrap();
    let mm2_offer = keylet::offer(&mm2_id, 1);
    ledger
        .put_state(
            mm2_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM2,
                "Sequence": 1,
                "TakerPays": iou("USD", MM, "50"),
                "TakerGets": iou("GBP", ISSUER, "50"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let gbp = helpers::currency_to_bytes("GBP");
    let mm_id = decode_account_id(MM).unwrap();
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let usd_mm_gbp_book = keylet::book_dir(&usd, &mm_id, &gbp, &issuer_id);
    ledger
        .put_state(
            usd_mm_gbp_book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm2_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "25"),
        "SendMax": iou("USD", ISSUER, "25"),
        "Paths": [[
            { "account": MM, "type": 0x01 },
            { "currency": "GBP", "issuer": ISSUER, "type": 0x30 }
        ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 75.0).abs() < 1e-6);
    // MM absorbed the 25 USD@ISSUER inflow during the ripple step.
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 25.0).abs() < 1e-6);
    // MM2 received 25 USD@MM and gave up 25 GBP@ISSUER.
    assert!((holder_balance(&sandbox, MM2, MM, "USD") - 25.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, ISSUER, "GBP") - 75.0).abs() < 1e-6);
    // BOB credited the 25 GBP@ISSUER.
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 25.0).abs() < 1e-6);
}

/// A ripple path whose intermediate account lacks the inbound trust
/// line must be rejected, and the walker must fall back to a viable
/// alternative Path. Two alternatives: (a) ripple through MM3 (no
/// trust line — dead end) and (b) ripple through MM (live trust
/// line). MM2 is the destination-side issuer in both.
#[test]
fn apply_cross_currency_ripple_skips_path_without_trust_line() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);
    put_account(&mut ledger, MM2, "50000000", None);
    put_account(&mut ledger, MM3, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    // MM has the inbound trust line, MM3 does not.
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM2, MM, "USD", 0.0);
    put_trust_line(&mut ledger, BOB, MM2, "USD", 0.0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("USD", MM2, "15"),
        "SendMax": iou("USD", ISSUER, "15"),
        "Paths": [
            [
                { "account": MM3, "type": 0x01 },
                { "account": MM2, "type": 0x01 }
            ],
            [
                { "account": MM, "type": 0x01 },
                { "account": MM2, "type": 0x01 }
            ],
        ],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Routed via MM, leaving MM3's (non-existent) trust line untouched.
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 15.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM2, MM, "USD") - 15.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, BOB, MM2, "USD") - 15.0).abs() < 1e-6);
}

// -- AMM strand (Phase 5a) --

#[allow(clippy::too_many_arguments)]
fn put_amm(
    ledger: &mut Ledger,
    asset_cur: &str,
    asset_iss: &str,
    asset2_cur: &str,
    asset2_iss: &str,
    pool1: u64,
    pool2: u64,
    trading_fee_bps: u32,
) -> rxrpl_primitives::Hash256 {
    let cur1 = helpers::currency_to_bytes(asset_cur);
    let cur2 = helpers::currency_to_bytes(asset2_cur);
    let iss1 = decode_account_id(asset_iss).unwrap();
    let iss2 = decode_account_id(asset2_iss).unwrap();
    let key = rxrpl_protocol::keylet::amm(&cur1, &iss1, &cur2, &iss2);
    let amm = serde_json::json!({
        "LedgerEntryType": "AMM",
        "Asset": { "currency": asset_cur, "issuer": asset_iss },
        "Asset2": { "currency": asset2_cur, "issuer": asset2_iss },
        "PoolBalance1": pool1.to_string(),
        "PoolBalance2": pool2.to_string(),
        "LPTokenBalance": "1000",
        "TradingFee": trading_fee_bps,
        "Flags": 0,
    });
    ledger
        .put_state(key, serde_json::to_vec(&amm).unwrap())
        .unwrap();
    key
}

/// Single-hop cross-currency strand with no book offers — the AMM is
/// the sole liquidity source. Verifies that the walker quotes the
/// constant-product swap, debits the source, credits the destination,
/// and updates the pool balances on the SLE.
#[test]
fn apply_cross_currency_amm_only_strand_succeeds() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);

    // Pool keylet sorts assets canonically. GBP < USD bytewise, so
    // PoolBalance1 corresponds to GBP and PoolBalance2 to USD.
    let amm_key = put_amm(&mut ledger, "GBP", ISSUER, "USD", ISSUER, 1000, 1000, 0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "50"),
        "SendMax": iou("USD", ISSUER, "100"),
        "Paths": [[ { "currency": "GBP", "issuer": ISSUER, "type": 0x30 } ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Bob received the full 50 GBP. Alice paid the AMM quote
    // (52.63... USD, rounded by the AMM SLE's u64 storage).
    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 50.0).abs() < 1e-6);
    let alice_after = holder_balance(&sandbox, ALICE, ISSUER, "USD");
    assert!(alice_after < 100.0 - 52.0 && alice_after > 100.0 - 53.0);

    // Pool balances reflect the swap: GBP side drained by 50, USD
    // side topped up by the input that produced 50 GBP of output.
    let amm_bytes = sandbox.read(&amm_key).unwrap();
    let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
    let new_gbp: u64 = amm["PoolBalance1"].as_str().unwrap().parse().unwrap();
    let new_usd: u64 = amm["PoolBalance2"].as_str().unwrap().parse().unwrap();
    assert_eq!(new_gbp, 950);
    assert!((1052..=1053).contains(&new_usd));
}

/// A book that satisfies part of the target leaves the remainder to
/// the AMM. The MM offer sells 20 GBP for 20 USD; the strand needs
/// 30 GBP total, so 10 GBP comes from the constant-product pool.
#[test]
fn apply_cross_currency_book_then_amm_combined() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);
    put_account(&mut ledger, MM, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
    put_trust_line(&mut ledger, MM, ISSUER, "GBP", 100.0);

    // MM offers 20 USD for 20 GBP (1:1).
    let mm_id = decode_account_id(MM).unwrap();
    let mm_offer = keylet::offer(&mm_id, 1);
    ledger
        .put_state(
            mm_offer,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "Offer",
                "Account": MM,
                "Sequence": 1,
                "TakerPays": iou("USD", ISSUER, "20"),
                "TakerGets": iou("GBP", ISSUER, "20"),
                "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let usd = helpers::currency_to_bytes("USD");
    let gbp = helpers::currency_to_bytes("GBP");
    let issuer_id = decode_account_id(ISSUER).unwrap();
    let usd_gbp_book = keylet::book_dir(&usd, &issuer_id, &gbp, &issuer_id);
    ledger
        .put_state(
            usd_gbp_book,
            serde_json::to_vec(&serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Indexes": [mm_offer.to_string()],
                "IndexNext": 0,
            }))
            .unwrap(),
        )
        .unwrap();

    let amm_key = put_amm(&mut ledger, "GBP", ISSUER, "USD", ISSUER, 1000, 1000, 0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "30"),
        "SendMax": iou("USD", ISSUER, "100"),
        "Paths": [[ { "currency": "GBP", "issuer": ISSUER, "type": 0x30 } ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 30.0).abs() < 1e-6);
    // MM consumed the full offer: +20 USD, -20 GBP.
    assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
    assert!((holder_balance(&sandbox, MM, ISSUER, "GBP") - 80.0).abs() < 1e-6);
    // AMM supplied the residual 10 GBP via the pool.
    let amm_bytes = sandbox.read(&amm_key).unwrap();
    let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
    let new_gbp: u64 = amm["PoolBalance1"].as_str().unwrap().parse().unwrap();
    assert_eq!(new_gbp, 990);
}

/// When no AMM is registered for the pair and the book is empty, the
/// strand fails `TecPathPartial` — the AMM lookup is a pure read with
/// no side effects, so absent AMM falls through to the existing
/// insufficient-liquidity error path.
#[test]
fn apply_cross_currency_no_amm_no_book_fails_path_partial() {
    let mut ledger = Ledger::genesis();
    put_account(&mut ledger, ISSUER, "100000000", None);
    put_account(&mut ledger, ALICE, "50000000", None);
    put_account(&mut ledger, BOB, "50000000", None);

    put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
    put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);

    let fees = FeeSettings::default();
    let view = LedgerView::with_fees(&ledger, fees.clone());
    let mut sandbox = Sandbox::new(&view);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": ALICE,
        "Destination": BOB,
        "Amount": iou("GBP", ISSUER, "10"),
        "SendMax": iou("USD", ISSUER, "20"),
        "Paths": [[ { "currency": "GBP", "issuer": ISSUER, "type": 0x30 } ]],
        "Fee": "10",
    });
    let rules = Rules::new();
    let mut ctx = ApplyContext {
        tx: &tx,
        view: &mut sandbox,
        rules: &rules,
        fees: &fees,
    };
    let result = PaymentTransactor.apply(&mut ctx);
    assert_eq!(result, Err(TransactionResult::TecPathPartial));
}
