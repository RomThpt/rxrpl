use rxrpl_amendment::feature::feature_id;
use rxrpl_amount::number::RoundingMode;
use rxrpl_amount::{
    IOUAmount, from_rate, get_rate, offer_quality, offer_quality_round_even, round_quality,
};
use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};

use crate::helpers;
use crate::owner_dir::{add_to_book_dir, add_to_owner_dir};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

fn u64_hex(n: u64) -> String {
    format!("{n:016X}")
}

#[cfg(test)]
mod ledger_105504157_tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    #[test]
    fn offer_create_crosses_xah_xrp_bridge_and_direct_rlusd_amm() {
        let fixture: Value =
            serde_json::from_str(include_str!("ledger_105504157_fixture.json")).unwrap();
        let mut ledger = Ledger::genesis();
        for (key, entry) in fixture["entries"].as_object().unwrap() {
            ledger
                .put_state(key.parse().unwrap(), serde_json::to_vec(entry).unwrap())
                .unwrap();
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("FlowSortStrands"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("fixAMMv1_3"),
            feature_id("fixReducedOffersV1"),
            feature_id("fixReducedOffersV2"),
        ]);
        let tx = &fixture["tx"];
        let mut ctx = ApplyContext {
            tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let input = Leg::parse(&tx["TakerGets"]).unwrap();
        let output = Leg::parse(&tx["TakerPays"]).unwrap();
        let threshold = crossing_quality_threshold(&input, &output, true, false);
        let direct =
            build_flow_strand(&ctx, &[tx["TakerGets"].clone(), tx["TakerPays"].clone()]).unwrap();
        assert!(direct.hops[0].has_clob);
        let direct_tip = book_tip_quality(&ctx, &direct.hops[0].inverse_book).unwrap();
        assert!(reject_quality(direct_tip, threshold));
        assert!(direct.terminal_has_amm());
        let bridge = build_flow_strand(
            &ctx,
            &[
                tx["TakerGets"].clone(),
                Value::String("0".into()),
                tx["TakerPays"].clone(),
            ],
        )
        .unwrap();
        assert!(num_le(
            &offer_crossing_initial_bound(&ctx, &direct).unwrap(),
            &offer_crossing_initial_bound(&ctx, &bridge).unwrap(),
        ));
        let taker = decode_account_id(tx["Account"].as_str().unwrap()).unwrap();
        assert!(offer_crossing_strand_reaches_limit_with_amm_mode(
            &ctx,
            &taker,
            &tx["TakerGets"],
            &tx["TakerPays"],
            threshold,
            true,
        ));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        for node in fixture["tx"]["meta"]["AffectedNodes"].as_array().unwrap() {
            let modified = &node["ModifiedNode"];
            let kind = modified["LedgerEntryType"].as_str().unwrap();
            if kind != "RippleState" && kind != "Offer" && kind != "AccountRoot" {
                continue;
            }
            if kind == "AccountRoot" && modified["FinalFields"]["Account"] == tx["Account"] {
                continue;
            }
            let key: Hash256 = modified["LedgerIndex"].as_str().unwrap().parse().unwrap();
            let current: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            let expected = &modified["FinalFields"];
            if kind == "RippleState" {
                assert_eq!(
                    current["Balance"]["value"], expected["Balance"]["value"],
                    "trust line {key}"
                );
            } else if kind == "Offer" {
                assert_eq!(current["TakerGets"], expected["TakerGets"], "offer {key}");
                assert_eq!(current["TakerPays"], expected["TakerPays"], "offer {key}");
            } else {
                assert_eq!(current["Balance"], expected["Balance"], "maker {key}");
            }
        }
        let changes = sandbox.into_changes();
        let expected_keys: HashSet<Hash256> = fixture["tx"]["meta"]["AffectedNodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                node["ModifiedNode"]["LedgerIndex"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert_eq!(
            changes.updates.keys().copied().collect::<HashSet<_>>(),
            expected_keys
        );
        assert!(changes.inserts.is_empty());
        assert!(changes.deletes.is_empty());
    }

    #[test]
    fn ledger_105501838_bridge_only_keeps_xrp_offer_drop() {
        let fixture: Value =
            serde_json::from_str(include_str!("ledger_105501838_fixture.json")).unwrap();
        let mut ledger = Ledger::genesis();
        for (key, entry) in fixture["entries"].as_object().unwrap() {
            ledger
                .put_state(key.parse().unwrap(), serde_json::to_vec(entry).unwrap())
                .unwrap();
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("FlowSortStrands"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("fixAMMv1_3"),
            feature_id("fixReducedOffersV1"),
            feature_id("fixReducedOffersV2"),
        ]);
        let tx = &fixture["tx"];
        let mut ctx = ApplyContext {
            tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let direct =
            build_flow_strand(&ctx, &[tx["TakerGets"].clone(), tx["TakerPays"].clone()]).unwrap();
        let bridge = build_flow_strand(
            &ctx,
            &[
                tx["TakerGets"].clone(),
                Value::String("0".into()),
                tx["TakerPays"].clone(),
            ],
        )
        .unwrap();
        let direct_bound = offer_crossing_initial_bound(&ctx, &direct).unwrap();
        let bridge_bound = offer_crossing_initial_bound(&ctx, &bridge).unwrap();
        assert!(!num_le(&direct_bound, &bridge_bound));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        for key in [
            "22BE8C70663D60BA04CEE4D53689B2313D56CB0699774E14E08EE18759D81070",
            "C94457B9FF18C020673E1ECE05A141E3BB58CB0F82DF874C2AC2135E197080C0",
        ] {
            let key: Hash256 = key.parse().unwrap();
            let actual: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            let node = fixture["tx"]["meta"]["AffectedNodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["ModifiedNode"]["LedgerIndex"].as_str() == Some(&key.to_string()))
                .unwrap();
            let expected = &node["ModifiedNode"]["FinalFields"];
            assert_eq!(actual["TakerGets"], expected["TakerGets"], "offer {key}");
            assert_eq!(actual["TakerPays"], expected["TakerPays"], "offer {key}");
        }
        for node in fixture["tx"]["meta"]["AffectedNodes"].as_array().unwrap() {
            let modified = &node["ModifiedNode"];
            let kind = modified["LedgerEntryType"].as_str().unwrap();
            if kind == "Offer"
                || (kind == "AccountRoot" && modified["FinalFields"]["Account"] == tx["Account"])
            {
                continue;
            }
            let key: Hash256 = modified["LedgerIndex"].as_str().unwrap().parse().unwrap();
            let current: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            let expected = &modified["FinalFields"];
            if kind == "RippleState" {
                assert_eq!(current["Balance"], expected["Balance"], "trust line {key}");
            } else if kind == "AccountRoot" {
                assert_eq!(current["Balance"], expected["Balance"], "maker {key}");
            }
        }
        let changes = sandbox.into_changes();
        let expected_keys: HashSet<Hash256> = fixture["tx"]["meta"]["AffectedNodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                n["ModifiedNode"]["LedgerIndex"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert_eq!(
            changes.updates.keys().copied().collect::<HashSet<_>>(),
            expected_keys
        );
        assert!(changes.inserts.is_empty());
        assert!(changes.deletes.is_empty());
    }

    #[test]
    fn ledger_105500132_passive_offer_rests_without_crossing() {
        let fixture: Value =
            serde_json::from_str(include_str!("ledger_105500132_fixture.json")).unwrap();
        let mut ledger = Ledger::genesis();
        for (key, entry) in fixture["entries"].as_object().unwrap() {
            ledger
                .put_state(key.parse().unwrap(), serde_json::to_vec(entry).unwrap())
                .unwrap();
        }
        let fees = FeeSettings {
            base_fee: 10,
            reserve_base: 1_000_000,
            reserve_increment: 200_000,
        };
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("FlowSortStrands"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("fixAMMv1_3"),
            feature_id("fixReducedOffersV1"),
            feature_id("fixReducedOffersV2"),
        ]);
        let tx = &fixture["tx"];
        let mut ctx = ApplyContext {
            tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let direct =
            build_flow_strand(&ctx, &[tx["TakerGets"].clone(), tx["TakerPays"].clone()]).unwrap();
        let bridge = build_flow_strand(
            &ctx,
            &[
                tx["TakerGets"].clone(),
                Value::String("0".into()),
                tx["TakerPays"].clone(),
            ],
        )
        .unwrap();
        assert_eq!(tx["Flags"].as_u64().unwrap() & TF_PASSIVE, TF_PASSIVE);
        assert!(direct.hops[0].has_clob);
        assert!(direct.terminal_has_amm());
        assert!(!num_le(
            &offer_crossing_initial_bound(&ctx, &direct).unwrap(),
            &offer_crossing_initial_bound(&ctx, &bridge).unwrap(),
        ));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let created = fixture["tx"]["meta"]["AffectedNodes"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|n| {
                n.get("CreatedNode")
                    .filter(|v| v["LedgerEntryType"] == "Offer")
            })
            .unwrap();
        let key: Hash256 = created["LedgerIndex"].as_str().unwrap().parse().unwrap();
        let actual: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
        assert_eq!(actual["TakerGets"], created["NewFields"]["TakerGets"]);
        assert_eq!(actual["TakerPays"], created["NewFields"]["TakerPays"]);
        let changes = sandbox.into_changes();
        let mut expected_updates = HashSet::new();
        let mut expected_inserts = HashSet::new();
        for node in fixture["tx"]["meta"]["AffectedNodes"].as_array().unwrap() {
            if let Some(modified) = node.get("ModifiedNode") {
                expected_updates.insert(
                    modified["LedgerIndex"]
                        .as_str()
                        .unwrap()
                        .parse::<Hash256>()
                        .unwrap(),
                );
            } else if let Some(created) = node.get("CreatedNode") {
                expected_inserts.insert(
                    created["LedgerIndex"]
                        .as_str()
                        .unwrap()
                        .parse::<Hash256>()
                        .unwrap(),
                );
            }
        }
        assert_eq!(
            changes.updates.keys().copied().collect::<HashSet<_>>(),
            expected_updates
        );
        assert_eq!(
            changes.inserts.keys().copied().collect::<HashSet<_>>(),
            expected_inserts
        );
        assert!(changes.deletes.is_empty());
    }

    #[test]
    fn ledger_105504158_direct_book_and_amm_match_oracle() {
        let fixture: Value =
            serde_json::from_str(include_str!("ledger_105504158_fixture.json")).unwrap();
        // This historical amendment ID predates feature_id(name) derivation.
        let default_ripple: Hash256 =
            "1562511F573A19AE9BD103B5D6B9E01B3B46805AEC5D3C4805C902B514399146"
                .parse()
                .unwrap();
        let mut ledger = Ledger::genesis();
        for (key, entry) in fixture["entries"].as_object().unwrap() {
            ledger
                .put_state(key.parse().unwrap(), serde_json::to_vec(entry).unwrap())
                .unwrap();
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([
            default_ripple,
            feature_id("FlowCross"),
            feature_id("FlowSortStrands"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("fixAMMv1_3"),
            feature_id("fixReducedOffersV1"),
            feature_id("fixReducedOffersV2"),
        ]);
        let tx = &fixture["tx"];
        let mut ctx = ApplyContext {
            tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let strand =
            build_flow_strand(&ctx, &[tx["TakerGets"].clone(), tx["TakerPays"].clone()]).unwrap();
        assert!(first_fib_offer_beats_clob(&ctx, &strand));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let source_key =
            keylet::account(&decode_account_id(tx["Account"].as_str().unwrap()).unwrap());
        let mut expected_updates = HashSet::new();
        let mut expected_inserts = HashSet::new();
        for node in fixture["tx"]["meta"]["AffectedNodes"].as_array().unwrap() {
            let Some(entry) = node.get("ModifiedNode").or_else(|| node.get("CreatedNode")) else {
                continue;
            };
            let kind = entry["LedgerEntryType"].as_str().unwrap();
            let key: Hash256 = entry["LedgerIndex"].as_str().unwrap().parse().unwrap();
            if node.get("ModifiedNode").is_some() {
                expected_updates.insert(key);
            } else {
                expected_inserts.insert(key);
            }
            let actual: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            let expected = entry.get("FinalFields").unwrap_or(&entry["NewFields"]);
            match kind {
                "Offer" => {
                    assert_eq!(actual["TakerGets"], expected["TakerGets"], "offer {key}");
                    assert_eq!(actual["TakerPays"], expected["TakerPays"], "offer {key}");
                }
                "RippleState" => {
                    assert_eq!(actual["Flags"], expected["Flags"], "trust line {key}");
                    let parse_balance = |text: &str| {
                        if let Some((mantissa, exponent)) = text.split_once('e') {
                            let signed: i64 = mantissa.parse().unwrap();
                            IOUAmount::from_parts(
                                signed.unsigned_abs(),
                                exponent.parse().unwrap(),
                                signed.is_negative(),
                            )
                            .unwrap()
                        } else {
                            IOUAmount::from_decimal_string(text).unwrap()
                        }
                    };
                    let actual_balance =
                        parse_balance(actual["Balance"]["value"].as_str().unwrap());
                    let expected_balance =
                        parse_balance(expected["Balance"]["value"].as_str().unwrap());
                    assert_eq!(actual_balance, expected_balance, "trust line {key}");
                }
                "AccountRoot" => {
                    if let Some(balance) = expected.get("Balance") {
                        if key == source_key {
                            let fee: u64 = tx["Fee"].as_str().unwrap().parse().unwrap();
                            let actual_balance: u64 =
                                actual["Balance"].as_str().unwrap().parse().unwrap();
                            let expected_balance: u64 = balance.as_str().unwrap().parse().unwrap();
                            assert_eq!(actual_balance - fee, expected_balance, "account {key}");
                        } else {
                            assert_eq!(&actual["Balance"], balance, "account {key}");
                        }
                    }
                    if let Some(owner_count) = expected.get("OwnerCount") {
                        assert_eq!(&actual["OwnerCount"], owner_count, "account {key}");
                    }
                }
                _ => {}
            }
        }
        let changes = sandbox.into_changes();
        assert_eq!(
            changes.updates.keys().copied().collect::<HashSet<_>>(),
            expected_updates
        );
        assert_eq!(
            changes.inserts.keys().copied().collect::<HashSet<_>>(),
            expected_inserts
        );
        assert!(changes.deletes.is_empty());
    }

    #[test]
    fn passive_sell_crosses_tiny_xrp_bridge_before_resting() {
        let fixture: Value =
            serde_json::from_str(include_str!("ledger_105504193_fixture.json")).unwrap();
        let next_quality = "29958A9E8FDB4462AB486E269C735F52ADDAEDF354F9F40E4F03E9A2B7566C00";
        assert_eq!(
            fixture["entries"][next_quality]["Indexes"][0],
            "467FB20E63BE08A19C7B81EC75DABBBDA0D726FC120C4A72F906359C763D2538"
        );
        let mut ledger = Ledger::genesis();
        for (key, entry) in fixture["entries"].as_object().unwrap() {
            ledger
                .put_state(key.parse().unwrap(), serde_json::to_vec(entry).unwrap())
                .unwrap();
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("FlowSortStrands"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("fixAMMv1_3"),
            feature_id("fixReducedOffersV1"),
            feature_id("fixReducedOffersV2"),
        ]);
        let tx = &fixture["tx"];
        let mut ctx = ApplyContext {
            tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let taker = decode_account_id(tx["Account"].as_str().unwrap()).unwrap();
        let in_leg = Leg::parse(&tx["TakerGets"]).unwrap();
        let out_leg = Leg::parse(&tx["TakerPays"]).unwrap();
        let threshold = flow_crossing_threshold(&ctx, &taker, &in_leg, &out_leg, true, true);
        let direct =
            build_flow_strand(&ctx, &[tx["TakerGets"].clone(), tx["TakerPays"].clone()]).unwrap();
        let bridge = build_flow_strand(
            &ctx,
            &[
                tx["TakerGets"].clone(),
                Value::String("0".into()),
                tx["TakerPays"].clone(),
            ],
        )
        .unwrap();
        assert!(num_gt(
            &offer_crossing_initial_bound(&ctx, &direct).unwrap(),
            &Number::from_iou(&from_rate(threshold).unwrap()),
        ));
        assert!(num_le(
            &offer_crossing_initial_bound(&ctx, &bridge).unwrap(),
            &Number::from_iou(&from_rate(threshold).unwrap()),
        ));
        assert!(!offer_crossing_strand_reaches_limit_with_amm_mode(
            &ctx,
            &taker,
            &tx["TakerGets"],
            &tx["TakerPays"],
            threshold,
            true,
        ));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let consumed: Hash256 = "E869F2408F8351C1703E198B182446FB66C8A63A41959D164CA90DE218FFF742"
            .parse()
            .unwrap();
        assert!(
            sandbox.read(&consumed).is_none(),
            "bridge offer should be consumed"
        );
        for node in fixture["tx"]["meta"]["AffectedNodes"].as_array().unwrap() {
            let Some(entry) = node.get("ModifiedNode").or_else(|| node.get("CreatedNode")) else {
                continue;
            };
            let kind = entry["LedgerEntryType"].as_str().unwrap();
            if kind == "AccountRoot" && entry["FinalFields"]["Account"] == tx["Account"] {
                continue;
            }
            if kind != "AccountRoot" && kind != "RippleState" && kind != "Offer" {
                continue;
            }
            let key: Hash256 = entry["LedgerIndex"].as_str().unwrap().parse().unwrap();
            let actual: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            let expected = entry.get("FinalFields").unwrap_or(&entry["NewFields"]);
            match kind {
                "AccountRoot" => {
                    assert_eq!(actual["Balance"], expected["Balance"], "account {key}")
                }
                "RippleState" => {
                    assert_eq!(actual["Balance"], expected["Balance"], "trust line {key}")
                }
                "Offer" => {
                    assert_eq!(
                        actual["TakerGets"], expected["TakerGets"],
                        "offer gets {key}"
                    );
                    assert_eq!(
                        actual["TakerPays"], expected["TakerPays"],
                        "offer pays {key}"
                    );
                }
                _ => unreachable!(),
            }
        }
        let changes = sandbox.into_changes();
        for (kind, actual) in [
            ("ModifiedNode", &changes.updates),
            ("CreatedNode", &changes.inserts),
            ("DeletedNode", &changes.deletes),
        ] {
            let expected: HashSet<Hash256> = fixture["tx"]["meta"]["AffectedNodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|node| node.get(kind))
                .map(|entry| entry["LedgerIndex"].as_str().unwrap().parse().unwrap())
                .collect();
            assert_eq!(
                actual.keys().copied().collect::<HashSet<_>>(),
                expected,
                "{kind}"
            );
        }
    }
}

/// The order-book directory index for a given quality: the book base with its
/// low 64 bits replaced by the quality (rate).
pub(crate) fn book_dir_with_quality(book_base: &Hash256, quality: u64) -> Hash256 {
    let mut bytes = *book_base.as_bytes();
    bytes[24..32].copy_from_slice(&quality.to_be_bytes());
    Hash256::new(bytes)
}

/// Packed quality of the best (lowest-rate) funded page on the inverse book,
/// or `None` if the book is empty. Used by `tfLimitQuality` to reject a path
/// whose composite rate is worse than SendMax/Amount before any hop mutates.
fn peek_best_book_quality(
    view: &dyn crate::view::read_view::ReadView,
    budget_in: &Leg,
    demand_out: &Leg,
) -> Option<u64> {
    peek_book_quality_skip(view, budget_in, demand_out, 0)
}

fn peek_book_quality_skip(
    view: &dyn crate::view::read_view::ReadView,
    budget_in: &Leg,
    demand_out: &Leg,
    skip: usize,
) -> Option<u64> {
    let inverse_book = keylet::book_dir(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );
    let book_prefix = inverse_book.as_bytes()[0..24].to_vec();
    let mut probe = book_dir_with_quality(&inverse_book, 0);
    let mut seen = 0usize;
    while let Some(dir_key) = view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != book_prefix[..] {
            return None;
        }
        probe = dir_key;
        let Some(bytes) = view.read(&dir_key) else {
            continue;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let empty = dir
            .get("Indexes")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if empty {
            continue;
        }
        if seen == skip {
            return Some(u64::from_be_bytes(
                dir_key.as_bytes()[24..32].try_into().ok()?,
            ));
        }
        seen += 1;
    }
    None
}

/// TakerGets of the best resting offer on the inverse book, used to cap a
/// reverse CLOB quote when PartialPayment Amount exceeds book size.
fn peek_best_funded_offer(
    ctx: &mut ApplyContext<'_>,
    budget_in: &Leg,
    demand_out: &Leg,
) -> Option<Value> {
    peek_funded_offer_skip(ctx, budget_in, demand_out, 0)
}

fn peek_funded_offer_skip(
    ctx: &mut ApplyContext<'_>,
    budget_in: &Leg,
    demand_out: &Leg,
    skip: usize,
) -> Option<Value> {
    let inverse_book = keylet::book_dir(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );
    let book_prefix = inverse_book.as_bytes()[0..24].to_vec();
    let mut probe = book_dir_with_quality(&inverse_book, 0);
    let mut seen = 0usize;
    while let Some(dir_key) = ctx.view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != book_prefix[..] {
            return None;
        }
        probe = dir_key;
        let Some(bytes) = ctx.view.read(&dir_key) else {
            continue;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let page: Vec<Hash256> = dir
            .get("Indexes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().and_then(|s| s.parse().ok()))
                    .collect()
            })
            .unwrap_or_default();
        let mut page_hit = false;
        for key in page {
            let Some(ob) = ctx.view.read(&key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                continue;
            };
            let Some(gets) = Leg::parse(&offer["TakerGets"]) else {
                continue;
            };
            let Some(owner_s) = offer.get("Account").and_then(|v| v.as_str()) else {
                continue;
            };
            let Ok(owner) = decode_account_id(owner_s) else {
                continue;
            };
            let funds = owner_funds_leg(ctx, &owner, &gets);
            if funds.is_zero() {
                continue;
            }
            page_hit = true;
            if seen == skip {
                return Some(offer);
            }
        }
        if page_hit {
            seen += 1;
        }
    }
    None
}

fn peek_best_book_gets(
    ctx: &mut ApplyContext<'_>,
    budget_in: &Leg,
    demand_out: &Leg,
) -> Option<Leg> {
    let offer = peek_best_funded_offer(ctx, budget_in, demand_out)?;
    let gets = Leg::parse(&offer["TakerGets"])?;
    let owner_s = offer.get("Account").and_then(|v| v.as_str())?;
    let owner = decode_account_id(owner_s).ok()?;
    let funds = owner_funds_leg(ctx, &owner, &gets);
    Some(if leg_ge(&funds, &gets) { gets } else { funds })
}

fn peek_best_book_pays(
    ctx: &mut ApplyContext<'_>,
    budget_in: &Leg,
    demand_out: &Leg,
) -> Option<Leg> {
    let offer = peek_best_funded_offer(ctx, budget_in, demand_out)?;
    Leg::parse(&offer["TakerPays"])
}

/// Parse a decimal value string (e.g. `"277.167203027"`) into an `IOUAmount`,
/// normalising the mantissa into rippled's `[10^15, 10^16)` range.
fn iou_from_decimal(s: &str) -> Option<IOUAmount> {
    let negative = s.starts_with('-');
    let s = s.trim_start_matches(['-', '+']);
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
    let digits = format!("{int_part}{frac_part}");
    let mut mantissa: u128 = digits.parse().ok()?;
    if mantissa == 0 {
        return IOUAmount::from_parts(0, 0, false).ok();
    }
    let mut exponent: i32 = -(frac_part.len() as i32);
    while mantissa < 1_000_000_000_000_000 {
        mantissa *= 10;
        exponent -= 1;
    }
    while mantissa >= 10_000_000_000_000_000 {
        mantissa /= 10;
        exponent += 1;
    }
    IOUAmount::from_parts(mantissa as u64, exponent, negative).ok()
}

/// Convert a TakerPays/TakerGets amount (IOU object or XRP drops string) to an
/// `IOUAmount` for quality computation.
fn amount_to_iou(amount: &Value) -> Option<IOUAmount> {
    if let Some(drops) = amount.as_str() {
        // XRP: drops as an integer value.
        return iou_from_decimal(drops);
    }
    iou_from_decimal(amount.get("value").and_then(|v| v.as_str())?)
}

/// The offer's quality (rate) = TakerPays / TakerGets, encoded as rippled's
/// 64-bit rate. Returns 0 if either side cannot be parsed.
///
/// `number_switchover` selects rippled's `STAmount::divide` canonicalisation:
/// once `fixUniversalNumber` is active the quotient is reduced round-half-to-
/// even (`offer_quality_round_even`); before it, by truncation (`offer_quality`).
/// The book directory of a modern offer (e.g. `04F5F33…` landing on `…7EA4`)
/// only matches rippled under the even reduction.
fn offer_book_quality(taker_pays: &Value, taker_gets: &Value, number_switchover: bool) -> u64 {
    match (amount_to_iou(taker_pays), amount_to_iou(taker_gets)) {
        (Some(p), Some(g)) => {
            let q = if number_switchover {
                offer_quality_round_even(&p, &g)
            } else {
                offer_quality(&p, &g)
            };
            q.unwrap_or(0)
        }
        _ => 0,
    }
}

/// OfferCreate transaction flags.
const TF_PASSIVE: u64 = 0x0001_0000;
const TF_IMMEDIATE_OR_CANCEL: u64 = 0x0002_0000;
const TF_FILL_OR_KILL: u64 = 0x0004_0000;
/// tfSell flag: the offer is a sell, so TakerGets is the exact amount.
const TF_SELL: u64 = 0x0008_0000;
const TF_HYBRID: u64 = 0x0010_0000;

/// Offer ledger-entry flags. rippled translates the transaction `tfPassive` /
/// `tfSell` flags into these (note `lsfSell` differs from `tfSell`); no other
/// transaction flag is persisted on the Offer SLE.
const LSF_PASSIVE: u64 = 0x0001_0000;
const LSF_SELL: u64 = 0x0002_0000;

/// rippled's `Quality::kMaxTickSize` — no rounding at or above 16 digits.
const MAX_TICK_SIZE: u8 = 16;

/// Round a non-negative decimal magnitude to the nearest integer drop, ties to
/// even — rippled's `Number`-based `STAmount(XRP)` canonicalisation
/// (`Number::operator rep()`, "round towards nearest, and on tie towards even"),
/// not truncation.
fn round_drops_half_even(dec: &str) -> Option<u64> {
    let dec = dec.strip_prefix('-').unwrap_or(dec);
    let (int_str, frac_str) = dec.split_once('.').unwrap_or((dec, ""));
    let mut drops: u64 = int_str.parse().ok()?;
    if let Some(&first) = frac_str.as_bytes().first() {
        let first = first - b'0';
        let rest_nonzero = frac_str.as_bytes()[1..].iter().any(|&b| b != b'0');
        if first > 5 || (first == 5 && (rest_nonzero || drops & 1 == 1)) {
            drops = drops.checked_add(1)?;
        }
    }
    Some(drops)
}

/// Reserialize an IOU offer side with a recomputed magnitude, preserving its
/// currency/issuer. Returns `None` for XRP sides (no tick rounding applies to
/// a recomputed native amount here).
fn rebuild_iou(original: &Value, value: &IOUAmount) -> Option<Value> {
    // An XRP side is a bare drops string, not an IOU object: the tick
    // re-derivation yields a fractional drops magnitude which rippled rounds to
    // the nearest drop (round half to even, `STAmount(XRP)` via `Number`), not
    // truncates. Without this the re-derivation silently failed on XRP offers
    // and the tick snap was skipped.
    if original.is_string() {
        let drops = round_drops_half_even(&value.to_decimal_string())?;
        return Some(Value::String(drops.to_string()));
    }
    let obj = original.as_object()?;
    let mut out = obj.clone();
    out.insert("value".into(), Value::from(value.to_decimal_string()));
    Some(Value::Object(out))
}

/// Apply issuer `TickSize` rounding to the offer amounts before crossing,
/// matching rippled's OfferCreate. When an IOU side's issuer sets a TickSize
/// below the 16-digit maximum, the offer quality is rounded up to that many
/// significant digits and the non-fixed side re-derived, snapping the offer to
/// the issuer's price grid. Offers with no tick size are returned unchanged.
fn tick_round_amounts(
    ctx: &ApplyContext<'_>,
    taker_pays: &Value,
    taker_gets: &Value,
    is_sell: bool,
) -> (Value, Value) {
    let tick_of = |amount: &Value| -> u8 {
        let Some(issuer) = amount.get("issuer").and_then(|v| v.as_str()) else {
            return MAX_TICK_SIZE;
        };
        let Ok(id) = decode_account_id(issuer) else {
            return MAX_TICK_SIZE;
        };
        ctx.view
            .read(&keylet::account(&id))
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|a| a.get("TickSize").and_then(|v| v.as_u64()))
            .map(|t| t as u8)
            .unwrap_or(MAX_TICK_SIZE)
    };

    let tick = tick_of(taker_pays).min(tick_of(taker_gets));
    if tick >= MAX_TICK_SIZE {
        return (taker_pays.clone(), taker_gets.clone());
    }

    let (Some(p_iou), Some(g_iou)) = (amount_to_iou(taker_pays), amount_to_iou(taker_gets)) else {
        return (taker_pays.clone(), taker_gets.clone());
    };
    // The tick grid snaps the offer's quality, so it must be computed with the
    // same (amendment-gated) divide canonicalisation as the placed book rate.
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let quality = if number_switchover {
        offer_quality_round_even(&p_iou, &g_iou)
    } else {
        offer_quality(&p_iou, &g_iou)
    };
    let Ok(quality) = quality else {
        return (taker_pays.clone(), taker_gets.clone());
    };
    let Ok(rate) = from_rate(round_quality(quality, tick)) else {
        return (taker_pays.clone(), taker_gets.clone());
    };

    if is_sell {
        if let Some(new_pays) = IOUAmount::multiply(&g_iou, &rate)
            .ok()
            .and_then(|p| rebuild_iou(taker_pays, &p))
        {
            return (new_pays, taker_gets.clone());
        }
    } else {
        // Recompute TakerGets = TakerPays / rate. Post-fixUniversalNumber rippled
        // does this divide in `Number` (round half to even), matching the rounding
        // used for the tick-snapped quality above; the legacy divide truncates.
        let quot = if number_switchover {
            IOUAmount::divide_round_even(&p_iou, &rate)
        } else {
            IOUAmount::divide(&p_iou, &rate)
        };
        if let Some(new_gets) = quot.ok().and_then(|g| rebuild_iou(taker_gets, &g)) {
            return (taker_pays.clone(), new_gets);
        }
    }
    (taker_pays.clone(), taker_gets.clone())
}

/// OfferCreate transaction handler.
///
/// Places an order on the decentralized exchange.
pub struct OfferCreateTransactor;

impl Transactor for OfferCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if ctx.tx.get("TakerPays").is_none() {
            return Err(TransactionResult::TemBadOffer);
        }
        if ctx.tx.get("TakerGets").is_none() {
            return Err(TransactionResult::TemBadOffer);
        }

        // Cannot have both sides be XRP
        let pays_is_xrp = ctx.tx["TakerPays"].is_string();
        let gets_is_xrp = ctx.tx["TakerGets"].is_string();
        if pays_is_xrp && gets_is_xrp {
            return Err(TransactionResult::TemBadOffer);
        }

        // Amounts must be positive
        if pays_is_xrp {
            let amount: u64 = ctx.tx["TakerPays"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if amount == 0 {
                return Err(TransactionResult::TemBadOffer);
            }
        }
        if gets_is_xrp {
            let amount: u64 = ctx.tx["TakerGets"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if amount == 0 {
                return Err(TransactionResult::TemBadOffer);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        if !ctx.view.exists(&key) {
            return Err(TransactionResult::TerNoAccount);
        }

        // PermissionedDEX gates only domain-scoped offers (those carrying a
        // DomainID); an open offer trades on the public book unrestricted. rippled
        // authorises a domain offer only for members of the domain named by the
        // tx's DomainID (the DomainID is the PermissionedDomain object's index) —
        // the trader must hold an accepted credential matching the domain's
        // AcceptedCredentials. This has nothing to do with the traded assets'
        // issuers.
        if ctx.rules.enabled(&feature_id("PermissionedDEX")) {
            if let Some(domain_id) = ctx.tx.get("DomainID").and_then(|v| v.as_str()) {
                check_domain_membership(ctx, &account_id, domain_id)?;
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let acct_key = keylet::account(&account_id);

        // Read account
        let bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        // The offer's Sequence (and its keylet) is rippled's `getSeqProxy()`
        // value: the TicketSequence when the transaction spends a ticket,
        // otherwise the transaction's Sequence. It must come from the TX, not the
        // account Sequence — the engine already consumed the sender's
        // Sequence/Ticket centrally, so the account Sequence is one ahead.
        let sequence = helpers::tx_seq_proxy_value(ctx.tx);

        let (pays_currency, pays_issuer) = currency_and_issuer(&ctx.tx["TakerPays"]);
        let (gets_currency, gets_issuer) = currency_and_issuer(&ctx.tx["TakerGets"]);

        // Snap the offer to the issuer's TickSize grid before crossing, exactly
        // as rippled does: the placed rate and re-derived amounts both flow from
        // the rounded quality. No-op when neither issuer sets a tick size.
        let is_sell = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_SELL != 0;
        let (taker_pays, taker_gets) =
            tick_round_amounts(ctx, &ctx.tx["TakerPays"], &ctx.tx["TakerGets"], is_sell);

        // The sender's Sequence/Ticket and fee are consumed centrally by the
        // engine (parent sandbox) before doApply, so they are charged even when
        // this transaction ends in a tec claim below.

        // Unfunded check (rippled preclaim): an offer must be at least partially
        // funded in the asset it sells (TakerGets), else tecUNFUNDED_OFFER — fee
        // and sequence charged, no offer placed, no crossing. accountFunds: an
        // IOU side is the holder's trust-line balance (the issuer of its own
        // currency is always funded); an XRP side is the liquid balance above the
        // owner reserve (`xrpLiquid`).
        let owner_count = acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let funded = match Leg::parse(&taker_gets) {
            Some(g) if g.is_xrp => {
                // preclaim runs before the fee is taken; add it back to the
                // post-fee balance the parent sandbox left us.
                let pre_fee = helpers::get_balance(&acct).saturating_add(helpers::get_fee(ctx.tx));
                pre_fee > ctx.fees.account_reserve(owner_count)
            }
            Some(g) => !owner_funds_leg(ctx, &account_id, &g).is_zero(),
            None => true,
        };
        if !funded {
            let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, nb)
                .map_err(|_| TransactionResult::TemMalformed)?;
            return Ok(TransactionResult::TecUnfundedOffer);
        }

        // OfferSequence (cancel-and-replace): rippled deletes the account's own
        // offer at that sequence before crossing — removed from its book and
        // owner directories with the owner reserve released. It is not an error
        // if the offer is already gone. The OwnerCount decrement lands on `acct`
        // (written back below), not the view copy a generic reap would clobber.
        if let Some(cancel_seq) = ctx.tx.get("OfferSequence").and_then(|v| v.as_u64()) {
            let cancel_key = keylet::offer(&account_id, cancel_seq as u32);
            if let Some(old) = ctx
                .view
                .read(&cancel_key)
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .filter(|o| o.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("Offer"))
            {
                if let Some(book_dir) = old
                    .get("BookDirectory")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Hash256>().ok())
                {
                    remove_from_book_dir(ctx.view, &book_dir, &cancel_key)?;
                }
                crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &cancel_key)?;
                let _ = ctx.view.erase(&cancel_key);
                helpers::adjust_owner_count(&mut acct, -1);
            }
        }

        // Cross against the inverse book — existing offers where someone pays
        // our `TakerGets` to receive our `TakerPays` (book keyed by `(gets,
        // pays)`) — best price first, filling crossable offers and reaping
        // unfunded ones (rippled's Taker). Returns the taker's leftover.
        //
        // A domain-scoped OfferCreate (carrying a DomainID, without tfHybrid)
        // crosses ONLY the permissioned domain book: rippled excludes open-book
        // offers and AMM liquidity from domain crossing ("AMM doesn't support
        // domain books"; BookStep's tryAMM skips the pool when the book is
        // domain-scoped). A hybrid offer (tfHybrid) also trades the public book,
        // so it keeps the open path.
        let domain_id_hash: Option<rxrpl_primitives::Hash256> = ctx
            .tx
            .get("DomainID")
            .and_then(|v| v.as_str())
            .and_then(|s| <rxrpl_primitives::Hash256 as std::str::FromStr>::from_str(s).ok());
        let tx_is_hybrid =
            ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_HYBRID != 0;
        let tx_flags = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
        let domain_only = domain_id_hash.is_some() && !tx_is_hybrid;
        let inverse_book = match &domain_id_hash {
            Some(d) if domain_only => keylet::book_dir_domain(
                &gets_currency,
                &gets_issuer,
                &pays_currency,
                &pays_issuer,
                d,
            ),
            _ => keylet::book_dir(&gets_currency, &gets_issuer, &pays_currency, &pays_issuer),
        };
        // Flow fixes the source `sendMax` before walking the book. Removing one
        // of the taker's own inverse offers during the walk may release owner
        // reserve, but those newly-liquid funds are not available to this
        // crossing.
        let flow_source_budget = Leg::parse(&taker_gets).map(|gets| {
            let funds = owner_funds_leg(ctx, &account_id, &gets);
            let mut send_max = gets.clone();
            if !gets.is_xrp && account_id != gets.issuer {
                let rate = transfer_rate(ctx, &gets.issuer);
                send_max.iou = IOUAmount::mul_round(&gets.iou, &rate, true).unwrap_or(gets.iou);
            }
            (leg_min(&send_max, &funds), leg_ge(&send_max, &funds))
        });
        reap_own_unfunded_inverse_tip(
            ctx,
            &account_id,
            &mut acct,
            &inverse_book,
            &taker_pays,
            &taker_gets,
        )?;
        // rippled's mPriorBalance for the resting-offer reserve gate: the XRP
        // balance before the fee and before crossing. The engine deducted the
        // fee centrally and crossing has not run yet, so add the fee back.
        let prior_balance = helpers::get_balance(&acct).saturating_add(helpers::get_fee(ctx.tx));
        // rippled gates offer crossing on the `FlowCross` amendment
        // (`CreateOffer::cross`): the modern Flow/`BookStep` engine when enabled,
        // the legacy `Taker` (with its XRP autobridge) when not. Every mainnet
        // ledger past 2018 has `FlowCross` enabled, so the legacy arm is dead on
        // any recent replay; it exists for byte-exact pre-`FlowCross` ledgers.
        let (remaining_pays, remaining_gets, crossed, source_exhausted) =
            if ctx.rules.enabled(&feature_id("FlowCross")) {
                match cross_offers(
                    ctx,
                    &account_id,
                    &mut acct,
                    &taker_pays,
                    &taker_gets,
                    &inverse_book,
                    is_sell,
                    domain_only,
                    flow_source_budget
                        .as_ref()
                        .map(|(budget, _)| budget)
                        .ok_or(TransactionResult::TemBadOffer)?,
                    flow_source_budget
                        .as_ref()
                        .is_some_and(|(_, funds_limited)| *funds_limited),
                ) {
                    Ok(remaining) => remaining,
                    // rippled's flowCross does not propagate a claimed Flow
                    // failure. It leaves the offer unchanged and returns
                    // tesSUCCESS to applyGuts, whose FillOrKill tail converts
                    // that remainder to tecKILLED. Preserve that terminal code
                    // here so build-ledger claims the FOK in its canonical pass
                    // instead of retrying a transient tecUNFUNDED/tecPATH code.
                    Err(result) if tx_flags & TF_FILL_OR_KILL != 0 && result.is_tec() => {
                        return Ok(TransactionResult::TecKilled);
                    }
                    Err(result) => return Err(result),
                }
            } else {
                let (remaining_pays, remaining_gets, crossed) = taker_cross(
                    ctx,
                    &account_id,
                    &mut acct,
                    &taker_pays,
                    &taker_gets,
                    &inverse_book,
                    is_sell,
                    domain_only,
                )?;
                (remaining_pays, remaining_gets, crossed, false)
            };

        // FlowCross returns the independently unfilled input and output before
        // `remaining_offer` normalises a source-exhausted remainder to two
        // zeroes. FillOrKill must inspect its exact side before that collapse:
        // a regular offer requires all requested output, while tfSell requires
        // all funded input to be consumed. Flow caps sendMax at the source's
        // liquid funds, so a sell can finish its funded budget with nominal
        // TakerGets still remaining (105503179/F352BE04).
        if ctx.rules.enabled(&feature_id("FlowCross")) && tx_flags & TF_FILL_OR_KILL != 0 {
            let (exact_remainder, exact_total) = if is_sell {
                (&remaining_gets, &taker_gets)
            } else {
                (&remaining_pays, &taker_pays)
            };
            let fully_filled = (is_sell
                && crossed
                && source_exhausted
                && flow_source_budget
                    .as_ref()
                    .is_some_and(|(_, funds_limited)| *funds_limited))
                || Leg::parse(exact_remainder)
                    .zip(Leg::parse(exact_total))
                    .is_some_and(|(remaining, total)| remaining_is_filled(&remaining, &total));
            if !fully_filled {
                return Ok(TransactionResult::TecKilled);
            }
        }

        // Flow returns the independently consumed input and output amounts. A
        // resting remainder must nevertheless retain the offer's original
        // quality: CreateOffer::flowCross first keeps the remaining output, then
        // re-derives the input with mulRound. Subtracting both flow amounts
        // independently drifts an offer after crossing price-improving liquidity
        // (Testnet 20934193 / 9B345F1A).
        let (remaining_pays, remaining_gets) = if ctx.rules.enabled(&feature_id("FlowCross")) {
            let pays_leg = Leg::parse(&taker_pays).ok_or(TransactionResult::TemBadOffer)?;
            let gets_leg = Leg::parse(&taker_gets).ok_or(TransactionResult::TemBadOffer)?;
            let orig_quality = if ctx.rules.enabled(&feature_id("fixUniversalNumber")) {
                rxrpl_amount::get_rate_round_even(
                    &leg_as_quality_iou(&gets_leg),
                    &leg_as_quality_iou(&pays_leg),
                )
            } else {
                rxrpl_amount::get_rate(
                    &leg_as_quality_iou(&gets_leg),
                    &leg_as_quality_iou(&pays_leg),
                )
            }
            .unwrap_or(0);
            let flow_remaining_pays =
                Leg::parse(&remaining_pays).ok_or(TransactionResult::TefInternal)?;
            remaining_offer(
                is_sell,
                orig_quality,
                &flow_buy_remaining_output(&flow_remaining_pays, &pays_leg, is_sell, crossed),
                &Leg::parse(&remaining_gets).ok_or(TransactionResult::TefInternal)?,
                &pays_leg,
                &gets_leg,
                crossed,
            )
        } else {
            (remaining_pays, remaining_gets)
        };

        // Nothing left to place when either side is exhausted (fully crossed):
        // rippled places no resting offer. Commit the taker's mutations.
        let commit_acct =
            |ctx: &mut ApplyContext<'_>, acct: &Value| -> Result<(), TransactionResult> {
                let nb = serde_json::to_vec(acct).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .update(acct_key, nb)
                    .map_err(|_| TransactionResult::TemMalformed)?;
                Ok(())
            };
        if value_is_zero(&remaining_pays) || value_is_zero(&remaining_gets) {
            commit_acct(ctx, &acct)?;
            return Ok(TransactionResult::TesSuccess);
        }

        // rippled clears the leftover offer when crossing exhausted the taker's
        // funds in the asset it sells: an offer it cannot fund at all is not
        // placed (`flowCross`, OfferCreate.cpp:481-499, `takerInBalance <= 0`,
        // where takerInBalance = accountFunds on the post-crossing sandbox =
        // liquid balance above the owner reserve). An IOU gets side's
        // post-crossing balance is committed to the view by crossing, so
        // `owner_funds_leg` reads the live value; the taker's XRP balance moves
        // through the working copy and is only written back on commit, so its
        // post-crossing liquid funds are computed from `acct`.
        if crossed {
            if let Some(gets_leg) = Leg::parse(&taker_gets) {
                let unfunded = source_exhausted
                    || if gets_leg.is_xrp {
                        let oc =
                            acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        helpers::get_balance(&acct) as i64 - ctx.fees.account_reserve(oc) as i64
                            <= 0
                    } else {
                        owner_funds_leg(ctx, &account_id, &gets_leg).is_zero()
                    };
                if unfunded {
                    commit_acct(ctx, &acct)?;
                    return Ok(TransactionResult::TesSuccess);
                }
            }
        }

        // A remainder survives crossing. Time-in-force flags decide its fate
        // before any resting offer is placed.
        if tx_flags & TF_FILL_OR_KILL != 0 {
            // FillOrKill: failure to fully cross kills the whole operation.
            commit_acct(ctx, &acct)?;
            return Ok(TransactionResult::TecKilled);
        }
        if tx_flags & TF_IMMEDIATE_OR_CANCEL != 0 {
            // ImmediateOrCancel: the remainder is cancelled, not placed. An offer
            // that transferred nothing returns tecKILLED; otherwise tesSUCCESS.
            commit_acct(ctx, &acct)?;
            return Ok(if crossed {
                TransactionResult::TesSuccess
            } else {
                TransactionResult::TecKilled
            });
        }

        // Owner reserve: a resting offer needs reserve for one more owned object.
        // rippled gates on mPriorBalance (the XRP balance before fee/crossing):
        // when it cannot cover the reserve for OwnerCount+1 the remainder is not
        // placed. If nothing crossed that is tecINSUF_RESERVE_OFFER (fee and
        // sequence charged, offer dropped); if the offer already crossed, the
        // fills stand and it succeeds without resting the remainder. Read the
        // count fresh: a cancelled OfferSequence has already decremented it.
        let reserve_count = acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if prior_balance < ctx.fees.account_reserve(reserve_count + 1) {
            if !crossed {
                // A claimed tec discards the doApply changes and keeps only the
                // fee and sequence — return Err so the engine rolls the child
                // back (and, under tapRETRY, defers the whole transaction to a
                // later pass to be reclaimed there, matching mainnet's
                // TransactionIndex and balances).
                return Err(TransactionResult::TecInsufReserveOffer);
            }
            commit_acct(ctx, &acct)?;
            return Ok(TransactionResult::TesSuccess);
        }

        let offer_key = keylet::offer(&account_id, sequence);

        // The order-book directory is keyed by the book base (currencies +
        // issuers) with its low 64 bits replaced by the offer's quality (rate),
        // so offers sort by price. rippled stores this as the offer's
        // BookDirectory and tags the directory with the rate + book assets.
        // rippled computes the offer's book rate (`uRate`) from the ORIGINAL
        // offer amounts before crossing and keys the resting offer's directory on
        // it; the residual carries the leftover but stays in the original-rate
        // directory. Re-deriving the rate from the (proportionally reduced)
        // leftover drifts by a divide ULP on the pre-`fixUniversalNumber`
        // truncating path (mainnet 30000004), so key on the original amounts.
        let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
        let quality = offer_book_quality(&taker_pays, &taker_gets, number_switchover);
        // A DomainID places the resting offer in the permissioned (domain) book
        // directory rather than the public book — rippled's getBookBase appends
        // the domain to the same BookDir hash, so a domain book lives at a
        // distinct base. The directory node is also tagged with the DomainID.
        // (`domain_id_hash` is computed above for the crossing routing.)
        let book_base = match &domain_id_hash {
            Some(d) => keylet::book_dir_domain(
                &pays_currency,
                &pays_issuer,
                &gets_currency,
                &gets_issuer,
                d,
            ),
            None => keylet::book_dir(&pays_currency, &pays_issuer, &gets_currency, &gets_issuer),
        };
        let book_dir_key = book_dir_with_quality(&book_base, quality);
        let mut book_describe: Vec<(&str, Value)> = vec![
            ("ExchangeRate", Value::from(u64_hex(quality))),
            ("TakerPaysCurrency", hex::encode_upper(pays_currency).into()),
            (
                "TakerPaysIssuer",
                hex::encode_upper(pays_issuer.as_bytes()).into(),
            ),
            ("TakerGetsCurrency", hex::encode_upper(gets_currency).into()),
            (
                "TakerGetsIssuer",
                hex::encode_upper(gets_issuer.as_bytes()).into(),
            ),
        ];
        if let Some(d) = &domain_id_hash {
            book_describe.push(("DomainID", d.to_string().into()));
        }
        let book_node = add_to_book_dir(ctx.view, &book_dir_key, &offer_key, &book_describe)?;
        let owner_node = add_to_owner_dir(ctx.view, &account_id, &offer_key)?;

        // Build the Offer SLE. sfFlags, sfBookNode and sfOwnerNode are REQUIRED
        // in rippled's Offer ledger format, so they always serialize (even at
        // zero); only sfExpiration is optional.
        let mut offer = serde_json::Map::new();
        offer.insert("LedgerEntryType".into(), "Offer".into());
        offer.insert("Account".into(), account_str.into());
        offer.insert("Sequence".into(), Value::from(sequence));
        // Resting offer carries the LEFTOVER after crossing, at the original rate.
        offer.insert("TakerPays".into(), remaining_pays.clone());
        offer.insert("TakerGets".into(), remaining_gets.clone());
        offer.insert("BookDirectory".into(), book_dir_key.to_string().into());
        // A domain-scoped offer carries its DomainID so the book/crossing logic
        // and clients can tell it apart from a public-book offer.
        if let Some(d) = &domain_id_hash {
            offer.insert("DomainID".into(), d.to_string().into());
        }
        // Placeholder PreviousTxnID/LgrSeq — the engine's central stamping fills
        // these with the creating transaction's id and ledger after apply.
        offer.insert(
            "PreviousTxnID".into(),
            "0000000000000000000000000000000000000000000000000000000000000000".into(),
        );
        offer.insert("PreviousTxnLgrSeq".into(), Value::from(0u32));
        // An offer carries its own Expiration (a hard ledger-time deadline) when
        // the transaction sets one; rippled copies it onto the Offer SLE.
        if let Some(exp) = ctx.tx.get("Expiration").and_then(|v| v.as_u64()) {
            offer.insert("Expiration".into(), Value::from(exp));
        }
        // sfFlags, sfBookNode and sfOwnerNode are REQUIRED fields on the Offer
        // SLE — rippled writes all three unconditionally, so they serialize even
        // when zero (a root-page offer carrying no flags). Only the
        // transaction's tfPassive/tfSell map onto the SLE, and tfSell (0x80000)
        // becomes lsfSell (0x20000) — they are not the same bit.
        let offer_flags = (if tx_flags & TF_PASSIVE != 0 {
            LSF_PASSIVE
        } else {
            0
        }) | (if tx_flags & TF_SELL != 0 { LSF_SELL } else { 0 });
        offer.insert("Flags".into(), Value::from(offer_flags));
        offer.insert("BookNode".into(), u64_hex(book_node).into());
        offer.insert("OwnerNode".into(), u64_hex(owner_node).into());
        let offer_bytes = serde_json::to_vec(&Value::Object(offer))
            .map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .insert(offer_key, offer_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        // Bump owner count for the new resting offer (sequence already consumed).
        helpers::adjust_owner_count(&mut acct, 1);

        let new_bytes = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(acct_key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// One side of an offer: native XRP (drops) or an IOU with its issuer/currency.
#[derive(Clone, Debug)]
struct Leg {
    is_xrp: bool,
    drops: i64,
    iou: IOUAmount,
    currency: [u8; 20],
    issuer: AccountId,
}

impl Leg {
    fn parse(v: &Value) -> Option<Leg> {
        if let Some(s) = v.as_str() {
            return Some(Leg {
                is_xrp: true,
                drops: s.parse().ok()?,
                iou: IOUAmount::ZERO,
                currency: [0u8; 20],
                issuer: AccountId::from([0u8; 20]),
            });
        }
        let value = v.get("value")?.as_str()?;
        let (currency, issuer) = currency_and_issuer(v);
        Some(Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::from_decimal_string(value).ok()?,
            currency,
            issuer,
        })
    }

    /// Render this leg with `amount` as its value (an IOU keeps currency/issuer).
    fn with_amount(&self, amount: &IOUAmount, xrp_drops: i64) -> Value {
        if self.is_xrp {
            return Value::from(xrp_drops.to_string());
        }
        serde_json::json!({
            "currency": currency_code_str(&self.currency),
            "issuer": encode_account_id(&self.issuer),
            "value": amount.to_decimal_string(),
        })
    }

    fn is_zero(&self) -> bool {
        if self.is_xrp {
            self.drops == 0
        } else {
            self.iou.is_zero()
        }
    }
}

/// True when a TakerPays/TakerGets value is zero (XRP `"0"` or IOU value 0).
fn value_is_zero(v: &Value) -> bool {
    Leg::parse(v).map(|l| l.is_zero()).unwrap_or(true)
}

/// Render a 20-byte currency code back to its string form (3-char ASCII or
/// 40-char hex), the inverse of `helpers::currency_to_bytes`.
fn currency_code_str(currency: &[u8; 20]) -> String {
    let ascii = &currency[12..15];
    let is_standard = currency[..12].iter().all(|&b| b == 0)
        && currency[15..].iter().all(|&b| b == 0)
        && ascii.iter().all(|&b| b != 0);
    if is_standard {
        String::from_utf8_lossy(ascii).to_string()
    } else {
        hex::encode_upper(currency)
    }
}

/// The issuer's transfer rate as an `IOUAmount` multiplier (1.0 = no fee).
/// rippled charges the fee only when neither party is the issuer.
pub(crate) fn transfer_rate(ctx: &ApplyContext<'_>, issuer: &AccountId) -> IOUAmount {
    let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
    let rate = transfer_rate_value(ctx, issuer);
    if rate > 1_000_000_000 {
        IOUAmount::from_parts(u64::from(rate), -9, false).unwrap_or(one)
    } else {
        one
    }
}

fn transfer_rate_value(ctx: &ApplyContext<'_>, issuer: &AccountId) -> u32 {
    let key = keylet::account(issuer);
    let rate = ctx
        .view
        .read(&key)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|a| a.get("TransferRate").and_then(|v| v.as_u64()));
    match rate {
        Some(r) if r > 1_000_000_000 && r <= u64::from(u32::MAX) => r as u32,
        _ => 1_000_000_000,
    }
}

fn add_iou_balance(
    balance: &IOUAmount,
    delta: &IOUAmount,
    number_switchover: bool,
) -> Result<IOUAmount, rxrpl_amount::AmountError> {
    if number_switchover {
        IOUAmount::add_round(balance, delta)
    } else {
        IOUAmount::add(balance, delta)
    }
}

#[derive(Clone, Copy)]
enum BalanceRounding {
    Legacy,
    Nearest,
    HalfUpward,
    DebitOwner,
}

fn amm_pool_debit_rounding(intermediate_output: bool) -> BalanceRounding {
    // Terminal AMM settlement uses Number's nearest rounding. The waived-fee
    // bridge has a separate debit in apply_amm_move.
    if intermediate_output {
        BalanceRounding::HalfUpward
    } else {
        BalanceRounding::Nearest
    }
}

fn amm_recipient_rounding(offer_crossing: bool, number_switchover: bool) -> BalanceRounding {
    // A modern OfferCreate AMM's terminal BookOfferCrossingStep credits the
    // taker through normal IOUAmount/Number addition. Keep the existing
    // rounding for payment hops and pre-Number crossing.
    if offer_crossing && number_switchover {
        BalanceRounding::Nearest
    } else {
        BalanceRounding::HalfUpward
    }
}

#[cfg(test)]
fn add_iou_balance_downward(a: &IOUAmount, b: &IOUAmount) -> IOUAmount {
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode};

    let _round = RoundModeGuard::new(RoundingMode::Downward);
    Number::from_iou(a).add(&Number::from_iou(b)).to_iou()
}

fn add_iou_balance_towards_zero(
    a: &IOUAmount,
    b: &IOUAmount,
) -> Result<IOUAmount, rxrpl_amount::AmountError> {
    if a.is_zero() {
        return Ok(*b);
    }
    if b.is_zero() {
        return Ok(*a);
    }
    let lo = a.exponent().min(b.exponent());
    let hi = a.exponent().max(b.exponent());
    if hi - lo > 19 {
        return Ok(if a.exponent() == hi { *a } else { *b });
    }
    let ma = i128::from(a.mantissa()) * 10i128.pow((a.exponent() - lo) as u32);
    let mb = i128::from(b.mantissa()) * 10i128.pow((b.exponent() - lo) as u32);
    let va = if a.is_negative() { -ma } else { ma };
    let vb = if b.is_negative() { -mb } else { mb };
    let sum = va + vb;
    if sum == 0 {
        return Ok(IOUAmount::ZERO);
    }
    let negative = sum < 0;
    let mut magnitude = sum.unsigned_abs();
    let mut exponent = lo;
    const MAX_MANTISSA: u128 = 9_999_999_999_999_999;
    while magnitude > MAX_MANTISSA {
        magnitude /= 10;
        exponent += 1;
    }
    IOUAmount::from_parts(magnitude as u64, exponent, negative)
}

fn iou_sum_is_halfway(a: &IOUAmount, b: &IOUAmount) -> bool {
    if a.is_zero() || b.is_zero() {
        return false;
    }
    let lo = a.exponent().min(b.exponent());
    let hi = a.exponent().max(b.exponent());
    if hi - lo > 19 {
        return false;
    }
    let ma = i128::from(a.mantissa()) * 10i128.pow((a.exponent() - lo) as u32);
    let mb = i128::from(b.mantissa()) * 10i128.pow((b.exponent() - lo) as u32);
    let va = if a.is_negative() { -ma } else { ma };
    let vb = if b.is_negative() { -mb } else { mb };
    let magnitude = (va + vb).unsigned_abs();
    const MAX_MANTISSA: u128 = 9_999_999_999_999_999;
    if magnitude <= MAX_MANTISSA {
        return false;
    }
    let mut probe = magnitude;
    let mut digits_over = 0u32;
    while probe > MAX_MANTISSA {
        probe /= 10;
        digits_over += 1;
    }
    let divisor = 10u128.pow(digits_over);
    magnitude % divisor == divisor / 2
}

fn add_iou_balance_half_upward(
    a: &IOUAmount,
    b: &IOUAmount,
) -> Result<IOUAmount, rxrpl_amount::AmountError> {
    let nearest = IOUAmount::add_round(a, b)?;
    if nearest.is_negative() && iou_sum_is_halfway(a, b) {
        add_iou_balance_towards_zero(a, b)
    } else {
        Ok(nearest)
    }
}

/// Apply a signed IOU change to a holder's trust line toward `issuer`.
/// `gain` > 0 credits the holder, < 0 debits. Balance is stored from the low
/// account's perspective, so a high-account holder's balance moves opposite to
/// its gain. Byte-exact via `IOUAmount` (no floating point).
pub(crate) fn credit_line(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    issuer: &AccountId,
    currency: &[u8; 20],
    gain: &IOUAmount,
    round: bool,
) -> Result<(), TransactionResult> {
    let rounding = if round {
        BalanceRounding::Nearest
    } else {
        BalanceRounding::Legacy
    };
    credit_line_with_rounding(ctx, holder, issuer, currency, gain, rounding)
}

fn credit_line_with_rounding(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    issuer: &AccountId,
    currency: &[u8; 20],
    gain: &IOUAmount,
    rounding: BalanceRounding,
) -> Result<(), TransactionResult> {
    let key = keylet::trust_line(holder, issuer, currency);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecPathDry)?;
    let mut line: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let cur = line
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let balance =
        IOUAmount::from_decimal_string(cur).map_err(|_| TransactionResult::TefInternal)?;
    let holder_is_high = holder.as_bytes() > issuer.as_bytes();
    let delta = if holder_is_high { gain.negate() } else { *gain };
    // Legacy crossing (pre-Number 2013 ledgers) truncates the smaller term;
    // modern Flow conversions round to nearest. See [`IOUAmount::add_round`].
    let new = match rounding {
        BalanceRounding::Legacy => add_iou_balance(&balance, &delta, false),
        BalanceRounding::Nearest => add_iou_balance(&balance, &delta, true),
        BalanceRounding::HalfUpward => add_iou_balance_half_upward(&balance, &delta),
        BalanceRounding::DebitOwner => {
            use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode};
            let direction = if holder_is_high {
                RoundingMode::Upward
            } else {
                RoundingMode::Downward
            };
            let _round = RoundModeGuard::new(direction);
            Ok(Number::from_iou(&balance)
                .add(&Number::from_iou(&delta))
                .to_iou())
        }
    }
    .map_err(|_| TransactionResult::TefInternal)?;
    // A one-drop offer can price its IOU input below the trust line's ULP.
    // rippleCredit leaves that line untouched when its rounded balance does
    // not move; writing it would create an extra ModifiedNode (105501848).
    if new == balance {
        return Ok(());
    }
    line["Balance"]["value"] = Value::String(new.to_decimal_string());
    let nb = serde_json::to_vec(&line).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

fn credit_line_ulp_if_coarse(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    amount: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    if amount.is_xrp || amount.issuer == *holder {
        return Ok(());
    }
    let key = keylet::trust_line(holder, &amount.issuer, &amount.currency);
    let Some(bytes) = ctx.view.read(&key) else {
        return Ok(());
    };
    let Ok(line) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let Some(s) = line
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
    else {
        return Ok(());
    };
    let Ok(bal) = IOUAmount::from_decimal_string(s) else {
        return Ok(());
    };
    if bal.exponent() < amount.iou.exponent() + 4 {
        return Ok(());
    }
    let Ok(ulp) = IOUAmount::from_parts(1, bal.exponent(), false) else {
        return Ok(());
    };
    credit_line(ctx, holder, &amount.issuer, &amount.currency, &ulp, round)
}

/// Add `delta` drops to an account's XRP balance (read/modify/write the SLE).
fn credit_xrp(
    ctx: &mut ApplyContext<'_>,
    account: &AccountId,
    delta: i64,
) -> Result<(), TransactionResult> {
    let key = keylet::account(account);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecPathDry)?;
    let mut acct: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct) as i64 + delta;
    if bal < 0 {
        return Err(TransactionResult::TecUnfundedOffer);
    }
    helpers::set_balance(&mut acct, bal as u64);
    let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

/// Cross the new offer against the inverse book, filling crossable resting
/// offers best-price-first. Returns the taker's remaining `(TakerPays,
/// TakerGets)` and whether any fill occurred. The taker's own AccountRoot XRP
/// balance is mutated through `taker_acct` (written by the caller); every other
/// entry (counterparties, trust lines, consumed offers) goes through the view.
///
/// Scope: full takes of funded, crossable offers (the validated path, mainnet
/// #338500). Partial takes rescale at the resting offer's quality. Unfunded
/// offers encountered in the walk are reaped, mirroring rippled's dirAdvance.
#[allow(clippy::too_many_arguments)]
fn cross_offers(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    taker_pays: &Value,
    taker_gets: &Value,
    inverse_book: &rxrpl_primitives::Hash256,
    is_sell: bool,
    domain_only: bool,
    source_budget: &Leg,
    source_funds_limited: bool,
) -> Result<(Value, Value, bool, bool), TransactionResult> {
    let out_leg = match Leg::parse(taker_pays) {
        Some(l) => l,
        None => {
            return Ok((
                taker_pays.clone(),
                taker_gets.clone(),
                false,
                source_budget.is_zero(),
            ));
        }
    };
    let in_leg = match Leg::parse(taker_gets) {
        Some(l) => l,
        None => {
            return Ok((
                taker_pays.clone(),
                taker_gets.clone(),
                false,
                source_budget.is_zero(),
            ));
        }
    };

    // Taker's quality limit in the inverse book = TakerGets / TakerPays.
    // The threshold must be computed with the SAME divide canonicalisation
    // used to store the offers' BookDirectory qualities (`offer_book_quality`):
    // once `fixUniversalNumber` is active rippled's getRate rounds half-to-even,
    // otherwise it truncates. Using the truncating `get_rate` here while the
    // dir qualities are round-even left the threshold 1 ULP low, so the walk
    // broke before an exactly-equal-priced resting offer and crossed nothing.
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let passive = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_PASSIVE != 0;
    let threshold =
        flow_crossing_threshold(ctx, taker, &in_leg, &out_leg, number_switchover, passive);
    // Flow grosses `sendMax` by the source transfer rate so BookStep may walk
    // far enough to reap self-owned/unfunded tips and to compare AMM strands.
    // A direct CLOB step, however, charges that rate as part of the strand's
    // quality. Compare its raw directory against the ungrossed taker limit;
    // otherwise an external offer just beyond the limit is crossed
    // (testnet ledger 105500436).
    let clob_threshold = crossing_quality_threshold(&in_leg, &out_leg, number_switchover, passive);
    if threshold == 0 {
        return Ok((
            taker_pays.clone(),
            taker_gets.clone(),
            false,
            source_budget.is_zero(),
        ));
    }
    // Flow checks the default strand's qualityUpperBound before forEachOffer
    // materialises its first CLOB tip. In particular, an AMM can win the raw
    // tip comparison yet fail the offer limit after its input transfer rate is
    // included. Reject that strand before either the AMM or a self offer is
    // touched (105501442). An IOU/IOU offer has a second XRP bridge strand, so
    // a dry direct strand cannot reject the whole crossing here. Domain books
    // have no AMM and use a separate key.
    if !domain_only
        && (in_leg.is_xrp || out_leg.is_xrp)
        && !offer_crossing_strand_reaches_limit(ctx, taker, taker_gets, taker_pays, threshold)
    {
        return Ok((
            taker_pays.clone(),
            taker_gets.clone(),
            false,
            source_budget.is_zero(),
        ));
    }

    let mut remaining_out = out_leg.clone();
    // FlowCross carries the aggregate output remainder as a Number. Keeping
    // that extra precision across several fills matters when the final offer
    // is partial: ledger 105500430 reaches 9889.176237768096 after summing the
    // preceding fills, while canonicalising every subtraction yields ...8100.
    let mut remaining_out_num = leg_to_number(&out_leg);
    // Flow accumulates strand outputs from smallest to largest. Preserve the
    // exact AMM and CLOB deliveries so a later partial offer uses the aggregate
    // remainder, rather than repeated rounded subtraction (105502845/TX16).
    let mut direct_outputs = Vec::new();
    let mut direct_amm_used = false;
    let mut direct_fib_pass_used = false;
    let mut remaining_in = in_leg.clone();
    let mut source_remaining = source_budget.clone();
    // The source DirectStep settles one aggregate input for a CLOB pass. Keep
    // its pre-crossing balance so multiple offer debits do not round the trust
    // line after every synthetic fill (105501905 / BB5D7C46).
    let source_balance = if number_switchover
        && !in_leg.is_xrp
        && in_leg.issuer != *taker
        && (in_leg.currency != out_leg.currency || in_leg.issuer != out_leg.issuer)
        && transfer_rate_value(ctx, &in_leg.issuer) == 1_000_000_000
    {
        let key = keylet::trust_line(taker, &in_leg.issuer, &in_leg.currency);
        ctx.view
            .read(&key)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|line| {
                IOUAmount::from_decimal_string(line["Balance"]["value"].as_str()?).ok()
            })
            .map(|balance| FlowSourceBalance {
                key,
                balance,
                holder_is_high: taker.as_bytes() > in_leg.issuer.as_bytes(),
            })
    } else {
        None
    };
    let mut clob_source_spent = Vec::new();
    let mut clob_quality = None;
    let mut mixed_clob_quality = false;
    let mut non_clob_source_spent = false;
    let mut crossed = false;
    let mut reaped_self_offer = false;
    // A CLOB-bounded AMM pass can leave only a self/unfunded tip behind. In
    // rippled, the next pass removes that tip, delivers nothing, and ends the
    // strand as dry. Do not turn that dry pass into a second AMM crossing at
    // the looser taker threshold in the terminal tail.
    let mut terminal_amm_allowed = true;
    // Flow's OfferStream advances once more after fully consuming a tip. While
    // doing so it reaps every unfunded offer it encounters, even across quality
    // directories, and only then exposes the next funded offer for the quality
    // check. Do not do this on the initial probe: already-unfunded offers beyond
    // the taker's limit stay untouched when no preceding tip was consumed.
    let mut advance_after_exhausted_tip = false;
    // Once a real CLOB callback has run, BookStep may materialise the next tip
    // and apply permanent self/unfunded cleanup before checking its quality.
    // A partial buy fill that satisfies the output stops before this advance.
    let mut clob_offer_attempted = false;
    // BookStep tries the AMM once per pass, then advances through self-owned
    // tips to the next CLOB offer. Reaching a new quality directory in that
    // same pass must not generate another AMM fragment.
    let mut skip_amm_after_self_tip = false;
    let book_prefix = &inverse_book.as_bytes()[0..24];
    let fok = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_FILL_OR_KILL != 0;
    let ioc =
        ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_IMMEDIATE_OR_CANCEL != 0;
    // Autobridge: when NEITHER leg is XRP, the offer also competes an
    // XRP-intermediate path (`in -> XRP -> out`) against the direct book. Prebuild
    // that two-hop strand once; `try_bridge_step` crosses it per band by quality.
    // A domain-only offer never bridges through the AMM (domain books exclude AMM
    // liquidity).
    let bridge = if !domain_only && !in_leg.is_xrp && !out_leg.is_xrp {
        build_flow_strand(
            ctx,
            &[
                taker_gets.clone(),
                Value::String("0".to_string()),
                taker_pays.clone(),
            ],
        )
    } else {
        None
    };

    // An IOU/IOU AMM competes with the XRP autobridge on every Flow pass when
    // its first Fibonacci offer leads the direct CLOB and the bridge. The
    // bridge's presence keeps AMM sizing in multi-path mode even when its
    // quality is outside this taker's limit (105504158 / 85F8EBBF).
    if let Some(bridge_strand) = bridge.as_ref()
        && let Some(direct_strand) =
            build_flow_strand(ctx, &[taker_gets.clone(), taker_pays.clone()])
        && ctx.rules.enabled(&feature_id("FlowSortStrands"))
        && direct_strand.terminal_has_amm()
        && ((!direct_strand.hops[0].has_clob
            && transfer_rate_value(ctx, &in_leg.issuer) == 1_000_000_000)
            || (!passive
                && ((transfer_rate_value(ctx, &in_leg.issuer) == 1_000_000_000
                    && book_tip_quality(ctx, &direct_strand.hops[0].inverse_book)
                        .is_none_or(|q| reject_quality(q, clob_threshold)))
                    || (transfer_rate_value(ctx, &in_leg.issuer) > 1_000_000_000
                        && first_fib_offer_beats_clob(ctx, &direct_strand)))
                && offer_crossing_strand_reaches_limit_with_amm_mode(
                    ctx, taker, taker_gets, taker_pays, threshold, true,
                )
                && matches!(
                    (
                        offer_crossing_initial_bound(ctx, &direct_strand),
                        offer_crossing_initial_bound(ctx, bridge_strand),
                    ),
                    (Some(direct), Some(bridge)) if num_le(&direct, &bridge)
                )))
        && transfer_rate_value(ctx, &out_leg.issuer) == 1_000_000_000
    {
        let demand = if is_sell {
            leg_to_number(&unbounded_leg(&out_leg))
        } else {
            leg_to_number(&out_leg)
        };
        let budget = leg_to_number(source_budget);
        let strands = [direct_strand, bridge_strand.clone()];
        let source_balance = flow_source_balance(ctx, taker, &strands);
        let limit =
            Number::from_iou(&from_rate(threshold).map_err(|_| TransactionResult::TefInternal)?);
        let (delivered, spent_total, _) = flow_multi(
            ctx,
            taker,
            taker_acct,
            taker,
            &strands,
            &demand,
            &budget,
            Some(&limit),
            &mut AmmContext::new(true),
        );
        // A tfSell that consumes the source's entire trust-line holding ends
        // with a zero balance. Replaying each AMM/bridge debit separately can
        // leave a sub-ULP negative residue even when the aggregate equals the
        // original holding (105501643 / B6762258).
        if is_sell
            && source_funds_limited
            && spent_total.sub(&budget).is_zero()
            && let Some(source) = source_balance.as_ref()
            && flow_holder_funds(&source.balance, source.holder_is_high)
                .sub(&budget)
                .is_zero()
        {
            set_flow_source_balance(ctx, source, &IOUAmount::ZERO);
        }
        if is_sell
            && source_funds_limited
            && let Some(source) = source_balance.as_ref()
        {
            clear_one_ulp_source_overdraft(ctx, source, source_budget);
        }
        let delivered = number_to_leg(&delivered, &out_leg);
        let spent = number_to_leg(&spent_total, &in_leg);
        let remaining_out = if leg_ge(&delivered, &out_leg) {
            leg_sub(&out_leg, &out_leg)
        } else {
            leftover_leg(&out_leg, &delivered, number_switchover)
        };
        let remaining_in = leftover_leg(&in_leg, &spent, number_switchover);
        return Ok((
            remaining_out.with_amount(&remaining_out.iou, remaining_out.drops),
            remaining_in.with_amount(&remaining_in.iou, remaining_in.drops),
            !spent.is_zero(),
            leg_ge(&spent, source_budget),
        ));
    }

    // The book base for traversal has the quality (low 64 bits) zeroed;
    // `keylet::book_dir` leaves those as hash bytes, so start the walk there.
    let mut probe = book_dir_with_quality(inverse_book, 0);
    let mut deferred_out_of_quality_dir = None;
    'walk: while let Some(dir_key) = ctx.view.succ(&probe) {
        // A partial buy fill can meet the requested output before spending
        // the whole input. Flow leaves that BookTip in place and does not
        // materialise a self-offer at the next quality (105500242 / TX 9).
        if !is_sell
            && clob_offer_attempted
            && remaining_out.is_zero()
            && !advance_after_exhausted_tip
        {
            break;
        }
        if &dir_key.as_bytes()[0..24] != book_prefix {
            break; // left this book
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        if dir_quality > threshold {
            let post_clob_cleanup =
                advance_after_exhausted_tip || (is_sell && clob_offer_attempted);
            // Once the strand falls below the taker's quality limit, retain
            // funded self offers at the next tip. The post-fill walk still
            // removes unfunded offers left by the exhausted maker.
            if post_clob_cleanup
                && !offer_crossing_strand_reaches_limit(
                    ctx, taker, taker_gets, taker_pays, threshold,
                )
            {
                if advance_past_unfunded_quality(
                    ctx,
                    taker,
                    taker_acct,
                    &dir_key,
                    QualityAdvanceMode::Speculative,
                )?
                .funded_tip_remains
                {
                    deferred_out_of_quality_dir = Some(dir_key);
                    break;
                }
                continue;
            }
            if !post_clob_cleanup {
                // A single-path AMM can keep the strand active even though the
                // CLOB tip is worse than the taker's limit. rippled constructs
                // OfferStream before trying that AMM, so expired/invalid tips
                // are removed first (105500799). If neither CLOB nor AMM can
                // reach the limit, the strand is rejected before OfferStream
                // runs and the funded expired tip stays untouched (105500455).
                if !offer_crossing_strand_reaches_limit(
                    ctx, taker, taker_gets, taker_pays, threshold,
                ) {
                    deferred_out_of_quality_dir = Some(dir_key);
                    break;
                }
                // Once the strand passes its quality bound, OfferStream may
                // materialise the first CLOB tip and remove a partially funded
                // offer whose strict reduction worsens its quality (105500437).
                reap_out_of_quality_tiny_tips(ctx, taker, taker_acct, &dir_key)?;
            }
            let mode = quality_advance_mode(
                dir_quality,
                threshold,
                post_clob_cleanup,
                is_sell,
                &remaining_out,
            );
            let advance = advance_past_unfunded_quality(ctx, taker, taker_acct, &dir_key, mode)?;
            reaped_self_offer |= advance.reaped_self_offer;
            if advance.funded_tip_remains {
                deferred_out_of_quality_dir = Some(dir_key);
                break;
            }
            continue;
        }
        // BookStep materialises the CLOB tip before comparing it with the
        // synthetic AMM offer. OfferStream permanently removes invalid and
        // unfunded tips at that point, but a funded self-offer is removed only
        // if execOffer later selects the CLOB. Do the permanent-only cleanup
        // before AMM selection; the regular offer loop performs self-cross
        // cleanup if the AMM does not finish the crossing.
        let funded_tip_before_amm = advance_past_unfunded_quality(
            ctx,
            taker,
            taker_acct,
            &dir_key,
            QualityAdvanceMode::Speculative,
        )?
        .funded_tip_remains;
        // After consuming a maker's available funds, OfferStream::step walks
        // past every now-unfunded tip before BookStep compares the AMM with a
        // funded book quality. Do not finish at an emptied intermediate page:
        // the next quality may hold another offer from that maker that must be
        // reaped first (105502488/TX27).
        if advance_after_exhausted_tip && !funded_tip_before_amm {
            continue;
        }
        // Interleave the AMM at this quality band BEFORE the resting CLOB offers.
        // rippled crosses whichever of the synthetic AMM offer / CLOB tip is
        // better at each quality level (BookStep tryAMM/execOffer). `amm_hop`'s
        // raw reserve-ratio spot gate admits the AMM only when it strictly beats
        // `dir_quality`, so this fires exactly when the AMM is at least as good
        // as the offers about to cross at this band; it is a no-op otherwise.
        let mut amm_consumed_in_band = false;
        if !remaining_out.is_zero() && !remaining_in.is_zero() {
            if let Some(b) = &bridge {
                let before = remaining_out.clone();
                let source_before_bridge = source_remaining.clone();
                let multi_path = offer_crossing_strand_reaches_limit_with_amm_mode(
                    ctx, taker, taker_gets, taker_pays, threshold, true,
                );
                let _ = try_bridge_step(
                    ctx,
                    taker,
                    taker_acct,
                    b,
                    multi_path,
                    &in_leg,
                    &out_leg,
                    &mut remaining_out,
                    &mut remaining_in,
                    &mut source_remaining,
                    &mut crossed,
                    dir_quality,
                    is_sell,
                )?;
                non_clob_source_spent |= leg_gt(&source_before_bridge, &source_remaining);
                if !leg_ge(&before, &remaining_out) || !leg_ge(&remaining_out, &before) {
                    remaining_out_num = leg_to_number(&remaining_out);
                }
                if remaining_out.is_zero() || remaining_in.is_zero() {
                    break 'walk;
                }
            }
            if !domain_only && !skip_amm_after_self_tip {
                let before = remaining_out.clone();
                let source_before_amm = source_remaining.clone();
                let (mut continue_to_clob, amm_delivered) = try_amm_step(
                    ctx,
                    taker,
                    taker_acct,
                    &mut remaining_out,
                    &mut remaining_in,
                    &mut source_remaining,
                    &mut crossed,
                    dir_quality,
                    fok,
                    is_sell,
                    /*cap_ending_spot_price=*/ true,
                )?;
                if number_switchover
                    && bridge.is_none()
                    && !is_sell
                    && let Some(delivered) = amm_delivered
                {
                    direct_outputs.push(leg_to_number(&delivered));
                    direct_amm_used = true;
                    (remaining_out, remaining_out_num) =
                        aggregate_output_remainder(&out_leg, &direct_outputs);
                    remaining_out =
                        flow_buy_remaining_output(&remaining_out, &out_leg, is_sell, crossed);
                    continue_to_clob = !remaining_out.is_zero();
                }
                amm_consumed_in_band = leg_gt(&source_before_amm, &source_remaining);
                non_clob_source_spent |= amm_consumed_in_band;
                // A funds-capped AMM fill exhausts the taker's source before
                // BookStep can visit the next offer, including a self offer.
                if amm_consumed_in_band && source_funds_limited && source_remaining.is_zero() {
                    break 'walk;
                }
                if continue_to_clob {
                    // execOffer evaluates self-cross removal before its CLOB
                    // callback checks the now-remaining budget. Mirror that
                    // ordering when an AMM offer asks BookStep to continue.
                    let advance = advance_past_unfunded_quality(
                        ctx,
                        taker,
                        taker_acct,
                        &dir_key,
                        QualityAdvanceMode::Executing,
                    )?;
                    reaped_self_offer |= advance.reaped_self_offer;
                    skip_amm_after_self_tip |= advance.reaped_self_offer;
                    terminal_amm_allowed &= clob_band_allows_terminal_amm(
                        amm_consumed_in_band,
                        funded_tip_before_amm,
                        advance.funded_tip_remains,
                    );
                }
                if !direct_amm_used
                    && (!leg_ge(&before, &remaining_out) || !leg_ge(&remaining_out, &before))
                {
                    remaining_out_num = leg_to_number(&remaining_out);
                }
                if !continue_to_clob {
                    // A selected AMM that cannot deliver an integral output
                    // ends this pass. Do not execute the funded CLOB tip or
                    // retry the same dry pool at the terminal quality.
                    terminal_amm_allowed = false;
                    break 'walk;
                }
                if remaining_out.is_zero() || remaining_in.is_zero() {
                    break 'walk; // AMM alone met the demand (or exhausted the taker's funds)
                }
            }
        }
        let offers = book_dir_indexes(ctx, &dir_key);

        for offer_key in offers {
            if !is_sell
                && clob_offer_attempted
                && remaining_out.is_zero()
                && !advance_after_exhausted_tip
            {
                break 'walk;
            }
            let Some(ob) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }
            let owner_str = offer.get("Account").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(owner) = decode_account_id(owner_str) else {
                continue;
            };
            // offer.out = what the offer gives = taker receives = offer.TakerGets.
            // offer.in  = what the offer wants = taker pays   = offer.TakerPays.
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
                continue;
            };
            let Some(offer_in) = Leg::parse(&offer["TakerPays"]) else {
                continue;
            };
            // An expired offer is removed 0-fill during the book walk, before any
            // funds/quality crossing (rippled `OfferStream`: `sfExpiration <=
            // parentCloseTime` via `hasExpired`). Core behaviour, not amendment
            // gated, so it applies on every ledger.
            if let Some(exp) = offer.get("Expiration").and_then(|v| v.as_u64()) {
                if exp <= ctx.view.parent_close_time() as u64 {
                    reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, &dir_key)?;
                    continue;
                }
            }
            // A bad offer with a zero amount on either side is removed 0-fill
            // (rippled `OfferStream`: `amount.empty()`).
            if offer_out.is_zero() || offer_in.is_zero() {
                reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, &dir_key)?;
                continue;
            }
            // A maker whose IN asset (what it receives) is deep-frozen can neither
            // receive nor send it, so the offer is removed 0-fill (rippled
            // `OfferStream`: `isDeepFrozen(owner, assetIn)`).
            if is_deep_frozen(ctx, &owner, &offer_in) {
                reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, &dir_key)?;
                continue;
            }
            // Owner-funds clamp: an offer can give at most what its owner
            // holds. Fully funded → the whole offer is available; underfunded
            // but positive → fill against the funded amount; zero → reap.
            // lsfSell must deliver the whole TakerGets; shortfall is unfunded
            // (30000048 45874350: 0.008 BTC vs 2.011).
            let funds = owner_funds_leg(ctx, &owner, &offer_out);
            let flags = offer.get("Flags").and_then(Value::as_u64).unwrap_or(0);
            let tiny_increased_quality = should_reap_tiny_increased_quality(
                &owner,
                &offer_in,
                &offer_out,
                &funds,
                dir_quality,
            );
            if tiny_increased_quality {
                reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, &dir_key)?;
                continue;
            }
            if offer_cannot_pay(&funds, &offer_out, flags, &owner == taker) {
                reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, &dir_key)?;
                continue;
            }
            if &owner == taker {
                // Flow's direct BookStep treats a self-owned inverse offer as
                // unfunded: remove it instead of crossing it. This applies to
                // funded offers too (mainnet 105500215 / 91B10CB9).
                reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, &dir_key)?;
                reaped_self_offer = true;
                continue;
            }
            // OfferStream materialises and permanently removes invalid or
            // self-crossing tips before BookStep's callback observes that the
            // taker's budget is exhausted. Only a funded external tip stops
            // the walk once the requested crossing is complete.
            let done = if is_sell {
                remaining_in.is_zero()
            } else {
                remaining_out.is_zero()
            };
            if done {
                break 'walk;
            }
            if reject_quality(dir_quality, clob_threshold) {
                break 'walk;
            }
            // BookStep first caps the owner's gross IOU debit to its funds,
            // then divides by the output transfer rate to find the net offer
            // output. Comparing the offer directly with gross funds can debit
            // more than the maker holds when the issuer charges a transfer fee
            // (105503215/CB282698).
            let output_rate =
                if offer_out.is_xrp || owner == offer_out.issuer || taker == &offer_out.issuer {
                    1_000_000_000
                } else {
                    transfer_rate_value(ctx, &offer_out.issuer)
                };
            let avail_out = leg_min(&offer_out, &net_book_payment_input(&funds, output_rate));
            // flowCross cannot spend more than the source `sendMax` captured
            // before the walk. Decrement that shared budget after every fill;
            // the unspent offer remainder stays based on the original amount and
            // is dropped when the source budget reaches zero (105500248
            // B88F1D1B, 105500305 E05D37B2).
            let input_rate =
                if offer_in.is_xrp || taker == &offer_in.issuer || owner == offer_in.issuer {
                    1_000_000_000
                } else {
                    transfer_rate_value(ctx, &offer_in.issuer)
                };
            // The source budget includes the issuer's transfer fee. Price the
            // offer against the net amount that its owner will receive.
            let net_source_remaining = net_book_payment_input(&source_remaining, input_rate);
            let net_spendable_in =
                net_offer_input_budget(&remaining_in, &source_remaining, input_rate);
            if net_spendable_in.is_zero() {
                break 'walk;
            }
            // A tfSell take is bounded by what the taker's remaining input can buy
            // at this offer's quality (it sells its whole TakerGets); a buy take is
            // bounded by the taker's remaining TakerPays demand.
            let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);
            let affordable_out = out_for_in(&net_spendable_in, &rate, &offer_out);
            let desired_out = if is_sell {
                avail_out.clone()
            } else {
                leg_min(&remaining_out, &avail_out)
            };
            let source_funds_bind = source_funds_limited
                && input_rate == 1_000_000_000
                && terminal_budget_binds(
                    &net_spendable_in,
                    &desired_out,
                    &affordable_out,
                    &offer_in,
                    &offer_out,
                    &rate,
                    leg_ge(&desired_out, &avail_out),
                );
            let use_full_input = is_sell
                || (source_funds_bind
                    && input_limited_uses_full_budget(true, false, false, &offer_out, false));
            let take_out = if is_sell {
                leg_min(&avail_out, &affordable_out)
            } else {
                leg_min(&desired_out, &affordable_out)
            };
            // A source remainder that buys no output makes this strand dry.
            // Do not debit the input or count it as a crossed offer.
            if take_out.is_zero() {
                break 'walk;
            }

            // Full take when the taker consumes the whole original offer;
            // otherwise a partial take of `take_out`, priced at the offer's
            // quality (`order_in = take_out * rate`, clamped to the offer).
            let full_take = consumes_full_offer(
                &take_out,
                &offer_out,
                &net_spendable_in,
                &offer_in,
                &remaining_out,
                &avail_out,
                is_sell,
            );
            let stop_after_input_limited_partial = ioc
                && is_sell
                && !full_take
                && use_full_input
                && !leg_ge(&take_out, &avail_out)
                && leg_gt(&offer_in, &net_spendable_in);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else if use_full_input && !leg_ge(&take_out, &avail_out) {
                // Input-bound: `take_out` was derived from the taker's whole
                // remaining input at this quality, so the taker gives exactly
                // that input. This is always true for tfSell and also for a
                // fee-free source whose available funds bind a non-sell pass;
                // repricing the integral output would leave artificial IOU dust.
                (take_out.clone(), net_spendable_in.clone())
            } else {
                let order_in = if !leg_ge(&take_out, &avail_out) {
                    // Demand-limited (funded offer, take bounded by the taker's
                    // remaining demand): the taker pays the strict ceil price at
                    // the offer's original book quality, as rippled's
                    // `Quality::ceilOutStrict`. Pricing from the offer's current
                    // (drifted) amounts under-prices an XRP take by a drop.
                    leg_min(&in_for_out(&take_out, &rate, &offer_in), &offer_in)
                } else if offer_in.is_xrp {
                    // BookStep limits an underfunded offer at its original
                    // directory quality, rounding the XRP input down to whole
                    // drops. The offer's current fields may have drifted after
                    // earlier fills (105500938); rounding upward instead
                    // over-charges other funded takes (105500121/122).
                    leg_min(
                        &in_for_out_round(&take_out, &rate, &offer_in, false),
                        &offer_in,
                    )
                } else {
                    // Funds-limited IOU take: BookStep limits the offer at its
                    // book quality with `ceilOutStrict(..., roundUp=false)`.
                    // This preserves BA71A68E's ...005 while rounding
                    // 105500333/49DE960D down from ...484 to ...483.
                    leg_min(
                        &in_for_out_round(&take_out, &rate, &offer_in, false),
                        &offer_in,
                    )
                };
                (take_out.clone(), order_in)
            };

            // Flow commits a quality-capped AMM fragment as one pass, then
            // reevaluates the residual CLOB take in a fresh strand pass. The
            // directory quality can still equal the taker's limit while the
            // integral/canonical partial amounts realize a rate one ULP worse.
            // That second pass is dry (with no relative-distance tolerance),
            // so leave the CLOB untouched and rest the residual offer.
            if amm_consumed_in_band {
                let mut effective_in = leg_as_quality_iou(&order_in);
                if !order_in.is_xrp && taker != &order_in.issuer && owner != order_in.issuer {
                    effective_in = IOUAmount::mul_round(
                        &effective_in,
                        &transfer_rate(ctx, &order_in.issuer),
                        true,
                    )
                    .unwrap_or(effective_in);
                }
                let realized =
                    quality_threshold(&effective_in, &order_out, number_switchover, false);
                if reject_quality(realized, threshold) {
                    terminal_amm_allowed = false;
                    break 'walk;
                }
            }

            // Move funds: the maker receives order_in, while the taker pays
            // gross_in including any input-issuer transfer fee.
            // Legacy Taker semantics: `order_out` is the NET the taker receives;
            // the owner's debit is grossed up by the output issuer's transfer
            // fee inside `pay_out`.
            // Post-fixUniversalNumber, rippled's Flow rounds trust-line balance
            // updates to nearest (Number); the legacy pre-2013 path truncated the
            // smaller term, which loses precision when a large balance is credited
            // a small amount (e.g. a 10^10 balance minus ~3e3 diverges 3 ULP).
            let gross_in = gross_book_payment_input(
                &order_in,
                &source_remaining,
                input_rate,
                leg_ge(&order_in, &net_source_remaining),
            );
            // A funds-limited quote can round its XRP output down to whole
            // drops while spending the entire IOU source. Flow rejects that
            // strand when its realized quality falls below the offer limit;
            // otherwise a tiny IOC crosses when rippled returns tecKILLED
            // (105503439/3D0B0D74).
            if source_funds_bind
                && reject_quality(
                    quality_threshold(
                        &leg_as_quality_iou(&gross_in),
                        &order_out,
                        number_switchover,
                        false,
                    ),
                    threshold,
                )
            {
                break 'walk;
            }
            if input_rate == 1_000_000_000 {
                pay_in(
                    ctx,
                    taker,
                    taker_acct,
                    &owner,
                    &order_in,
                    number_switchover,
                    PayInOptions {
                        skip_taker_debit: false,
                        allow_dust_drain: source_funds_limited,
                        gross_override: None,
                    },
                )?;
            } else {
                pay_in_book_payment(
                    ctx,
                    taker,
                    taker_acct,
                    &owner,
                    &order_in,
                    &gross_in,
                    number_switchover,
                    false,
                )?;
            }
            pay_out(
                ctx,
                taker,
                taker_acct,
                &owner,
                taker,
                &order_out,
                number_switchover,
            )?;
            if source_balance.is_some() {
                mixed_clob_quality |= clob_quality.is_some_and(|quality| quality != dir_quality);
                clob_quality = Some(dir_quality);
                clob_source_spent.push(leg_to_number(&gross_in));
            }
            clob_offer_attempted = true;
            skip_amm_after_self_tip = false;
            terminal_amm_allowed = true;

            // The offer's available liquidity is exhausted when the take
            // consumes all of `avail_out` — either the whole offer (`full_take`)
            // or, when the owner is underfunded, all of the owner's funds.
            // rippled deletes such an offer (the owner can no longer fund the
            // remainder) rather than leaving an unfunded husk in the book.
            let exhausted = leg_ge(&take_out, &avail_out);
            if exhausted {
                // Record the metadata delta (zero for a full take, the unfunded
                // remainder for a depleted owner) then delete, as rippled
                // consumes before BookTip / offerDelete drops the offer.
                let mut consumed = offer.clone();
                if full_take {
                    consumed["TakerGets"] = offer_out.with_amount(&IOUAmount::ZERO, 0);
                    consumed["TakerPays"] = offer_in.with_amount(&IOUAmount::ZERO, 0);
                } else {
                    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
                    let new_gets = leftover_leg(&offer_out, &order_out, number_switchover);
                    let new_pays = leftover_leg(&offer_in, &order_in, number_switchover);
                    consumed["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                    consumed["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                }
                let cb =
                    serde_json::to_vec(&consumed).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, cb)
                    .map_err(|_| TransactionResult::TefInternal)?;
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                advance_after_exhausted_tip = true;
            } else {
                // Reduce the resting offer in place by the filled amounts. Both
                // sides round to nearest once fixUniversalNumber is on (Number
                // semantics); the legacy truncating subtraction left the leftover
                // TakerGets 1 ULP high on a partial take (e.g. 105500025 XAH offer
                // 402E09A9: …638335 vs mainnet …638334) and 1 ULP low on 2017
                // TakerPays (30000008: …366 vs mainnet …367).
                let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
                let new_gets = leftover_leg(&offer_out, &order_out, number_switchover);
                let new_pays = leftover_leg(&offer_in, &order_in, number_switchover);
                let mut reduced = offer.clone();
                reduced["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                reduced["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                // A real sub-ULP fill can transfer funds while both offer legs
                // canonically remain unchanged. Do not emit an empty ModifiedNode.
                if reduced["TakerGets"] != offer["TakerGets"]
                    || reduced["TakerPays"] != offer["TakerPays"]
                {
                    let rb =
                        serde_json::to_vec(&reduced).map_err(|_| TransactionResult::TefInternal)?;
                    ctx.view
                        .update(offer_key, rb)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }
                // Quality quantisation can make `take_out` compare just below
                // the original offer even though canonical subtraction rounds
                // one residual side to zero. Such a dry offer is consumed and
                // deleted by Flow's OfferStream, with the zero residual retained
                // in DeletedNode.FinalFields.
                if new_gets.is_zero() || new_pays.is_zero() {
                    reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                    advance_after_exhausted_tip = true;
                } else {
                    advance_after_exhausted_tip = false;
                }
            }

            // A tfSell fill can deliver more than the remaining TakerPays demand;
            // clamp the demand at zero rather than underflowing the drops/mantissa.
            remaining_out = if leg_ge(&order_out, &remaining_out) {
                remaining_out_num = rxrpl_amount::number::Number::ZERO;
                leg_sub(&remaining_out, &remaining_out)
            } else if direct_amm_used {
                direct_outputs.push(leg_to_number(&order_out));
                let (remainder, number) = aggregate_output_remainder(&out_leg, &direct_outputs);
                remaining_out_num = number;
                remainder
            } else if number_switchover {
                remaining_out_num = remaining_out_num.sub(&leg_to_number(&order_out));
                number_to_leg(&remaining_out_num, &out_leg)
            } else {
                leg_sub(&remaining_out, &order_out)
            };
            if number_switchover && bridge.is_none() && !is_sell && !direct_amm_used {
                direct_outputs.push(leg_to_number(&order_out));
            }
            // FlowCross carries the taker's remaining input as a Number. Round
            // the aggregate remainder once per fill under fixUniversalNumber;
            // truncating each subtraction can leave one extra ULP for the next
            // offer and make a tfSell cross spend above its TakerGets ceiling.
            remaining_in = leftover_leg(&remaining_in, &order_in, number_switchover);
            source_remaining = leftover_leg(&source_remaining, &gross_in, number_switchover);
            crossed = true;
            // Flow's sendMax was capped to the taker's actual funds. A partial
            // BookStep take that spends those funds stops this offer stream;
            // the nominal TakerGets can still have a positive remainder. Do
            // not materialise later self offers with no source left
            // (105502632/TX16). A fully consumed maker tip still advances so
            // OfferStream can reap the maker's now-unfunded offers.
            if source_funds_limited && source_remaining.is_zero() && !advance_after_exhausted_tip {
                break 'walk;
            }
            // An input-limited partial tfSell+IOC fill stops before the next
            // self-owned tip (105502352/TX8). A non-IOC sell still advances to
            // reap that tip (105500733/TX280), and a fully consumed maker tip
            // keeps the existing post-fill advance path.
            if stop_after_input_limited_partial
                && remaining_in.is_zero()
                && !advance_after_exhausted_tip
            {
                break 'walk;
            }
        }
    }

    // Tail: the AMM may still beat every remaining CLOB up to the taker's overall
    // limit — the book was empty, was exhausted, or the walk stopped at a dir
    // worse than `threshold` while the AMM's spot still beats it. Cross the AMM up
    // to `threshold` (the loosest gate). rippled interleaves the AMM by quality
    // rather than gating it on whether any CLOB crossed, so this is NOT `!crossed`
    // gated: a single crossed CLOB offer must not suppress better AMM liquidity.
    if !remaining_out.is_zero() && !remaining_in.is_zero() {
        let mut direct_flow_pass = false;
        if let Some(b) = &bridge {
            let source_before_bridge = source_remaining.clone();
            let multi_path = offer_crossing_strand_reaches_limit_with_amm_mode(
                ctx, taker, taker_gets, taker_pays, threshold, true,
            );
            let bridge_handoff = try_bridge_step(
                ctx,
                taker,
                taker_acct,
                b,
                multi_path,
                &in_leg,
                &out_leg,
                &mut remaining_out,
                &mut remaining_in,
                &mut source_remaining,
                &mut crossed,
                threshold,
                is_sell,
            )?;
            non_clob_source_spent |= leg_gt(&source_before_bridge, &source_remaining);
            if let Some(mut amm_ctx) = bridge_handoff {
                let source_before_direct = source_remaining.clone();
                direct_fib_pass_used = try_direct_flow_amm_pass(
                    ctx,
                    taker,
                    taker_acct,
                    taker_gets,
                    taker_pays,
                    &in_leg,
                    &out_leg,
                    &mut remaining_out,
                    &mut remaining_in,
                    &mut source_remaining,
                    &mut crossed,
                    threshold,
                    is_sell,
                    &mut amm_ctx,
                )?;
                non_clob_source_spent |= leg_gt(&source_before_direct, &source_remaining);
                direct_flow_pass = true;
            }
        }
        // BookStep retries AMM with no CLOB quality only when the book was
        // empty at the start of this pass. Reaping a self-owned tip with no
        // fill ends that pass dry; it must not reopen the AMM at the looser
        // taker limit (105501188).
        if !direct_flow_pass
            && !domain_only
            && terminal_amm_allowed
            && (!reaped_self_offer || crossed)
        {
            let source_before_amm = source_remaining.clone();
            let _ = try_amm_step(
                ctx,
                taker,
                taker_acct,
                &mut remaining_out,
                &mut remaining_in,
                &mut source_remaining,
                &mut crossed,
                threshold,
                fok,
                is_sell,
                /*cap_ending_spot_price=*/ false,
            )?;
            non_clob_source_spent |= leg_gt(&source_before_amm, &source_remaining);
            if terminal_amm_reaches_deferred_clob(
                is_sell,
                leg_gt(&source_before_amm, &source_remaining),
                &source_remaining,
                &remaining_out,
            ) && let Some(dir_key) = deferred_out_of_quality_dir
            {
                let _ = advance_past_unfunded_quality(
                    ctx,
                    taker,
                    taker_acct,
                    &dir_key,
                    QualityAdvanceMode::Executing,
                )?;
            }
        }
    }

    if direct_fib_pass_used && let Some(source) = source_balance.as_ref() {
        // Flow's source DirectStep settles the aggregate strand input. Applying
        // the bridge and direct AMM debits separately can shift the source
        // trust line by one IOU ULP (105503523 / 27E2ED6D).
        let spent = leg_to_number(&in_leg).sub(&leg_to_number(&remaining_in));
        reconcile_flow_source_balance(ctx, source, &spent);
    }

    if clob_source_spent.len() > 1
        && is_sell
        && !mixed_clob_quality
        && !non_clob_source_spent
        && let Some(source) = source_balance.as_ref()
    {
        let spent = sum_smallest_to_largest(&clob_source_spent);
        // A final input-limited offer can price a few Number ULPs below the
        // sell budget even though Flow's source DirectStep consumes that entire
        // budget. The offer reductions retain their priced values; only the
        // source endpoint settles the full gross input (105500187 / TX13).
        let spent_leg = number_to_leg(&spent, &in_leg);
        let budget_drained = debit_drains_source(source_budget, &spent_leg);
        if remaining_in.is_zero() || budget_drained {
            let source_debit = if budget_drained {
                leg_to_number(source_budget)
            } else {
                spent
            };
            reconcile_flow_source_balance(ctx, source, &source_debit);
        }
    }

    // A funds-capped tfSell can leave a one-ULP negative source balance after
    // separate AMM/book debits even though the original holding was the entire
    // crossing budget. Flow's source step settles that exhausted holding at
    // zero (105502451/TX25); keep larger overdrafts visible.
    if is_sell
        && source_funds_limited
        && crossed
        && let Some(source) = source_balance.as_ref()
    {
        clear_one_ulp_source_overdraft(ctx, source, source_budget);
    }

    Ok((
        remaining_out.with_amount(&remaining_out.iou, remaining_out.drops),
        remaining_in.with_amount(&remaining_in.iou, remaining_in.drops),
        crossed,
        source_remaining.is_zero(),
    ))
}

fn reap_out_of_quality_tiny_tips(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dir_key: &Hash256,
) -> Result<(), TransactionResult> {
    let quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
    for offer_key in book_dir_indexes(ctx, dir_key) {
        let Some(bytes) = ctx.view.read(&offer_key) else {
            continue;
        };
        let Ok(offer) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if offer.get("LedgerEntryType").and_then(Value::as_str) != Some("Offer") {
            continue;
        }
        let Some(owner) = offer
            .get("Account")
            .and_then(Value::as_str)
            .and_then(|value| decode_account_id(value).ok())
        else {
            continue;
        };
        let (Some(offer_in), Some(offer_out)) = (
            offer.get("TakerPays").and_then(Leg::parse),
            offer.get("TakerGets").and_then(Leg::parse),
        ) else {
            continue;
        };
        let funds = owner_funds_leg(ctx, &owner, &offer_out);
        if !should_reap_tiny_increased_quality(&owner, &offer_in, &offer_out, &funds, quality) {
            break;
        }
        if tiny_offer_is_permanently_reapable(ctx, &owner, &offer_out, &funds) {
            reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, dir_key)?;
        }
    }
    Ok(())
}

/// Continue Flow's post-fill OfferStream advance into a quality directory that
/// is already worse than the taker's limit. Unfunded/invalid tips disappear,
/// but the first funded offer stops the walk and remains untouched.
#[derive(Clone, Copy)]
enum QualityAdvanceMode {
    /// A speculative AMM probe: keep funded expired and self-owned offers.
    Speculative,
    /// OfferStream has been selected: remove expired and self-owned offers.
    Executing,
    /// OfferStream is materialising its first tip before the quality check:
    /// remove expired entries, but preserve an out-of-quality self offer.
    PreQuality,
}

fn quality_advance_mode(
    dir_quality: u64,
    threshold: u64,
    post_clob_cleanup: bool,
    is_sell: bool,
    remaining_out: &Leg,
) -> QualityAdvanceMode {
    // A successful AMM quote can keep the strand active after a CLOB fill,
    // but it cannot make a worse-quality funded self offer crossable. rippled's
    // BookOfferCrossingStep only removes a self offer when its own quality meets
    // the offer-crossing limit. PreQuality still reaps invalid/unfunded tips.
    if !post_clob_cleanup || reject_quality(dir_quality, threshold) {
        QualityAdvanceMode::PreQuality
    } else if preserve_next_quality_self_offer(is_sell, remaining_out) {
        QualityAdvanceMode::Speculative
    } else {
        QualityAdvanceMode::Executing
    }
}

struct QualityAdvanceResult {
    funded_tip_remains: bool,
    reaped_self_offer: bool,
}

fn advance_past_unfunded_quality(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dir_key: &Hash256,
    mode: QualityAdvanceMode,
) -> Result<QualityAdvanceResult, TransactionResult> {
    let quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
    let mut reaped_self_offer = false;
    for offer_key in book_dir_indexes(ctx, dir_key) {
        let Some(bytes) = ctx.view.read(&offer_key) else {
            continue;
        };
        let Ok(offer) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if offer.get("LedgerEntryType").and_then(Value::as_str) != Some("Offer") {
            continue;
        }
        let Some(owner) = offer
            .get("Account")
            .and_then(Value::as_str)
            .and_then(|value| decode_account_id(value).ok())
        else {
            continue;
        };
        let (Some(offer_in), Some(offer_out)) = (
            offer.get("TakerPays").and_then(Leg::parse),
            offer.get("TakerGets").and_then(Leg::parse),
        ) else {
            continue;
        };
        let expired = offer
            .get("Expiration")
            .and_then(Value::as_u64)
            .is_some_and(|expiration| expiration <= ctx.view.parent_close_time() as u64);
        let funds = owner_funds_leg(ctx, &owner, &offer_out);
        let flags = offer.get("Flags").and_then(Value::as_u64).unwrap_or(0);
        let tiny_increased_quality =
            should_reap_tiny_increased_quality(&owner, &offer_in, &offer_out, &funds, quality);
        if tiny_increased_quality {
            if !matches!(mode, QualityAdvanceMode::Speculative)
                || tiny_offer_is_permanently_reapable(ctx, &owner, &offer_out, &funds)
            {
                reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, dir_key)?;
            }
            continue;
        }
        // The pre-AMM probe is speculative: an AMM may satisfy the whole
        // demand before this CLOB tip is selected. Keep an otherwise funded
        // expired offer untouched in that case (105500455), and likewise keep
        // a funded self-offer until the CLOB is actually selected (105500679).
        // A short self-owned lsfSell still has funds, so it also stays at an
        // out-of-quality PreQuality tip. Its insufficiency is acted on only
        // after the CLOB is selected (105502135 / TX1).
        let defer_short_self_sell = matches!(mode, QualityAdvanceMode::PreQuality)
            && owner == *taker
            && flags & LSF_SELL != 0
            && !funds.is_zero()
            && !leg_ge(&funds, &offer_out);
        let otherwise_removable = offer_in.is_zero()
            || offer_out.is_zero()
            || is_deep_frozen(ctx, &owner, &offer_in)
            || (offer_cannot_pay(&funds, &offer_out, flags, &owner == taker)
                && !defer_short_self_sell);
        let self_reap = matches!(mode, QualityAdvanceMode::Executing)
            && owner == *taker
            && !expired
            && !otherwise_removable;
        let removable = (!matches!(mode, QualityAdvanceMode::Speculative) && expired)
            || otherwise_removable
            || (matches!(mode, QualityAdvanceMode::Executing) && owner == *taker);
        if !removable {
            return Ok(QualityAdvanceResult {
                funded_tip_remains: true,
                reaped_self_offer,
            });
        }
        reap_walk_offer(ctx, &owner, taker, taker_acct, &offer_key, dir_key)?;
        reaped_self_offer |= self_reap;
    }
    Ok(QualityAdvanceResult {
        funded_tip_remains: false,
        reaped_self_offer,
    })
}

/// Cross the book's AMM pool (if present and strictly better than `cq`) up to the
/// target quality `cq`, limited to the taker's funds, updating the remaining
/// demand/budget and `crossed` in place. Mirrors rippled's per-quality-band AMM
/// injection (`BookStep::tryAMM`): `amm_hop`'s raw spot gate admits the
/// AMM only when it strictly beats `cq`, so this is a no-op unless the AMM is at
/// least as good as the CLOB at this band.
#[allow(clippy::too_many_arguments)]
fn try_amm_step(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    source_remaining: &mut Leg,
    crossed: &mut bool,
    cq: u64,
    fok: bool,
    is_sell: bool,
    cap_ending_spot_price: bool,
) -> Result<(bool, Option<Leg>), TransactionResult> {
    // rippled's flowCross limits the crossing input to the taker's funds
    // (`sendMax = min(takerAmount.in, accountFunds)`, OfferCreate.cpp:400): the
    // AMM swap would otherwise overdraw the taker (its sold-asset balance goes
    // negative) when TakerGets exceeds what it holds.
    let input_rate = if remaining_in.is_xrp || taker == &remaining_in.issuer {
        1_000_000_000
    } else {
        transfer_rate_value(ctx, &remaining_in.issuer)
    };
    let gross_input_budget = input_rate > 1_000_000_000
        && ctx.tx.get("TransactionType").and_then(Value::as_str) == Some("OfferCreate");
    let budget = if gross_input_budget {
        let mut gross = remaining_in.clone();
        gross.iou = IOUAmount::mul_round(&gross.iou, &transfer_rate(ctx, &gross.issuer), true)
            .unwrap_or(gross.iou);
        leg_min(&gross, source_remaining)
    } else {
        leg_min(remaining_in, source_remaining)
    };
    if budget.is_zero() {
        return Ok((true, None));
    }
    // A tfSell offer sells its whole TakerGets and accepts any TakerPays (rippled
    // deliver = kMaxNative), so the AMM crosses the whole input budget rather than
    // stopping at the TakerPays demand. Capping at `remaining_out` made `amm_hop`
    // back-solve the input for the TakerPays and leave the rest of the sold asset
    // unspent (e.g. 105500045 A902BECE: 110.635 OGRE unsold into the OGRE/XRP AMM,
    // whose marginal quality stayed above the taker's limit the whole way).
    let demand = if is_sell {
        unbounded_leg(remaining_out)
    } else {
        remaining_out.clone()
    };
    // OfferCreate's sendMax quality is gross of the input issuer's transfer
    // fee, while the AMM curve is priced in the net amount reaching the pool.
    // The terminal quality cap must therefore use the original net offer
    // quality; the realised-quality check below still uses the gross limit.
    let pool_quality = if cap_ending_spot_price {
        cq
    } else {
        let input = ctx.tx.get("TakerGets").and_then(Leg::parse);
        let output = ctx.tx.get("TakerPays").and_then(Leg::parse);
        match (input, output) {
            (Some(input), Some(output)) => crossing_quality_threshold(
                &input,
                &output,
                ctx.rules.enabled(&feature_id("fixUniversalNumber")),
                ctx.tx.get("Flags").and_then(Value::as_u64).unwrap_or(0) & TF_PASSIVE != 0,
            ),
            _ => cq,
        }
    };
    // FillOrKill atomicity applies to the aggregate Flow result, not to each
    // liquidity source. The AMM may contribute a partial step that combines
    // with CLOB liquidity; the exact-side check in the transactor either keeps
    // the complete aggregate fill or rolls every step back with tecKILLED.
    let strict_tail_quality = !cap_ending_spot_price
        && ctx
            .tx
            .get("TakerGets")
            .and_then(Leg::parse)
            .is_some_and(|leg| {
                !leg.is_xrp && transfer_rate_value(ctx, &leg.issuer) == 1_000_000_000
            })
        && ctx
            .tx
            .get("TakerPays")
            .and_then(Leg::parse)
            .is_some_and(|leg| {
                !leg.is_xrp && transfer_rate_value(ctx, &leg.issuer) == 1_000_000_000
            });
    let tail_snapshot = strict_tail_quality.then(|| (ctx.view.checkpoint(), taker_acct.clone()));
    let quote = amm_hop(
        ctx,
        taker,
        taker_acct,
        taker,
        &demand,
        &budget,
        /*skip_input_debit=*/ false,
        /*skip_output_credit=*/ false,
        /*create_missing_dest_line=*/ true,
        /*partial=*/ amm_offer_allows_partial_output(fok, is_sell),
        /*reverse_output_limited=*/ false,
        /*target_quality=*/ Some(cq),
        /*pool_target_quality=*/ Some(pool_quality),
        cap_ending_spot_price,
        /*waive_output_transfer_fee=*/ true,
    )?;
    let selected_zero_output = matches!(&quote, AmmHopResult::ZeroOutput);
    if let AmmHopResult::Filled(delivered, spent) = quote {
        if strict_tail_quality {
            // The final spot-price cap can quote an integral IOU fragment whose
            // realised input/output rate is just worse than the offer limit.
            // Flow discards that entire pass (105503523 / 4061C8DC).
            let realised = quality_threshold(
                &leg_as_quality_iou(&spent),
                &delivered,
                ctx.rules.enabled(&feature_id("fixUniversalNumber")),
                false,
            );
            if reject_quality(realised, cq) {
                if let Some((cp, account_before)) = tail_snapshot {
                    ctx.view.rollback(cp);
                    *taker_acct = account_before;
                }
                return Ok((true, None));
            }
        }
        // A tfSell fill can deliver more than the remaining TakerPays demand;
        // clamp the demand at zero rather than underflowing the drops/mantissa.
        *remaining_out = if is_sell && leg_ge(&delivered, remaining_out) {
            leg_sub(remaining_out, remaining_out)
        } else {
            leg_sub(remaining_out, &delivered)
        };
        let net_spent = if gross_input_budget {
            net_book_payment_input(&spent, input_rate)
        } else {
            spent.clone()
        };
        *remaining_in = leg_sub(remaining_in, &net_spent);
        *source_remaining = leg_sub(source_remaining, &spent);
        *crossed = true;
        return Ok((
            amm_step_allows_clob(is_sell, remaining_out),
            Some(delivered),
        ));
    }
    Ok((!selected_zero_output, None))
}

fn amm_step_allows_clob(is_sell: bool, remaining_out: &Leg) -> bool {
    is_sell || !remaining_out.is_zero()
}

// A stale directory emptied before AMM selection cannot keep the pool bound
// to that directory's quality on the next pass.
fn clob_band_allows_terminal_amm(
    amm_consumed: bool,
    funded_tip_before_amm: bool,
    funded_tip_remains: bool,
) -> bool {
    !amm_consumed || !funded_tip_before_amm || funded_tip_remains
}

fn preserve_next_quality_self_offer(is_sell: bool, remaining_out: &Leg) -> bool {
    !is_sell && remaining_out.is_zero()
}

fn terminal_amm_reaches_deferred_clob(
    is_sell: bool,
    amm_consumed_input: bool,
    source_remaining: &Leg,
    remaining_out: &Leg,
) -> bool {
    is_sell && amm_consumed_input && source_remaining.is_zero() && !remaining_out.is_zero()
}

fn amm_offer_allows_partial_output(_fok: bool, _is_sell: bool) -> bool {
    true
}

/// Cross the XRP-intermediate autobridge path (`in -> XRP -> out`, a prebuilt
/// two-hop `bridge` strand) as a competing source in the `cross_offers` walk,
/// mirroring [`try_amm_step`]: cross bridge passes whose realised quality still
/// beats `cq` (the current CLOB band, or the taker's overall limit at the tail),
/// updating the remaining demand/budget in place. Sells can stop before a
/// worse next band; buys retain the bounded-demand aggregate quality check.
#[allow(clippy::too_many_arguments)]
fn try_bridge_step(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    bridge: &FlowStrand,
    multi_path: bool,
    in_leg: &Leg,
    out_leg: &Leg,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    source_remaining: &mut Leg,
    crossed: &mut bool,
    cq: u64,
    is_sell: bool,
) -> Result<Option<AmmContext>, TransactionResult> {
    // Flow evaluates the strand quality limit and its AMM amounts using the
    // transaction's Number precision. The 16-digit scale applies until the
    // vault or lending amendments enable the larger mantissa.
    let _number_scale = crate::amm_helpers::amm_number_scale_guard(ctx.rules);
    let mut budget = leg_min(remaining_in, source_remaining);
    // Flow uses Fibonacci AMM chunks only while both the direct and bridge
    // strands remain eligible. A dry direct book leaves a single bridge strand
    // (105500435); funded direct liquidity keeps the multi-path mode
    // (105500934). Retain the AMM iteration counter across bridge passes.
    let mut amm_ctx = AmmContext::new(multi_path);

    // Flow constructs the default and XRP bridge strands before filtering by
    // quality. The first `activateNext` therefore quotes their AMMs in
    // multipath mode, even if the direct strand will subsequently be removed.
    // A single-path spot quote here admits bridge liquidity whose initial
    // Fibonacci offer cannot meet the transaction limit.
    let passive = ctx.tx.get("Flags").and_then(Value::as_u64).unwrap_or(0) & TF_PASSIVE != 0;
    let threshold = flow_crossing_threshold(
        ctx,
        taker,
        in_leg,
        out_leg,
        ctx.rules.enabled(&feature_id("fixUniversalNumber")),
        passive,
    );
    let bridge_bound = offer_crossing_initial_bound(ctx, bridge);
    let bridge_limit = from_rate(threshold)
        .ok()
        .map(|rate| Number::from_iou(&rate));
    let pass_limit = is_sell
        .then(|| from_rate(cq).ok().map(|rate| Number::from_iou(&rate)))
        .flatten();
    if !matches!((bridge_bound, bridge_limit), (Some(bound), Some(limit)) if num_le(&bound, &limit))
    {
        return Ok(None);
    }

    // Keep a checkpoint for the complete candidate and another for each pass.
    // A sell can keep an earlier favorable band when the next one misses its
    // limit; buys still evaluate the complete candidate atomically.
    let cp = ctx.view.checkpoint();
    // `taker_acct` is a detached JSON value held by OfferCreate while the rest
    // of the candidate lives in the sandbox. Candidate execution may update its
    // OwnerCount (for example while creating a temporary trust line), so a view
    // rollback must restore this value as well.
    let taker_acct_before = taker_acct.clone();
    let mut pass_ins = Vec::new();
    let mut pass_outs = Vec::new();
    let mut bridge_reaps = Vec::new();
    let mut quality_adjusted = false;
    let mut handoff_to_direct = false;
    let mut demand = if is_sell {
        unbounded_leg(out_leg)
    } else {
        remaining_out.clone()
    };
    for _ in 0..FLOW_MAX_TRIES {
        if budget.is_zero() || (!is_sell && demand.is_zero()) {
            break;
        }
        amm_ctx.clear();
        let ri = leg_to_number(&budget);
        let ro = leg_to_number(&demand);
        let pass_cp = ctx.view.checkpoint();
        let taker_acct_before_pass = taker_acct.clone();
        let Some(res) = execute_strand_pass(
            ctx,
            taker,
            taker_acct,
            taker,
            bridge,
            &ri,
            &ro,
            &mut amm_ctx,
            /*offer_create_bridge=*/ true,
            /*source_funds_limited=*/ false,
            Some(cq),
            &mut bridge_reaps,
        ) else {
            break;
        };
        if res.out_amt.is_zero() || res.in_amt.is_zero() {
            break;
        }
        let spent = leg_from_magnitude(&res.in_amt.to_iou(), in_leg);
        let delivered = leg_from_magnitude(&res.out_amt.to_iou(), out_leg);
        if spent.is_zero() || delivered.is_zero() {
            break;
        }
        let pass_quality_adjusted = res.quality_adjusted;
        // A sell can keep its favorable first pass when the next pass exceeds
        // its quality limit. A buy keeps its bounded output demand and checks
        // the aggregate below; retaining the first pass changes its result.
        let pass_exceeds_limit = pass_limit
            .as_ref()
            .is_some_and(|limit| num_gt(&res.in_amt.div(&res.out_amt), limit));
        if (!pass_quality_adjusted && pass_exceeds_limit)
            || (pass_quality_adjusted
                && !bridge_adjusted_pass_meets_limit(
                    &res.in_amt,
                    &res.out_amt,
                    cq,
                    ctx.rules.enabled(&feature_id("fixUniversalNumber")),
                ))
        {
            ctx.view.rollback(pass_cp);
            *taker_acct = taker_acct_before_pass;
            break;
        }
        quality_adjusted |= pass_quality_adjusted;
        pass_ins.push(res.in_amt);
        pass_outs.push(res.out_amt);
        budget = leg_sub(&budget, &spent);
        if !is_sell {
            demand = if leg_ge(&delivered, &demand) {
                leg_sub(&demand, &demand)
            } else {
                leg_sub(&demand, &delivered)
            };
        }
        amm_ctx.update();
        // Flow ranks both strands again after each pass. The direct path may
        // overtake the next bridge band through either an AMM (105503523) or
        // a resting book offer (105503720).
        if let Some(direct) = build_flow_strand(
            ctx,
            &[ctx.tx["TakerGets"].clone(), ctx.tx["TakerPays"].clone()],
        ) && let (Some(direct_q), Some(bridge_q), Some(limit_q)) = (
            strand_quality_ub(ctx, &direct, taker, &amm_ctx),
            strand_quality_ub(ctx, bridge, taker, &amm_ctx),
            bridge_limit.as_ref(),
        ) && num_le(&direct_q, &bridge_q)
            && num_le(&direct_q, limit_q)
        {
            handoff_to_direct = direct.terminal_has_amm();
            break;
        }
        if pass_quality_adjusted {
            break;
        }
    }

    if pass_ins.is_empty() {
        ctx.view.rollback(cp);
        *taker_acct = taker_acct_before;
        return Ok(None);
    }
    let total_in = sum_smallest_to_largest(&pass_ins);
    let total_out = sum_smallest_to_largest(&pass_outs);
    let in_q = num_quality_iou(&total_in, in_leg.is_xrp);
    let out_q = num_quality_iou(&total_out, out_leg.is_xrp);
    let eff_q = if ctx.rules.enabled(&feature_id("fixUniversalNumber")) {
        rxrpl_amount::get_rate_round_even(&in_q, &out_q)
    } else {
        rxrpl_amount::get_rate(&in_q, &out_q)
    }
    .unwrap_or(u64::MAX);
    if !(rxrpl_amount::is_better_quality(eff_q, cq)
        || quality_adjusted && rxrpl_amount::within_relative_distance(eff_q, cq))
    {
        ctx.view.rollback(cp);
        *taker_acct = taker_acct_before;
        return Ok(None);
    }

    let spent = leg_from_magnitude(&total_in.to_iou(), in_leg);
    let delivered = leg_from_magnitude(&total_out.to_iou(), out_leg);
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    *remaining_in = leftover_leg(remaining_in, &spent, number_switchover);
    *source_remaining = leftover_leg(source_remaining, &spent, number_switchover);
    *remaining_out = if leg_ge(&delivered, remaining_out) {
        leg_sub(remaining_out, remaining_out)
    } else {
        leftover_leg(remaining_out, &delivered, number_switchover)
    };
    *crossed = true;
    Ok(handoff_to_direct.then_some(amm_ctx))
}

/// Execute the direct strand's next AMM Fibonacci offer after the bridge yields
/// to it. The regular terminal AMM swap spans a larger spot-price interval and
/// over-delivers when both strands remain active (105503523 / 27E2ED6D).
#[allow(clippy::too_many_arguments)]
fn try_direct_flow_amm_pass(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    taker_gets: &Value,
    taker_pays: &Value,
    in_leg: &Leg,
    out_leg: &Leg,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    source_remaining: &mut Leg,
    crossed: &mut bool,
    threshold: u64,
    is_sell: bool,
    amm_ctx: &mut AmmContext,
) -> Result<bool, TransactionResult> {
    let Some(direct) = build_flow_strand(ctx, &[taker_gets.clone(), taker_pays.clone()]) else {
        return Ok(false);
    };
    let budget = leg_min(remaining_in, source_remaining);
    if budget.is_zero() || (!is_sell && remaining_out.is_zero()) {
        return Ok(false);
    }
    let demand = if is_sell {
        unbounded_leg(out_leg)
    } else {
        remaining_out.clone()
    };
    let cp = ctx.view.checkpoint();
    let taker_acct_before = taker_acct.clone();
    let mut reaps = Vec::new();
    amm_ctx.set_multi_path(true);
    amm_ctx.clear();
    let pass = execute_strand_pass(
        ctx,
        taker,
        taker_acct,
        taker,
        &direct,
        &leg_to_number(&budget),
        &leg_to_number(&demand),
        amm_ctx,
        /*offer_create_bridge=*/ true,
        /*source_funds_limited=*/ false,
        Some(threshold),
        &mut reaps,
    );
    let Some(pass) = pass else {
        ctx.view.rollback(cp);
        *taker_acct = taker_acct_before;
        return Ok(false);
    };
    if pass.in_amt.is_zero() || pass.out_amt.is_zero() || pass.offers_used != 0 {
        ctx.view.rollback(cp);
        *taker_acct = taker_acct_before;
        return Ok(false);
    }
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let limit_rate =
        Number::from_iou(&from_rate(threshold).map_err(|_| TransactionResult::TefInternal)?);
    let realised_rate = pass.in_amt.div(&pass.out_amt);
    if num_gt(&realised_rate, &limit_rate) {
        ctx.view.rollback(cp);
        *taker_acct = taker_acct_before;
        return Ok(false);
    }

    let spent = leg_from_magnitude(&pass.in_amt.to_iou(), in_leg);
    let delivered = leg_from_magnitude(&pass.out_amt.to_iou(), out_leg);
    *remaining_in = leftover_leg(remaining_in, &spent, number_switchover);
    *source_remaining = leftover_leg(source_remaining, &spent, number_switchover);
    *remaining_out = if leg_ge(&delivered, remaining_out) {
        leg_sub(remaining_out, remaining_out)
    } else {
        leftover_leg(remaining_out, &delivered, number_switchover)
    };
    *crossed = true;
    amm_ctx.update();
    Ok(true)
}

fn bridge_adjusted_pass_meets_limit(
    input: &Number,
    output: &Number,
    limit: u64,
    round_even: bool,
) -> bool {
    let input = num_quality_iou(input, false);
    let output = num_quality_iou(output, false);
    let rate = if round_even {
        rxrpl_amount::get_rate_round_even(&input, &output)
    } else {
        rxrpl_amount::get_rate(&input, &output)
    };
    rate.is_ok_and(|rate| {
        rxrpl_amount::is_better_quality(rate, limit)
            || rxrpl_amount::within_relative_distance(rate, limit)
    })
}

/// Output deliverable for a given input at a resting offer's price. This mirrors
/// rippled's `Quality::ceilInStrict` (the input-limited path in `BookStep`):
/// `out = in / q` rounded DOWN (`divRoundStrict`), where `q` is the offer's
/// book-directory quality rate (`rate = in/out`) — the fixed quality bucket the
/// offer lives in, NOT the offer's drifted post-partial-fill `TakerPays/TakerGets`
/// ratio. Pricing from the bucketed dir rate is what makes the consumed/owner
/// amounts byte-exact (e.g. mainnet SOLO fills land on `…2747`, not `…2750`).
/// Magnitudes are pure `IOUAmount` (drops count as integers) to avoid the
/// native/IOU normalisation hazard of mixed-asset multiply.
fn out_for_in(in_amt: &Leg, rate: &IOUAmount, offer_out: &Leg) -> Leg {
    let in_iou = leg_as_quality_iou(in_amt);
    // Price the fill off the book directory's quality (`rate = in/out`, the fixed
    // quality bucket the offer lives in), exactly as rippled's `BookStep` uses
    // `offer.quality().rate()` rather than the offer's current (post-partial-fill)
    // `TakerPays/TakerGets` ratio. Re-deriving the rate from the drifted current
    // amounts left interior book-hop outputs 1-2 ULP off `divRoundStrict`.
    let out_iou =
        IOUAmount::div_round(&in_iou, rate, /*round_up*/ false).unwrap_or(IOUAmount::ZERO);
    leg_from_magnitude(&out_iou, offer_out)
}

#[allow(clippy::too_many_arguments)]
fn consumes_full_offer(
    take_out: &Leg,
    offer_out: &Leg,
    spendable_in: &Leg,
    offer_in: &Leg,
    remaining_out: &Leg,
    avail_out: &Leg,
    is_sell: bool,
) -> bool {
    leg_ge(take_out, offer_out)
        || (leg_ge(avail_out, offer_out)
            && leg_ge(spendable_in, offer_in)
            && (is_sell || leg_ge(remaining_out, offer_out)))
}

/// Build a leg in `template`'s asset carrying magnitude `mag`. For XRP the
/// magnitude is floored to an integer drop count (rippled never delivers a
/// fractional drop); for an IOU it is the value directly.
fn leg_from_magnitude(mag: &IOUAmount, template: &Leg) -> Leg {
    let mut out = template.clone();
    if template.is_xrp {
        let s = mag.to_decimal_string();
        let whole = s.split('.').next().unwrap_or("0");
        // Saturate rather than collapse to 0 on overflow: a magnitude beyond
        // i64::MAX drops only arises from the unbounded-budget sentinel converted
        // to XRP (an interior hop's `out_for_in(unbounded_in, rate)` in the
        // reverse pass). It means "no budget cap", so it must read as huge —
        // `unwrap_or(0)` made the budget bind to zero and starved the hop. No real
        // XRP amount exceeds i64::MAX (max supply ~1e17 drops), so fills unchanged.
        out.drops = whole.parse::<i64>().unwrap_or(i64::MAX);
    } else {
        out.iou = *mag;
    }
    out
}

/// Input required for a given output at a resting offer's quality rate, priced as
/// rippled's `Quality::ceilOutStrict`: `in = mulRoundStrict(out, rate, roundUp)`.
/// The rate is the offer's book-directory quality, fixed when the offer was placed
/// and unchanged by partial fills (rippled reprices every take at the original
/// quality, never the drifted current `TakerPays/TakerGets`). The strict
/// canonicalisation rounds up a fractional drop below 0.1 that the legacy
/// `mul_round` truncates, so an XRP take is not under-priced by a drop.
fn in_for_out(out_amt: &Leg, rate: &IOUAmount, in_template: &Leg) -> Leg {
    in_for_out_round(out_amt, rate, in_template, true)
}

fn in_for_out_round(out_amt: &Leg, rate: &IOUAmount, in_template: &Leg, round_up: bool) -> Leg {
    let amt = rxrpl_amount::Amount::mul_round_strict(
        &leg_to_amount(out_amt),
        &rxrpl_amount::Amount::Iou(*rate),
        in_template.is_xrp,
        round_up,
    )
    .unwrap_or(leg_to_amount(in_template));
    amount_to_leg(&amt, in_template)
}

/// Taker-era `Quality::ceilOut`: `mulRound`, not `mulRoundStrict`. A fractional
/// drop below 0.1 truncates; FlowCross `ceilOutStrict` would still ceil it
/// (30000057 502E1318 over-spent 1 drop, while 30000054 E60 fraction 0.88 agrees).
fn in_for_out_taker(out_amt: &Leg, rate: &IOUAmount, in_template: &Leg) -> Leg {
    let amt = rxrpl_amount::Amount::mul_round(
        &leg_to_amount(out_amt),
        &rxrpl_amount::Amount::Iou(*rate),
        in_template.is_xrp,
        true,
    )
    .unwrap_or(leg_to_amount(in_template));
    amount_to_leg(&amt, in_template)
}

/// Input for a take priced from the resting offer's own current amounts (`in =
/// ceil(offer_in * out / offer_out)`, multiply-before-divide for XRP so no drop is
/// lost). Used for the input- and funds-limited branches, where repricing the
/// clamped output at the book's quantized quality rate over-charges by a drop.
/// IOU input keeps the rate-based path.
fn in_for_out_offer(offer_in: &Leg, out_amt: &Leg, offer_out: &Leg, rate: &IOUAmount) -> Leg {
    if !offer_in.is_xrp {
        return in_for_out(out_amt, rate, offer_in);
    }
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode};
    let priced = Number::from_int(offer_in.drops)
        .mul(&Number::from_iou(&leg_as_quality_iou(out_amt)))
        .div(&Number::from_iou(&leg_as_quality_iou(offer_out)));
    let _g = RoundModeGuard::new(RoundingMode::Upward);
    let mut out = offer_in.clone();
    out.drops = priced.to_xrp_drops_mode() as i64;
    out
}

/// Cross-currency Payment book crossing under the single-taker conversion model
/// (the taker both pays the input and receives the output — used for
/// `Account == Destination` currency conversions). Walks the book of offers
/// giving `target_out`'s asset for `budget_in`'s asset, best price first, with
/// NO per-offer quality cap: rippled fills greedily until the target output is
/// delivered or the `SendMax` input budget is spent. Unfunded offers are reaped;
/// funded ones are fully or partially filled. The taker's XRP balance moves
/// through `taker_acct` (the caller writes it back). Returns
/// `(delivered_out, spent_in)`.
pub(crate) fn cross_book_payment(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    target_out: &Value,
    budget_in: &Value,
) -> Result<(Value, Value), TransactionResult> {
    let demand_out = Leg::parse(target_out).ok_or(TransactionResult::TemBadAmount)?;
    let in_tmpl = Leg::parse(budget_in).ok_or(TransactionResult::TemBadAmount)?;

    // The source cannot spend more than it holds: cap the SendMax budget at the
    // taker's spendable balance in the input asset (rippled's source-funds limit
    // / `accountFunds`). For an IOU that is the trust-line balance; for XRP it is
    // `owner_funds_leg`'s liquid balance (balance minus the reserve), so a
    // reserve-constrained source stops at its reserve instead of spending into
    // it. `pay_in` only prevents a negative balance, not a dip below the reserve.
    let mut budget = in_tmpl.clone();
    let funds = owner_funds_leg(ctx, taker, &in_tmpl);
    let source_funds_limited = leg_gt(&budget, &funds);
    if leg_ge(&budget, &funds) {
        budget = funds.clone();
    }
    let source_input_rate =
        if !in_tmpl.is_xrp && taker != &in_tmpl.issuer && dest != &in_tmpl.issuer {
            transfer_rate_value(ctx, &in_tmpl.issuer)
        } else {
            1_000_000_000
        };

    let (delivered, spent) = cross_book_hop(
        ctx,
        taker,
        taker_acct,
        dest,
        &demand_out,
        &budget,
        /*skip_input_debit=*/ false,
        /*skip_output_credit=*/ false,
        /*single_band=*/ false,
        /*allow_amm=*/ true,
        /*lq_stop=*/ None,
        /*preserve_input_limited_iou_ulp=*/ false,
        /*force_full_input_budget=*/ false,
        source_input_rate,
        source_funds_limited,
        /*direct_payment_demand=*/ true,
    )?;
    Ok((
        delivered.with_amount(&delivered.iou, delivered.drops),
        spent.with_amount(&spent.iou, spent.drops),
    ))
}

/// Cross a SINGLE order book, delivering up to `demand_out` (its magnitude is
/// the demand cap) while spending up to `budget_in` (its magnitude is the
/// budget). Best price first via `succ()` over the book's quality
/// sub-directories. Returns `(delivered_out, spent_in)` as `Leg`s in the
/// demand/budget assets. This is the byte-exact `BookStep` primitive shared by
/// the single-book conversion ([`cross_book_payment`]) and the multi-hop
/// path-payment chain ([`cross_path_payment`]).
///
/// `skip_input_debit` / `skip_output_credit` exist for INTERMEDIATE hops of a
/// multi-hop strand, where the input came from the previous book's offer owners
/// (so the taker is not debited again) and the output feeds the next book's
/// offer owners (so no account is credited the delivery). For those hops the
/// counter-party leg of the move is realised by the adjacent hop touching the
/// SAME issuer's trust lines, keeping the intermediate issuer's obligations
/// balanced. The first hop debits the taker; the last hop credits `dest`.
#[allow(clippy::too_many_arguments)]
fn cross_book_hop(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
    skip_input_debit: bool,
    skip_output_credit: bool,
    single_band: bool,
    allow_amm: bool,
    lq_stop: Option<(u64, u64)>,
    preserve_input_limited_iou_ulp: bool,
    force_full_input_budget: bool,
    source_input_rate: u32,
    source_funds_limited: bool,
    direct_payment_demand: bool,
) -> Result<(Leg, Leg), TransactionResult> {
    cross_book_hop_with_reaps(
        ctx,
        taker,
        taker_acct,
        dest,
        demand_out,
        budget_in,
        skip_input_debit,
        skip_output_credit,
        single_band,
        allow_amm,
        lq_stop,
        preserve_input_limited_iou_ulp,
        force_full_input_budget,
        source_input_rate,
        source_funds_limited,
        None,
        false,
        false,
        direct_payment_demand,
    )
}

#[allow(clippy::too_many_arguments)]
fn cross_book_hop_direct_tail(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
) -> Result<(Leg, Leg), TransactionResult> {
    cross_book_hop_with_reaps(
        ctx,
        taker,
        taker_acct,
        dest,
        demand_out,
        budget_in,
        true,
        false,
        false,
        false,
        None,
        false,
        false,
        1_000_000_000,
        false,
        None,
        true,
        false,
        false,
    )
}

/// A reverse quote rolls its sandbox back, but FlowOfferStream keeps offers that
/// were already invalid in the parent view for deletion after strand selection.
type PermanentOffer = (AccountId, Hash256, Hash256);

#[allow(clippy::too_many_arguments)]
fn cross_book_hop_with_reaps(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
    skip_input_debit: bool,
    skip_output_credit: bool,
    single_band: bool,
    allow_amm: bool,
    lq_stop: Option<(u64, u64)>,
    preserve_input_limited_iou_ulp: bool,
    force_full_input_budget: bool,
    source_input_rate: u32,
    source_funds_limited: bool,
    mut permanent_reaps: Option<&mut Vec<PermanentOffer>>,
    reverse_direct_tail: bool,
    preserve_source_residual: bool,
    direct_payment_demand: bool,
) -> Result<(Leg, Leg), TransactionResult> {
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let inverse_book = keylet::book_dir(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );

    let mut remaining_out = demand_out.clone();
    let mut remaining_in = budget_in.clone();
    let mut consumed_whole_offer = false;
    let mut xrp_half_up: Option<AccountId> = None;
    let dest_line_key = keylet::trust_line(dest, &demand_out.issuer, &demand_out.currency);
    let dest_line_before = if !skip_output_credit && !demand_out.is_xrp {
        ctx.view.read(&dest_line_key).and_then(|b| {
            let v: Value = serde_json::from_slice(&b).ok()?;
            v.get("Balance")?.get("value")?.as_str().map(str::to_string)
        })
    } else {
        None
    };
    // When `single_band`, the walk consumes only ONE quality level (the first
    // page on which a funded offer is actually filled). rippled's `forEachOffer`
    // processes one quality band per `BookStep::rev/fwd`; the multi-path Flow loop
    // (`flow_multi`) drives one band per pass so a hop's CLOB liquidity interleaves
    // with the shared AMM's fib chunks. Unfunded/skipped offers do not lock the
    // band (they are reaped and the walk continues to the next page).
    let mut band_quality: Option<u64> = None;
    // Accumulate the realised delivery / spend directly from each fill. Computing
    // them as `start - remaining` underflows for an interior hop, whose demand cap
    // is the huge `unbounded_leg` sentinel (~1e96): a real ~0.2 delivery is below
    // its ULP, so the subtraction would round to 0 and starve the next hop.
    let mut delivered = leg_from_magnitude(&IOUAmount::ZERO, demand_out);
    delivered.drops = 0;
    let mut delivered_parts: Vec<Number> = Vec::new();
    let mut spent = leg_from_magnitude(&IOUAmount::ZERO, budget_in);
    spent.drops = 0;
    let book_prefix = inverse_book.as_bytes()[0..24].to_vec();
    let mut probe = book_dir_with_quality(&inverse_book, 0);
    'walk: while let Some(dir_key) = ctx.view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != book_prefix[..] {
            break;
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        // Bounded to a single quality band: once an offer at quality Q has been
        // filled, stop at the first page of a worse quality.
        if let Some((prefix, limit)) = lq_stop {
            let mut pq = if prefix == 0 {
                dir_quality
            } else {
                composed_quality(prefix, dir_quality)
            };
            if !demand_out.is_xrp && demand_out.issuer != *dest {
                let tr = transfer_rate(ctx, &demand_out.issuer);
                let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
                if tr > one {
                    pq = composed_quality(pq, pack_rate(&tr));
                }
            }
            if limit != 0 && pq > limit {
                break 'walk;
            }
        } else if single_band {
            if let Some(bq) = band_quality {
                if bq != dir_quality {
                    break 'walk;
                }
            }
        }
        let offers = book_dir_indexes(ctx, &dir_key);
        // BookStep asks the AMM to compete with the first crossable offer, not
        // merely the first directory. Empty, expired, self-owned, or unfunded
        // pages are skipped before their quality can cap the synthetic offer.
        let funded_tip = offers.iter().any(|offer_key| {
            let Some(ob) = ctx.view.read(offer_key) else {
                return false;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                return false;
            };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                return false;
            }
            if offer
                .get("Expiration")
                .and_then(|v| v.as_u64())
                .is_some_and(|exp| exp <= ctx.view.parent_close_time() as u64)
            {
                return false;
            }
            let Some(owner) = offer
                .get("Account")
                .and_then(|v| v.as_str())
                .and_then(|s| decode_account_id(s).ok())
            else {
                return false;
            };
            if &owner == taker {
                return false;
            }
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
                return false;
            };
            !owner_funds_leg(ctx, &owner, &offer_out).is_zero()
        });
        // Interleave the AMM at this quality band BEFORE crossing the resting
        // CLOB offers, mirroring cross_offers' try_amm_step (rippled BookStep
        // tryAMM/execOffer): amm_hop's raw spot gate admits the pool
        // only when it strictly beats this band's quality, so it crosses exactly
        // where the AMM is at least as good as the offers about to fill and is a
        // no-op on a pure-CLOB book. The output lands on `dest`; interior hops
        // carry the previous hop's delivery so their input is not funds-capped.
        if allow_amm
            && funded_tip
            && !remaining_in.is_zero()
            && !hop_out_done(&remaining_out, demand_out)
        {
            let amm_budget = if skip_input_debit {
                remaining_in.clone()
            } else {
                leg_min(&remaining_in, &owner_funds_leg(ctx, taker, budget_in))
            };
            if !amm_budget.is_zero() {
                let amm_quote = amm_hop(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    &remaining_out,
                    &amm_budget,
                    skip_input_debit,
                    skip_output_credit,
                    /*create_missing_dest_line=*/ true,
                    /*partial=*/ true,
                    /*reverse_output_limited=*/ false,
                    /*target_quality=*/ Some(dir_quality),
                    /*pool_target_quality=*/ Some(dir_quality),
                    /*cap_ending_spot_price=*/ true,
                    /*waive_output_transfer_fee=*/ false,
                )?;
                match amm_quote {
                    AmmHopResult::Filled(amm_out, amm_spent) => {
                        remaining_out = leg_sub_round(&remaining_out, &amm_out);
                        remaining_in = leg_sub_round(&remaining_in, &amm_spent);
                        delivered = leg_add(&delivered, &amm_out);
                        delivered_parts.push(leg_to_number(&amm_out));
                        spent = leg_add(&spent, &amm_spent);
                    }
                    AmmHopResult::ZeroOutput => break 'walk,
                    AmmHopResult::Unavailable => {}
                }
            }
            if remaining_in.is_zero() || hop_out_done(&remaining_out, demand_out) {
                break 'walk;
            }
        }
        let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);

        for offer_key in offers {
            if hop_out_done(&remaining_out, demand_out) {
                break 'walk;
            }
            // After an ordinary input exhaustion, BookStep stops before the
            // next offer. A source-limited direct conversion still inspects
            // expiration on the next tip (105500249), but does not reap an
            // unexpired unfunded tip (105500652).
            if book_step_input_exhausted(&spent, &remaining_in) && !source_funds_limited {
                break 'walk;
            }
            let Some(ob) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }
            let owner_str = offer.get("Account").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(owner) = decode_account_id(owner_str) else {
                continue;
            };
            if &owner == taker {
                continue;
            }
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
                continue;
            };
            let Some(offer_in) = Leg::parse(&offer["TakerPays"]) else {
                continue;
            };
            if offer
                .get("Expiration")
                .and_then(|v| v.as_u64())
                .is_some_and(|exp| exp <= ctx.view.parent_close_time() as u64)
            {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                if let Some(reaps) = permanent_reaps.as_mut() {
                    reaps.push((owner, offer_key, dir_key));
                }
                continue;
            }
            if book_step_input_exhausted(&spent, &remaining_in) {
                break 'walk;
            }
            let funds = owner_funds_leg(ctx, &owner, &offer_out);
            if funds.is_zero() {
                let originally_unfunded =
                    original_owner_funds_leg(ctx, &owner, &offer_out).is_zero();
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                if originally_unfunded && let Some(reaps) = permanent_reaps.as_mut() {
                    reaps.push((owner, offer_key, dir_key));
                }
                continue;
            }
            if should_reap_tiny_increased_quality(
                &owner,
                &offer_in,
                &offer_out,
                &funds,
                dir_quality,
            ) {
                let originally_tiny =
                    tiny_offer_is_permanently_reapable(ctx, &owner, &offer_out, &funds);
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                if originally_tiny && let Some(reaps) = permanent_reaps.as_mut() {
                    reaps.push((owner, offer_key, dir_key));
                }
                continue;
            }
            // Keep inspecting invalid offers after source funds are exhausted,
            // but do not spend a representable arithmetic remainder on a later
            // funded offer from the same page (105500929, TX42).
            if !skip_input_debit
                && !budget_in.is_xrp
                && !spent.is_zero()
                && owner_funds_leg(ctx, taker, budget_in).is_zero()
            {
                break 'walk;
            }
            let avail_out = leg_min(&offer_out, &funds);
            // OfferStream continues stepping invalid entries after source funds
            // are exhausted, so expired/unfunded offers are still reaped. A
            // funded IOU remainder may also cost less than one XRP drop when a
            // step starts with no representable input (105500212 6B2B088C).
            // Once this step has spent input, however, BookStep stops before
            // advancing to a later zero-cost offer (105500592 AFD10F04).
            let zero_cost_funded = zero_cost_funded_xrp_offer(
                &spent,
                &remaining_in,
                &offer_in,
                &avail_out,
                &offer_out,
            );
            if remaining_in.is_zero() && !zero_cost_funded {
                break 'walk;
            }

            // Pricing rate: the offer's book-directory quality (`in/out`). A
            // degenerate page quality of zero only arises in synthetic ledgers
            // whose offer sits at the book base rather than a quality sub-dir;
            // there, fall back to the offer's current amounts so those fixtures
            // still price. Real mainnet book pages always carry a valid quality.
            let eff_rate = if rate.is_zero() {
                IOUAmount::divide(
                    &leg_as_quality_iou(&offer_in),
                    &leg_as_quality_iou(&offer_out),
                )
                .unwrap_or(IOUAmount::ZERO)
            } else {
                rate
            };

            // Output capped by remaining demand, funded availability, and what
            // the remaining input budget can buy at this offer's price.
            // Last-hop remaining is NET dest credit; TakerGets is GROSS. When a
            // transfer fee applies, cap take_out at remaining*rate so dest nets
            // remaining (30000046 63B72EB4: 1.002 left a 50 JPY shortfall that
            // walked worse JPY/XRP offers).
            let terminal = !skip_input_debit && !skip_output_credit;
            let fee_applies = !skip_output_credit
                && !offer_out.is_xrp
                && offer_out.issuer != owner
                && offer_out.issuer != *dest;
            let remaining_out_cap = if fee_applies {
                let rate = transfer_rate(ctx, &offer_out.issuer);
                let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
                if rate > one {
                    let mut cap = remaining_out.clone();
                    cap.iou = grossed(&remaining_out.iou, &rate);
                    // A multi-hop last hop may need one extra gross ULP so the
                    // recipient nets the requested Amount (30000051). Only add
                    // it when the initial gross-up still nets short: in a later
                    // strand pass it can already net exactly the remaining
                    // demand (105501947 / TX21).
                    if !terminal
                        && gross_book_output_cap_nets_short(
                            &cap.iou,
                            &remaining_out.iou,
                            transfer_rate_value(ctx, &offer_out.issuer),
                        )
                    {
                        if let Ok(bumped) =
                            IOUAmount::from_parts(cap.iou.mantissa() + 1, cap.iou.exponent(), false)
                        {
                            cap.iou = bumped;
                        }
                    }
                    cap
                } else {
                    remaining_out.clone()
                }
            } else {
                remaining_out.clone()
            };
            // BookPaymentStep accounts input in two domains: the offer receives
            // net TakerPays (`order_in`), while the source budget/funds are gross
            // of the input issuer's transfer fee (`step_in`). Price the offer
            // against the net amount that the remaining gross budget can fund.
            let net_remaining_in = net_book_payment_input(&remaining_in, source_input_rate);
            let budget_out = out_for_in(&net_remaining_in, &eff_rate, &offer_out);
            let mut take_out = leg_min(&remaining_out_cap, &avail_out);
            // rippled forks on the INPUT side (StrandFlow reverse-then-forward,
            // maxIn < requiredIn at step 0): the SendMax budget binds only when it
            // cannot afford the ceilOut input cost of the demanded output. A
            // floored `budget_out` that merely equals demand is not sufficient to
            // prove the budget binds (105500016), but one strictly below demand is
            // definitive even if proportional offer-field pricing rounds down.
            // This step-0 forward re-run only applies to a TERMINAL take with a
            // bounded demand (single-book conversion). An interior hop carries the
            // unbounded-demand sentinel, so its ceilOut input cost is meaningless;
            // keep the output-side test there (the #337 walk-stop relies on it).
            // For a fully funded XRP offer, the exact TakerPays can fit the
            // source budget even when flooring by the rounded directory quality
            // yields zero drops. The full offer remains affordable in that case.
            let budget_binds = if (force_full_input_budget
                && offer_out.is_xrp
                && leg_ge(&avail_out, &offer_out)
                && leg_ge(&take_out, &offer_out)
                && budget_out.is_zero()
                && leg_ge(&net_remaining_in, &offer_in))
                || funds_limited_offer_fits_budget(
                    &offer_in,
                    &offer_out,
                    &avail_out,
                    &take_out,
                    &net_remaining_in,
                )
                || zero_cost_funded
            {
                false
            } else if terminal {
                terminal_budget_binds(
                    &net_remaining_in,
                    &take_out,
                    &budget_out,
                    &offer_in,
                    &offer_out,
                    &eff_rate,
                    leg_ge(&take_out, &avail_out),
                )
            } else if force_full_input_budget && offer_out.is_xrp {
                // OfferCreate's first autobridge leg is reverse-priced to the
                // exact XRP demand of the second leg. A quality-floor estimate
                // can sit one drop below that demand even though the reverse
                // budget covers its proportional ceilOut cost (105500249
                // 4C84DE5C). Preserve the reverse demand when it is affordable.
                !leg_ge(
                    &net_remaining_in,
                    &in_for_out_offer(&offer_in, &take_out, &offer_out, &eff_rate),
                )
            } else if !skip_input_debit
                && skip_output_credit
                && offer_in.is_xrp
                && !offer_out.is_xrp
                && leg_gt(&offer_out, &take_out)
                && leg_ge(&net_remaining_in, &offer_in)
            {
                // The reverse pass may demand slightly less than this offer's
                // TakerGets while its XRP TakerPays fits SendMax exactly. The
                // directory-quality estimate can floor below that demand,
                // but BookStep::fwdImp keeps limitStepOut's reverse output and
                // clamps the input cost to TakerPays (105503019/TX39).
                false
            } else {
                leg_ge(&take_out, &budget_out)
            };
            if budget_binds {
                take_out = budget_out.clone();
            }
            // The remaining input budget can no longer buy even one unit of this
            // book's output (a sub-drop / sub-ULP residual left after the binding
            // offer took the bulk): the input is exhausted, so the step is done.
            // rippled's input-limited `BookStep` stops here; walking on would cross
            // every worse-priced offer for a dust `order_in` that rounds to ~0 and
            // eventually fault paying an unrepresentable amount (a stuck interior
            // hop that fails the whole strand).
            if budget_binds && take_out.is_zero() {
                break 'walk;
            }

            // Paying the offer's TakerPays must consume TakerGets. out_for_in
            // of exact Pays can floor short of Gets (30000099 0C0344E7).
            let full_take = full_offer_take(
                &take_out,
                &avail_out,
                &remaining_in,
                &remaining_out_cap,
                &offer_in,
                &offer_out,
            );
            if full_take {
                consumed_whole_offer = true;
            }
            let funds_limited = !full_take && !budget_binds && leg_ge(&take_out, &avail_out);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else if budget_binds {
                let order_in = if input_limited_uses_full_budget(
                    terminal,
                    skip_input_debit,
                    skip_output_credit,
                    &offer_out,
                    force_full_input_budget,
                ) {
                    // ceilIn: the SendMax input is the binding limit (it cannot
                    // afford the ceilOut cost of the demanded output), so rippled
                    // re-runs the terminal book forward with `in = maxIn` and spends
                    // the WHOLE remaining budget — the forward re-run asserts
                    // `result.in == limit` and ceilIn pins in=limit,
                    // out=floor(limit/rate). The same applies to every
                    // input-limited IOU->XRP BookStep: drops are discrete, so
                    // repricing the floored output can leave an artificial IOU
                    // residual at the source or between steps and can dry the
                    // otherwise valid strand (ledgers 105500157, 105500352).
                    leg_min(&net_remaining_in, &offer_in)
                } else {
                    // Interior hop: charge the ceilOut price for the delivered
                    // output and leave the sub-drop budget remainder unspent, never
                    // over-charging the maker for dust the budget can't fully buy
                    // (#336/#337). Spending the whole budget drifts an interior hop.
                    leg_min(
                        &leg_min(
                            &in_for_out_offer(&offer_in, &take_out, &offer_out, &eff_rate),
                            &offer_in,
                        ),
                        &net_remaining_in,
                    )
                };
                (take_out.clone(), order_in)
            } else if funds_limited {
                // Interior hops: scale TakerPays * funded / TakerGets rounded
                // DOWN so a ceil-priced input does not over-charge the next book.
                // Terminal hops: BookStep ceils IOU input (`Quality::ceilOutStrict`)
                // after limiting the offer itself with `roundUp=false`; the
                // transfer-rate gross-up happens later in `step_in`. XRP
                // TakerPays normally stays proportional: ceiling it over-spent
                // 2127 drops on 30000046/63B72EB4.
                let order_in = if skip_input_debit
                    && !skip_output_credit
                    && offer_in.is_xrp
                    && ctx.tx.get("TransactionType").and_then(Value::as_str) == Some("OfferCreate")
                {
                    // The last XRP bridge book prices a funded maker's XRP
                    // input at its fixed directory quality, even when the
                    // offer's current fields have drifted (105503833).
                    leg_min(
                        &leg_min(
                            &in_for_out_round(&take_out, &eff_rate, &offer_in, false),
                            &offer_in,
                        ),
                        &net_remaining_in,
                    )
                } else if terminal && !offer_in.is_xrp {
                    leg_min(
                        &leg_min(
                            &in_for_out_round(&take_out, &eff_rate, &offer_in, false),
                            &offer_in,
                        ),
                        &net_remaining_in,
                    )
                } else {
                    let scaled = bridge_funded_xrp_input(
                        &offer_in,
                        &avail_out,
                        &offer_out,
                        &eff_rate,
                        force_full_input_budget,
                    );
                    let floor = leg_min(&leg_min(&scaled, &offer_in), &remaining_in);
                    // The post-hop +1 drop is part of rippled's transfer-fee
                    // gross-up path (30000046 B19E), not generic proportional
                    // XRP scaling. Applying it to a parity-rate issue overpays
                    // an underfunded maker by one drop (105500212 6B2B088C).
                    if funds_limited_xrp_rounds_up(
                        terminal,
                        offer_in.is_xrp,
                        floor.drops,
                        fee_applies,
                        &transfer_rate(ctx, &offer_out.issuer),
                    ) && xrp_half_up.is_none()
                    {
                        xrp_half_up = Some(owner);
                    }
                    floor
                };
                (take_out.clone(), order_in)
            } else {
                // Demand-limited: price at the offer's book quality, never
                // exceeding the resting offer or the remaining budget. Pricing
                // from the offer's drifted current amounts under-prices an XRP
                // take by a drop. Pre-FlowCross Taker uses `mulRound` (fraction
                // below 0.1 drop truncates); FlowCross BookStep uses
                // `ceilOutStrict`.
                let priced = if ctx.rules.enabled(&feature_id("FlowCross")) {
                    in_for_out(&take_out, &eff_rate, &offer_in)
                } else {
                    in_for_out_taker(&take_out, &eff_rate, &offer_in)
                };
                let order_in = leg_min(&leg_min(&priced, &offer_in), &net_remaining_in);
                (take_out.clone(), order_in)
            };

            let (order_out, mut order_in) = {
                let mut order_in = order_in;
                if legacy_intermediate_xrp_rounding_applies(
                    skip_input_debit,
                    skip_output_credit,
                    offer_in.is_xrp,
                    order_in.drops,
                    net_remaining_in.drops,
                    offer_in.drops,
                ) {
                    order_in.drops += 2;
                }
                (order_out, order_in)
            };

            // The fee-capped forward pass can quote one IOU ULP below the
            // amount removed from the resting offer after Number rounds its
            // residual. For a newly created maker line, rippled credits that
            // canonical consumed amount (105503449/BEF9CAC7).
            if terminal
                && budget_binds
                && source_input_rate != 1_000_000_000
                && !order_in.is_xrp
                && ctx
                    .view
                    .read(&keylet::trust_line(
                        &owner,
                        &order_in.issuer,
                        &order_in.currency,
                    ))
                    .is_none()
            {
                let residual = leftover_leg(&offer_in, &order_in, number_switchover);
                let canonical = leftover_leg(&offer_in, &residual, number_switchover);
                if leg_gt(&canonical, &order_in) {
                    order_in = canonical;
                }
            }

            let step_in =
                gross_book_payment_input(&order_in, &remaining_in, source_input_rate, budget_binds);
            if source_input_rate == 1_000_000_000 {
                pay_in(
                    ctx,
                    taker,
                    taker_acct,
                    &owner,
                    &order_in,
                    number_switchover,
                    PayInOptions {
                        skip_taker_debit: skip_input_debit,
                        allow_dust_drain: !preserve_source_residual
                            && book_payment_dust_drain(
                                full_take,
                                ctx.tx.get("TransactionType").and_then(Value::as_str)
                                    == Some("Payment")
                                    && taker == dest,
                            ),
                        gross_override: None,
                    },
                )?;
            } else {
                pay_in_book_payment(
                    ctx,
                    taker,
                    taker_acct,
                    &owner,
                    &order_in,
                    &step_in,
                    number_switchover,
                    skip_input_debit,
                )?;
            }
            // `order_out` is the GROSS the offer/owner gives (the TakerGets it
            // reduces by); `net_out` is what `dest` actually receives after the
            // output issuer's transfer fee. The book demand (`remaining_out`) is
            // tracked in NET terms, so it decrements by `net_out`, not the gross.
            let balance_rounding = if number_switchover {
                BalanceRounding::Nearest
            } else {
                BalanceRounding::Legacy
            };
            let owner_rounding = if reverse_direct_tail {
                BalanceRounding::DebitOwner
            } else {
                balance_rounding
            };
            let fee_rounding = book_output_fee_rounding(budget_binds, skip_output_credit);
            let net_out = pay_out_gross(
                ctx,
                taker,
                taker_acct,
                &owner,
                dest,
                &order_out,
                number_switchover,
                owner_rounding,
                balance_rounding,
                skip_output_credit,
                fee_rounding,
                false,
            )?;
            // Debiting Gets (e=-19) into a 22.45 BTC line (e=-14) truncates 1
            // ULP vs mainnet (30000099 0C0344E7). Give it back when the line
            // is coarser than Gets by 4+ exponents.
            if full_take && skip_input_debit && !skip_output_credit {
                credit_line_ulp_if_coarse(ctx, &owner, &order_out, number_switchover)?;
            }

            if full_take || funds_limited {
                let mut consumed = offer.clone();
                if full_take {
                    consumed["TakerGets"] = offer_out.with_amount(&IOUAmount::ZERO, 0);
                    consumed["TakerPays"] = offer_in.with_amount(&IOUAmount::ZERO, 0);
                } else {
                    let new_gets = leftover_leg(&offer_out, &order_out, number_switchover);
                    let mut new_pays = leftover_leg(&offer_in, &order_in, number_switchover);
                    if xrp_half_up == Some(owner) && new_pays.is_xrp && new_pays.drops > 0 {
                        new_pays.drops -= 1;
                    }
                    consumed["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                    consumed["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                }
                let cb =
                    serde_json::to_vec(&consumed).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, cb)
                    .map_err(|_| TransactionResult::TefInternal)?;
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
            } else {
                let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
                let mut new_gets = if reverse_direct_tail {
                    leg_sub_directional(&offer_out, &order_out, RoundingMode::Downward)
                } else {
                    leftover_leg(&offer_out, &order_out, number_switchover)
                };
                // Last-hop input-limited: `out_for_in` floors 1 ULP below the
                // consumed Gets (30000066 33FB4822 leftover `…6341` vs `…634`).
                // Apply that correction only while the hop remains partial. If
                // this fill satisfies the requested output exactly, rippled
                // keeps the untrimmed leftover (105500157 C32058ED).
                if should_trim_input_limited_iou_leftover(
                    skip_input_debit,
                    skip_output_credit,
                    budget_binds,
                    &new_gets,
                    preserve_input_limited_iou_ulp,
                    &net_out,
                    &remaining_out,
                ) {
                    if let Ok(trimmed) = IOUAmount::from_parts(
                        new_gets.iou.mantissa() - 1,
                        new_gets.iou.exponent(),
                        false,
                    ) {
                        new_gets.iou = trimmed;
                    }
                }
                let new_pays = if reverse_direct_tail {
                    leg_sub_directional(&offer_in, &order_in, RoundingMode::Upward)
                } else {
                    leftover_leg(&offer_in, &order_in, number_switchover)
                };
                let mut reduced = offer.clone();
                reduced["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                reduced["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                let rb =
                    serde_json::to_vec(&reduced).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, rb)
                    .map_err(|_| TransactionResult::TefInternal)?;
                // A proportional fill can exhaust one side exactly while
                // leaving representable dust on the other. rippled records
                // those asymmetric residuals in DeletedNode.FinalFields, then
                // removes the dry offer. Leaving the zero-sided Offer in the
                // sandbox violates NoBadOffers and rolls back the transaction.
                if new_gets.is_zero() || new_pays.is_zero() {
                    reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                }
            }

            remaining_out = leg_sub_round(&remaining_out, &net_out);
            remaining_in = leg_sub_round(&remaining_in, &step_in);
            delivered = leg_add(&delivered, &net_out);
            delivered_parts.push(leg_to_number(&net_out));
            spent = leg_add(&spent, &step_in);
            // Lock the band on the first funded fill: subsequent pages of a worse
            // quality are not consumed this call.
            if single_band {
                band_quality = Some(dir_quality);
            }
        }
    }

    // Flow accumulates pass results in a sorted multiset, then folds from the
    // smallest amount upward. Preserve that order here even though this legacy
    // single-book driver walks all bands in one call.
    if !demand_out.is_xrp && delivered_parts.len() > 1 {
        delivered = number_to_leg(&sum_smallest_to_largest(&delivered_parts), demand_out);
    }

    // Per-fill dest credits truncate; their sum sat 5e-14 below Amount on
    // 63B72EB4. Reconcile with one add of the canonical BookStep output;
    // a direct conversion uses its realised delivery when it misses Amount.
    if let Some(before_s) = dest_line_before {
        if dest != &demand_out.issuer {
            let credit = book_destination_credit(
                demand_out,
                &remaining_out,
                &delivered,
                direct_payment_demand,
                should_bump_partial_path_credit(
                    skip_input_debit,
                    consumed_whole_offer,
                    taker,
                    dest,
                ),
            );
            if let Ok(before) = IOUAmount::from_decimal_string(&before_s) {
                let holder_is_high = dest.as_bytes() > demand_out.issuer.as_bytes();
                let delta = if holder_is_high {
                    credit.negate()
                } else {
                    credit
                };
                if let Ok(expected) = add_iou_balance(&before, &delta, number_switchover) {
                    if let Some(bytes) = ctx.view.read(&dest_line_key) {
                        if let Ok(mut line) = serde_json::from_slice::<Value>(&bytes) {
                            line["Balance"]["value"] = Value::String(expected.to_decimal_string());
                            if let Ok(nb) = serde_json::to_vec(&line) {
                                let _ = ctx.view.update(dest_line_key, nb);
                            }
                        }
                    }
                }
            }
        }
    }

    if !skip_input_debit {
        if let Some(owner) = xrp_half_up {
            credit_xrp(ctx, &owner, 1)?;
            let bal = helpers::get_balance(taker_acct) as i64 - 1;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedOffer);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
    }

    Ok((delivered, spent))
}

fn terminal_budget_binds(
    remaining_in: &Leg,
    take_out: &Leg,
    budget_out: &Leg,
    offer_in: &Leg,
    offer_out: &Leg,
    rate: &IOUAmount,
    funds_cap: bool,
) -> bool {
    // A proportional cost computed from the offer's current fields can round
    // down enough to fit even when the fixed book quality says the budget buys
    // strictly less than the requested output. Preserve the proportional check
    // for floor-equality cases, but bind when floor(in / quality) is strictly
    // below an ordinary demand (105500074). An underfunded maker is different:
    // rippled reduces the offer first and compares that reduced cost (105500388).
    let proportional_cost = in_for_out_offer(offer_in, take_out, offer_out, rate);
    if funds_cap {
        let funds_cost = scale_funds_limited_input(offer_in, take_out, offer_out);
        if leg_ge(remaining_in, &funds_cost) {
            return false;
        }
    }
    !leg_ge(budget_out, take_out) || !leg_ge(remaining_in, &proportional_cost)
}

/// BookStep reduces an underfunded maker's offer to its available output
/// before comparing that offer's proportional XRP input with the taker's
/// budget. A quality-floor quote may buy slightly less output with the same
/// integral drop cost, but the affordable reduced offer still drains the maker.
fn funds_limited_offer_fits_budget(
    offer_in: &Leg,
    offer_out: &Leg,
    available: &Leg,
    requested: &Leg,
    remaining_in: &Leg,
) -> bool {
    leg_gt(offer_out, available)
        && leg_ge(requested, available)
        && leg_ge(
            remaining_in,
            &scale_funds_limited_input(offer_in, available, offer_out),
        )
}

fn funds_limited_xrp_rounds_up(
    terminal: bool,
    offer_in_is_xrp: bool,
    floor_drops: i64,
    fee_applies: bool,
    output_rate: &IOUAmount,
) -> bool {
    terminal
        && offer_in_is_xrp
        && floor_drops >= 1_000_000
        && fee_applies
        && *output_rate > parity_rate()
}

/// Scale a funded offer's input side by `available / original output`.
///
/// Native input must multiply before dividing and then truncate to drops. Doing
/// the ratio first loses precision at the IOUAmount boundary and can round the
/// native result up by one drop (105500212 6B2B088C).
fn scale_funds_limited_input(offer_in: &Leg, available: &Leg, offer_out: &Leg) -> Leg {
    if offer_in.is_xrp {
        use rxrpl_amount::number::Number;
        let scaled = Number::from_int(offer_in.drops)
            .mul(&Number::from_iou(&leg_as_quality_iou(available)))
            .div(&Number::from_iou(&leg_as_quality_iou(offer_out)));
        let mut out = offer_in.clone();
        out.drops = scaled.to_xrp_drops() as i64;
        return out;
    }

    let ratio = IOUAmount::divide(
        &leg_as_quality_iou(available),
        &leg_as_quality_iou(offer_out),
    )
    .unwrap_or(IOUAmount::ZERO);
    let scaled =
        IOUAmount::multiply(&leg_as_quality_iou(offer_in), &ratio).unwrap_or(IOUAmount::ZERO);
    leg_from_magnitude(&scaled, offer_in)
}

/// The XRP bridge's first book must exhaust a maker whose funded XRP is one
/// drop above the output bought by proportional pricing of its drifted fields.
/// Its fixed directory quality supplies the exact funded cap (105502181/TX36).
fn bridge_funded_xrp_input(
    offer_in: &Leg,
    available: &Leg,
    offer_out: &Leg,
    rate: &IOUAmount,
    bridge_first_hop: bool,
) -> Leg {
    let scaled = scale_funds_limited_input(offer_in, available, offer_out);
    if bridge_first_hop && available.is_xrp && !offer_in.is_xrp {
        let repriced_out = out_for_in(&scaled, rate, offer_out);
        if repriced_out.drops.checked_add(1) == Some(available.drops) {
            return in_for_out(available, rate, offer_in);
        }
    }
    scaled
}

fn zero_cost_funded_xrp_offer(
    spent_so_far: &Leg,
    remaining_in: &Leg,
    offer_in: &Leg,
    available: &Leg,
    offer_out: &Leg,
) -> bool {
    spent_so_far.is_zero()
        && remaining_in.is_zero()
        && offer_in.is_xrp
        && !available.is_zero()
        && !leg_ge(available, offer_out)
        && scale_funds_limited_input(offer_in, available, offer_out).is_zero()
}

fn book_step_input_exhausted(spent_so_far: &Leg, remaining_in: &Leg) -> bool {
    !spent_so_far.is_zero() && remaining_in.is_zero()
}

fn input_limited_uses_full_budget(
    terminal: bool,
    _skip_input_debit: bool,
    _skip_output_credit: bool,
    offer_out: &Leg,
    force_full_input_budget: bool,
) -> bool {
    terminal || force_full_input_budget || offer_out.is_xrp
}

fn should_trim_input_limited_iou_leftover(
    skip_input_debit: bool,
    skip_output_credit: bool,
    budget_binds: bool,
    new_gets: &Leg,
    preserve_input_limited_iou_ulp: bool,
    net_out: &Leg,
    remaining_out: &Leg,
) -> bool {
    skip_input_debit
        && !skip_output_credit
        && budget_binds
        && !new_gets.is_xrp
        && !preserve_input_limited_iou_ulp
        && !leg_ge(net_out, remaining_out)
        && new_gets.iou.mantissa() > 0
}

fn preserve_flow_input_limited_iou_ulp(
    hop: usize,
    limiting: usize,
    input_is_xrp: bool,
    terminal_self_payment: bool,
) -> bool {
    // A forward BookStep fed by discrete XRP drops keeps the offer remainder
    // produced by limitStepIn. Re-trimming that IOU remainder loses one ULP per
    // pass, regardless of whether the drops came from an AMM or another book.
    // A partial self-payment's terminal step has the same Number subtraction
    // rule even when it is itself the limiting step (105500439 E9D931B0).
    terminal_self_payment || (hop > 0 && hop > limiting && input_is_xrp)
}

/// Legacy Flow rounds an interior XRP take two drops above the directory-quality
/// price (30000103). Applying the same adjustment to a source-facing book moves
/// those drops out of the final input-limited fill.
fn legacy_intermediate_xrp_rounding_applies(
    skip_input_debit: bool,
    skip_output_credit: bool,
    offer_in_is_xrp: bool,
    order_in_drops: i64,
    remaining_in_drops: i64,
    offer_in_drops: i64,
) -> bool {
    skip_input_debit
        && skip_output_credit
        && offer_in_is_xrp
        && (100_000_000..200_000_000).contains(&order_in_drops)
        && order_in_drops + 2 <= remaining_in_drops
        && order_in_drops + 2 <= offer_in_drops
}

fn should_bump_partial_path_credit(
    skip_input_debit: bool,
    consumed_whole_offer: bool,
    taker: &AccountId,
    dest: &AccountId,
) -> bool {
    // A self-payment already carries the final hop output back to the same
    // account. Its destination line must use the delivered amount verbatim;
    // adding the path-credit ULP would count that rounding correction twice.
    skip_input_debit && !consumed_whole_offer && taker != dest
}

fn book_destination_credit(
    demand: &Leg,
    remaining: &Leg,
    delivered: &Leg,
    direct_payment_demand: bool,
    bump_partial_path: bool,
) -> IOUAmount {
    // Flow's reverse quote supplies a canonical sub-demand for a BookStep,
    // even on a partial Payment. A direct conversion instead supplies Amount
    // as a ceiling; dust below that ceiling is an actual delivery shortfall.
    if remaining.is_zero()
        || (remaining_is_filled(remaining, demand)
            && (!direct_payment_demand || leg_ge(delivered, demand)))
    {
        demand.iou
    } else if bump_partial_path {
        let d = delivered.iou;
        IOUAmount::from_parts(d.mantissa() + 1, d.exponent(), false).unwrap_or(d)
    } else {
        delivered.iou
    }
}

/// The reverse BookStep quotes the requested output before the source budget
/// limits the forward fill. OfferStream advances past a short first quality
/// band, permanently removing expired offers before checking the next band's
/// quality. Preserve those removals even if the forward fill stops in that
/// first band with a one-drop offer remainder.
fn reap_expired_reverse_xrp_book_tail(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    input: &Leg,
    output: &Leg,
    demand: &Leg,
) -> Vec<PermanentOffer> {
    let mut reaps = Vec::new();
    if !output.is_xrp || !demand.is_xrp {
        return reaps;
    }
    let book = keylet::book_dir(
        &input.currency,
        &input.issuer,
        &output.currency,
        &output.issuer,
    );
    let prefix = &book.as_bytes()[..24];
    let mut probe = book_dir_with_quality(&book, 0);
    let mut first_quality = None;
    let mut available = 0u64;
    while let Some(dir_key) = ctx.view.succ(&probe) {
        if &dir_key.as_bytes()[..24] != prefix {
            break;
        }
        probe = dir_key;
        let quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        for offer_key in book_dir_indexes(ctx, &dir_key) {
            let Some(bytes) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(Value::as_str) != Some("Offer") {
                continue;
            }
            let Some(owner) = offer
                .get("Account")
                .and_then(Value::as_str)
                .and_then(|address| decode_account_id(address).ok())
            else {
                continue;
            };
            if &owner == taker {
                continue;
            }
            if offer
                .get("Expiration")
                .and_then(Value::as_u64)
                .is_some_and(|expiration| expiration <= ctx.view.parent_close_time() as u64)
            {
                reaps.push((owner, offer_key, dir_key));
                continue;
            }
            // BookStep checks the next funded offer's quality only after the
            // stream has discarded expired entries in front of it.
            if first_quality.is_some_and(|first| first != quality) {
                return reaps;
            }
            let Some(gets) = offer.get("TakerGets").and_then(Leg::parse) else {
                continue;
            };
            let funds = owner_funds_leg(ctx, &owner, &gets);
            if funds.is_zero() {
                continue;
            }
            first_quality.get_or_insert(quality);
            available = available.saturating_add(leg_min(&gets, &funds).drops.max(0) as u64);
            if available >= demand.drops.max(0) as u64 {
                return reaps;
            }
        }
    }
    reaps
}

/// Cross a chain of order books (a single resolved path), forwarding each
/// book's output as the next book's input budget. `boundaries[0]` is the
/// source asset (carrying the `SendMax`/budget magnitude on entry), each
/// interior boundary is an intermediate currency, and `boundaries[last]` is the
/// delivered asset (the `Amount`). Returns `(delivered_out, spent_in)` in the
/// last/first boundary assets respectively.
///
/// This is the input-limited forward flow of rippled's `StrandFlow`: the source
/// pushes its budget through hop 0, the realised output funds hop 1, and so on.
/// It is byte-exact for partial payments whose `SendMax` is the binding limit
/// (the validated repro shape). Each interior hop must consume the entirety of
/// the previous hop's delivery — otherwise the intermediate issuer's
/// obligations would not balance — which is enforced by returning
/// `TecPathPartial` (discarding the sandbox) when a downstream book cannot
/// absorb the upstream delivery.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cross_path_payment(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    boundaries: &[Value],
    target_out: &Value,
    budget_in: &Value,
    limit_quality: bool,
) -> Result<(Value, Value), TransactionResult> {
    let n = boundaries.len();
    if n < 2 {
        return Err(TransactionResult::TecPathDry);
    }

    // First hop budget = SendMax, capped at the source's spendable funds
    // (rippled's accountHolds source-funds limit): an IOU leg by the taker's
    // trust-line balance, an XRP leg by the liquid balance above the owner
    // reserve. owner_funds_leg returns xrpLiquid for XRP. Without the XRP cap a
    // reserve-constrained source spent into its reserve and converted far past
    // what mainnet allows (e.g. 105500025+1 rKAYA1e8: 36k liquid drops, not the
    // 500k SendMax), delivering above DeliverMin where mainnet returns
    // tecPATH_PARTIAL.
    let mut carry = Leg::parse(budget_in).ok_or(TransactionResult::TemBadAmount)?;
    let funds = owner_funds_leg(ctx, taker, &carry);
    let source_funds_limited = leg_ge(&carry, &funds);
    if source_funds_limited {
        carry = funds;
    }
    let final_demand = Leg::parse(target_out).ok_or(TransactionResult::TemBadAmount)?;

    // rippled `StrandFlow` output-limited case: for a pure-AMM multi-hop strand
    // whose required input fits the budget, reverse-price from the demanded
    // output back to the source input and apply the exact (reverse-rounded)
    // per-hop amounts, delivering the full `target_out`. The forward-only flow
    // below cannot: it pushes the whole budget through hop 0 (unbounded interior
    // demand), over-produces the intermediate asset, and fails the interior
    // consume invariant. Returns `Ok(None)` (no mutation) for input-limited or
    // CLOB-bearing strands, which fall through to the forward flow.
    if let Some(res) = reverse_amm_strand(
        ctx,
        taker,
        taker_acct,
        dest,
        boundaries,
        &final_demand,
        &carry,
    )? {
        return Ok(res);
    }

    if n >= 3 && final_demand.is_xrp && !limit_quality {
        let input = Leg::parse(&boundaries[n - 2]).ok_or(TransactionResult::TemBadAmount)?;
        let output = Leg::parse(&boundaries[n - 1]).ok_or(TransactionResult::TemBadAmount)?;
        let reaps = reap_expired_reverse_xrp_book_tail(ctx, taker, &input, &output, &final_demand);
        apply_permanent_reaps(ctx, &reaps);
    }

    // tfLimitQuality: refuse the path when the composed best-offer quality is
    // worse than SendMax/Amount. After 30000010 consumed the 10 CNY CNY/XRP
    // band, 30000012 Payment AE4D4DEA (same conversion) is tecPATH_DRY on
    // mainnet; walking the leftover band over-filled maker r4aqu2zb.
    let mut limit_q = 0u64;
    if limit_quality {
        let send_max_leg = Leg::parse(budget_in).ok_or(TransactionResult::TemBadAmount)?;
        limit_q = get_rate(
            &leg_as_quality_iou(&send_max_leg),
            &leg_as_quality_iou(&final_demand),
        )
        .unwrap_or(0);
        let mut path_q: Option<u64> = None;
        for hop in 0..(n - 1) {
            let in_tmpl = Leg::parse(&boundaries[hop]).ok_or(TransactionResult::TemBadAmount)?;
            let out_tmpl =
                Leg::parse(&boundaries[hop + 1]).ok_or(TransactionResult::TemBadAmount)?;
            let Some(hq) = peek_best_book_quality(ctx.view, &in_tmpl, &out_tmpl) else {
                return Err(TransactionResult::TecPathDry);
            };
            path_q = Some(match path_q {
                None => hq,
                // Round-up (worse) matches Quality.cpp and Dries 30000095
                // 4C4D82CA (tecPATH_DRY, SendMax 248 XRP / 73 USD) while
                // 256D45D9 (0.997 of limit) and 30000096 2DDBE66F still pass.
                Some(prev) => composed_quality(prev, hq),
            });
        }
        if let Some(mut pq) = path_q {
            // Dest Amount is net. Last-hop issuer fee worsens the path
            // (30000095 4C4D82CA tecPATH_DRY at 0.9986 * 1.002; 256D45D9
            // and 30000096 2DDBE66F stay under the looser SendMax/Amount).
            let last_out = Leg::parse(&boundaries[n - 1]).ok_or(TransactionResult::TemBadAmount)?;
            if !last_out.is_xrp && last_out.issuer != *dest {
                let tr = transfer_rate(ctx, &last_out.issuer);
                let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
                if tr > one {
                    pq = composed_quality(pq, pack_rate(&tr));
                }
            }
            if limit_q != 0 && pq > limit_q {
                return Err(TransactionResult::TecPathDry);
            }
        }
    }

    let reverse_mixed =
        reverse_clob_then_amm_amounts(ctx, taker, dest, boundaries, &final_demand, limit_q);
    let terminal_reverse_amm = reverse_mixed.is_some();
    let reverse_amts = reverse_mixed
        .or_else(|| reverse_clob_amounts(ctx, dest, boundaries, &final_demand, limit_q));
    if let Some(ref amts) = reverse_amts {
        if amts.len() == n && leg_ge(&carry, &amts[0]) {
            carry = amts[0].clone();
        }
    }

    let mut delivered = final_demand.clone();
    let mut source_spent = carry.clone();
    for hop in 0..(n - 1) {
        let in_tmpl = Leg::parse(&boundaries[hop]).ok_or(TransactionResult::TemBadAmount)?;
        let out_tmpl = Leg::parse(&boundaries[hop + 1]).ok_or(TransactionResult::TemBadAmount)?;
        let is_first = hop == 0;
        let is_last = hop == n - 2;

        // Carry the realised magnitude into this hop's budget, keeping the
        // boundary's currency/issuer/kind.
        let mut budget = in_tmpl.clone();
        if budget.is_xrp {
            budget.drops = carry.drops;
        } else {
            budget.iou = carry.iou;
        }

        // Demand: the final hop is capped by the requested Amount. Interior hops
        // used to be unbounded, which spent the whole SendMax on CNY and then
        // failed `legs_eq` on the ETH hop (30000051 B6E7A0DC). When a reverse
        // CLOB quote exists, use that intermediate size instead.
        let demand = if is_last {
            let mut d = if let Some(rev) = reverse_amts.as_ref().and_then(|a| a.last()) {
                rev.clone()
            } else {
                let mut d = out_tmpl.clone();
                if d.is_xrp {
                    d.drops = final_demand.drops;
                } else {
                    d.iou = final_demand.iou;
                }
                d
            };
            if let Some(cap) = dest_headroom(ctx, dest, &out_tmpl) {
                if cap.iou <= d.iou {
                    d.iou = cap.iou;
                }
            }
            d
        } else if let Some(d) = reverse_amts.as_ref().and_then(|a| a.get(hop + 1)) {
            d.clone()
        } else {
            unbounded_leg(&out_tmpl)
        };

        // tfLimitQuality: stop the last hop when this page composed with
        // prior hops exceeds SendMax/Amount. A hard single page missed the
        // second JPY/CNY band (30000103 70135FCE). Hop 0 still walks freely.
        let lq_stop = if limit_quality && is_last && limit_q != 0 {
            let mut prefix = 0u64;
            for h in 0..(n - 2) {
                let hi = Leg::parse(&boundaries[h]).ok_or(TransactionResult::TemBadAmount)?;
                let ho = Leg::parse(&boundaries[h + 1]).ok_or(TransactionResult::TemBadAmount)?;
                let Some(hq) = peek_best_book_quality(ctx.view, &hi, &ho) else {
                    continue;
                };
                prefix = if prefix == 0 {
                    hq
                } else {
                    composed_quality(prefix, hq)
                };
            }
            Some((prefix, limit_q))
        } else {
            None
        };
        let source_input_rate = if is_first
            && source_funds_limited
            && !in_tmpl.is_xrp
            && taker != &in_tmpl.issuer
            && dest != &in_tmpl.issuer
        {
            transfer_rate_value(ctx, &in_tmpl.issuer)
        } else {
            1_000_000_000
        };
        let (clob_out, clob_spent) = cross_book_hop(
            ctx,
            taker,
            taker_acct,
            dest,
            &demand,
            &budget,
            /*skip_input_debit=*/ !is_first,
            /*skip_output_credit=*/ !is_last,
            /*single_band=*/ limit_quality && is_last && reverse_amts.is_none(),
            /*allow_amm=*/ true,
            lq_stop,
            // A partial self-payment's terminal BookStep forwards the input
            // limited result back to the source account. Its offer remainder
            // keeps the untrimmed Number subtraction; applying the generic
            // intermediate-hop correction consumes one extra IOU ULP
            // (105500244 DCF6A69A).
            /*preserve_input_limited_iou_ulp=*/
            is_last && taker == dest && ctx.rules.enabled(&feature_id("fixUniversalNumber")),
            /*force_full_input_budget=*/ false,
            source_input_rate,
            /*source_funds_limited=*/ false,
            /*direct_payment_demand=*/ false,
        )?;

        // Residual liquidity from the AMM pool for this pair, on the budget not
        // yet spent on the order book and the demand not yet met. For a
        // pure-AMM hop the book delivers nothing and the swap handles the whole
        // budget. (Strict quality interleaving of coexisting AMM + CLOB is not
        // yet modelled — pure-AMM and pure-CLOB hops are exact.)
        let mut got = clob_out.clone();
        let mut spent = clob_spent.clone();
        let residual_budget = leg_sub(&budget, &clob_spent);
        let residual_demand = leg_sub(&demand, &clob_out);
        if !residual_budget.is_zero() && !residual_demand.is_zero() {
            if let AmmHopResult::Filled(amm_out, amm_spent) = amm_hop(
                ctx,
                taker,
                taker_acct,
                dest,
                &residual_demand,
                &residual_budget,
                /*skip_input_debit=*/ !is_first,
                /*skip_output_credit=*/ !is_last,
                /*create_missing_dest_line=*/ false,
                /*partial=*/ true,
                terminal_reverse_amm && is_last,
                /*target_quality=*/ None,
                /*pool_target_quality=*/ None,
                /*cap_ending_spot_price=*/ false,
                /*waive_output_transfer_fee=*/ false,
            )? {
                got = leg_add(&got, &amm_out);
                spent = leg_add(&spent, &amm_spent);
            }
        }

        // An interior hop must fully consume what the previous hop delivered, or
        // the intermediate currency does not balance across the two books.
        if !is_first && !legs_eq_or_one_ulp(&spent, &carry) {
            return Err(TransactionResult::TecPathPartial);
        }
        if is_first {
            source_spent = spent;
        }
        // rippled charges the intermediate issuer's transfer fee once as the IOU
        // ripples through the interior account between two consecutive books. Net
        // the carry here for the legacy parity-input path. A source-fee-aware
        // BookPayment already carries the gross book output into the next step,
        // whose input settlement applies that boundary fee; dividing here would
        // charge it twice. Final delivery, XRP boundaries, and issuer-party hops
        // carry no fee at this boundary.
        carry = got.clone();
        if !is_last
            && source_input_rate == 1_000_000_000
            && !out_tmpl.is_xrp
            && out_tmpl.issuer != *taker
            && out_tmpl.issuer != *dest
        {
            let rate = transfer_rate(ctx, &out_tmpl.issuer);
            let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
            if rate != one {
                carry.iou = IOUAmount::div_round(&carry.iou, &rate, false).unwrap_or(carry.iou);
            }
        }
        delivered = got;
    }

    Ok((
        delivered.with_amount(&delivered.iou, delivered.drops),
        source_spent.with_amount(&source_spent.iou, source_spent.drops),
    ))
}

/// Explicit payment strand entries. Direct steps carry obligations between
/// adjacent accounts; they cannot be represented by a book boundary alone.
pub(crate) enum ResolvedPathStep {
    Book {
        input: Value,
        output: Value,
    },
    Direct {
        src: AccountId,
        dst: AccountId,
        currency: [u8; 20],
    },
}

struct DirectPaymentStep {
    src: AccountId,
    dst: AccountId,
    currency: [u8; 20],
    src_q_out: u32,
    dst_q_in: u32,
    max_src_to_dst: IOUAmount,
    redeeming: bool,
    src_no_ripple: bool,
    dst_no_ripple: bool,
}

impl DirectPaymentStep {
    fn from_view(
        ctx: &ApplyContext<'_>,
        src: AccountId,
        dst: AccountId,
        currency: [u8; 20],
        prev_redeems: bool,
        prev_q_in: u32,
    ) -> Result<Self, TransactionResult> {
        let key = keylet::trust_line(&src, &dst, &currency);
        let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecPathDry)?;
        let line: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let flags = line.get("Flags").and_then(Value::as_u64).unwrap_or(0);
        let src_low = src.as_bytes() < dst.as_bytes();
        let src_frozen = if src_low { 0x0040_0000 } else { 0x0080_0000 };
        let dst_frozen = if src_low { 0x0080_0000 } else { 0x0040_0000 };
        let src_no_ripple = flags & if src_low { 0x0010_0000 } else { 0x0020_0000 } != 0;
        let dst_no_ripple = flags & if src_low { 0x0020_0000 } else { 0x0010_0000 } != 0;
        if flags & (src_frozen | dst_frozen | 0x0600_0000) != 0 {
            return Err(TransactionResult::TecPathDry);
        }
        let quality = |field: &str| -> u32 {
            line.get(field)
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v != 0)
                .unwrap_or(1_000_000_000)
        };
        let dst_q_in = quality(if src_low {
            "HighQualityIn"
        } else {
            "LowQualityIn"
        });
        let src_quality_out = quality(if src_low {
            "LowQualityOut"
        } else {
            "HighQualityOut"
        });
        let stored = IOUAmount::from_decimal_string(
            line.get("Balance")
                .and_then(|b| b.get("value"))
                .and_then(Value::as_str)
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let src_holds = if src_low { stored } else { stored.negate() };
        let src_flags = ctx
            .view
            .read(&keylet::account(&src))
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|account| account.get("Flags").and_then(Value::as_u64))
            .unwrap_or(0);
        if src_flags & 0x0040_0000 != 0 {
            return Err(TransactionResult::TecPathDry);
        }
        let src_auth = if src_low { 0x0004_0000 } else { 0x0008_0000 };
        if src_flags & 0x0004_0000 != 0 && flags & src_auth == 0 && stored.is_zero() {
            return Err(TransactionResult::TecPathDry);
        }
        let redeeming = !src_holds.is_zero() && !src_holds.is_negative();
        let src_q_out = if redeeming {
            src_quality_out.max(prev_q_in)
        } else if prev_redeems {
            transfer_rate_value(ctx, &src)
        } else {
            1_000_000_000
        };
        let max_src_to_dst = if redeeming {
            src_holds
        } else {
            let limit = IOUAmount::from_decimal_string(
                line.get(if src_low { "HighLimit" } else { "LowLimit" })
                    .and_then(|v| v.get("value"))
                    .and_then(Value::as_str)
                    .ok_or(TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
            IOUAmount::add_round(&limit, &src_holds).map_err(|_| TransactionResult::TefInternal)?
        };
        if max_src_to_dst.is_zero() || max_src_to_dst.is_negative() {
            return Err(TransactionResult::TecPathDry);
        }
        Ok(Self {
            src,
            dst,
            currency,
            src_q_out,
            dst_q_in,
            max_src_to_dst,
            redeeming,
            src_no_ripple,
            dst_no_ripple,
        })
    }

    fn reverse(&self, out: IOUAmount) -> Result<(IOUAmount, IOUAmount), TransactionResult> {
        let requested = out
            .mul_ratio(1_000_000_000, self.dst_q_in, true)
            .map_err(|_| TransactionResult::TefInternal)?;
        if requested > self.max_src_to_dst {
            return Err(TransactionResult::TecPathPartial);
        }
        let input = requested
            .mul_ratio(self.src_q_out, 1_000_000_000, true)
            .map_err(|_| TransactionResult::TefInternal)?;
        Ok((input, requested))
    }

    fn forward(
        &self,
        ctx: &mut ApplyContext<'_>,
        input: IOUAmount,
    ) -> Result<IOUAmount, TransactionResult> {
        let moved = input
            .mul_ratio(1_000_000_000, self.src_q_out, false)
            .map_err(|_| TransactionResult::TefInternal)?
            .min(self.max_src_to_dst);
        if moved.is_zero() {
            return Err(TransactionResult::TecPathDry);
        }
        let output = moved
            .mul_ratio(self.dst_q_in, 1_000_000_000, false)
            .map_err(|_| TransactionResult::TefInternal)?;
        credit_line(
            ctx,
            &self.src,
            &self.dst,
            &self.currency,
            &moved.negate(),
            true,
        )?;
        Ok(output)
    }
}

/// A book, two account ripples, and a terminal book. Reverse pricing fixes
/// each interior demand before the forward pass mutates the view.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cross_book_direct_path(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    steps: &[ResolvedPathStep],
    target: &Value,
    budget: &Value,
) -> Result<(Value, Value), TransactionResult> {
    let [
        ResolvedPathStep::Book {
            input: first_in,
            output: first_out,
        },
        ResolvedPathStep::Direct {
            src: first_src,
            dst: first_dst,
            currency: first_currency,
        },
        ResolvedPathStep::Direct {
            src: second_src,
            dst: second_dst,
            currency: second_currency,
        },
        ResolvedPathStep::Book {
            input: last_in,
            output: last_out,
        },
    ] = steps
    else {
        return Err(TransactionResult::TecPathDry);
    };
    if first_dst != second_src
        || first_currency != second_currency
        || last_out != target
        || first_in != budget
    {
        return Err(TransactionResult::TecPathDry);
    }
    let first_in = Leg::parse(first_in).ok_or(TransactionResult::TemBadAmount)?;
    let first_out = Leg::parse(first_out).ok_or(TransactionResult::TemBadAmount)?;
    let last_in = Leg::parse(last_in).ok_or(TransactionResult::TemBadAmount)?;
    let last_out = Leg::parse(last_out).ok_or(TransactionResult::TemBadAmount)?;
    if !first_in.is_xrp
        || first_out.is_xrp
        || last_in.is_xrp
        || last_out.is_xrp
        || first_out.issuer != *first_src
        || last_in.issuer != *second_dst
        || first_out.currency != *first_currency
        || last_in.currency != *second_currency
    {
        return Err(TransactionResult::TecPathDry);
    }
    let source_funds = owner_funds_leg(ctx, taker, &first_in);
    let source_budget = leg_min(&first_in, &source_funds);

    let first_direct = DirectPaymentStep::from_view(
        ctx,
        *first_src,
        *first_dst,
        *first_currency,
        true,
        1_000_000_000,
    )?;
    let second_direct = DirectPaymentStep::from_view(
        ctx,
        *second_src,
        *second_dst,
        *second_currency,
        first_direct.redeeming,
        first_direct.dst_q_in,
    )?;
    if first_direct.src_no_ripple
        || (first_direct.dst_no_ripple && second_direct.src_no_ripple)
        || second_direct.dst_no_ripple
    {
        return Err(TransactionResult::TecPathDry);
    }
    // A speculative book walk obtains the exact input needed by the terminal
    // CLOB. Its account and ledger changes are discarded before application.
    let checkpoint = ctx.view.checkpoint();
    let acct_before = taker_acct.clone();
    let last_quote = cross_book_hop(
        ctx,
        taker,
        taker_acct,
        dest,
        &last_out,
        &unbounded_leg(&last_in),
        true,
        false,
        false,
        false,
        None,
        false,
        false,
        1_000_000_000,
        false,
        false,
    );
    ctx.view.rollback(checkpoint);
    *taker_acct = acct_before;
    let (quoted_out, required_last_in) = last_quote?;
    if quoted_out.is_zero() {
        return Err(TransactionResult::TecPathDry);
    }
    // The terminal book's reverse quote can be one canonical IOU unit below
    // the amount moved by its preceding DirectStep. Retain that guard unit in
    // the upstream demand; the forward book may spend one unit less.
    if required_last_in.iou.is_zero() {
        return Err(TransactionResult::TecPathDry);
    }
    let guarded_last_in = IOUAmount::from_parts(
        required_last_in.iou.mantissa() + 1,
        required_last_in.iou.exponent(),
        false,
    )
    .map_err(|_| TransactionResult::TefInternal)?;
    let (second_required, _) = second_direct.reverse(guarded_last_in)?;
    let (first_required, _) = first_direct.reverse(second_required)?;
    let mut first_demand = first_out.clone();
    first_demand.iou = first_required;
    let (got_first, spent_first) = cross_book_hop(
        ctx,
        taker,
        taker_acct,
        dest,
        &first_demand,
        &source_budget,
        false,
        true,
        false,
        false,
        None,
        false,
        false,
        1_000_000_000,
        false,
        false,
    )?;
    if got_first.is_zero() {
        return Err(TransactionResult::TecPathDry);
    }
    let first_step = DirectPaymentStep::from_view(
        ctx,
        *first_src,
        *first_dst,
        *first_currency,
        true,
        1_000_000_000,
    )?;
    let first_carry = first_step.forward(ctx, got_first.iou)?;
    let second_step = DirectPaymentStep::from_view(
        ctx,
        *second_src,
        *second_dst,
        *second_currency,
        first_step.redeeming,
        first_step.dst_q_in,
    )?;
    let second_carry = second_step.forward(ctx, first_carry)?;
    let mut last_budget = last_in.clone();
    last_budget.iou = second_carry;
    let source_limited =
        !leg_ge(&got_first, &first_demand) || spent_first.drops >= source_budget.drops;
    let (delivered, spent_last) = if source_limited {
        cross_book_hop(
            ctx,
            taker,
            taker_acct,
            dest,
            &last_out,
            &last_budget,
            true,
            false,
            false,
            false,
            None,
            false,
            false,
            1_000_000_000,
            false,
            false,
        )?
    } else {
        // StrandFlow keeps the reverse-priced terminal book when the source
        // does not limit the strand. Reapply that quote after the direct carry.
        cross_book_hop_direct_tail(
            ctx,
            taker,
            taker_acct,
            dest,
            &last_out,
            &unbounded_leg(&last_in),
        )?
    };
    if delivered.is_zero()
        || leg_gt(&spent_last, &last_budget)
        || !legs_eq_or_one_ulp(&spent_last, &last_budget)
    {
        return Err(TransactionResult::TecPathPartial);
    }
    Ok((
        delivered.with_amount(&delivered.iou, delivered.drops),
        spent_first.with_amount(&spent_first.iou, spent_first.drops),
    ))
}

/// Equality of two like-typed legs (exact drops, exact IOU magnitude).
fn legs_eq(a: &Leg, b: &Leg) -> bool {
    if a.is_xrp {
        a.drops == b.drops
    } else {
        a.iou == b.iou
    }
}

/// Interior hop balance: reverse-priced carry can sit 1 ULP above the
/// forward take (30000095 256D45D9 CNY `…583` vs `…582`). A larger gap is
/// still `tecPATH_PARTIAL` (unbounded hop-0 over-spend).
fn legs_eq_or_one_ulp(a: &Leg, b: &Leg) -> bool {
    if legs_eq(a, b) {
        return true;
    }
    if a.is_xrp {
        return (a.drops - b.drops).unsigned_abs() <= 1;
    }
    a.iou.exponent() == b.iou.exponent() && a.iou.mantissa().abs_diff(b.iou.mantissa()) <= 1
}

/// A `Leg` in `template`'s asset carrying an effectively unbounded magnitude,
/// used as the demand cap for an interior hop (the budget is the real limit).
fn unbounded_leg(template: &Leg) -> Leg {
    let mut out = template.clone();
    if template.is_xrp {
        out.drops = i64::MAX;
    } else {
        // A budget far above any real book size, but well below the STAmount max
        // exponent (+80): the reverse pass prices `budget / rate` in `out_for_in`,
        // and a near-max budget divided by a sub-1 rate overflows `div_round` to
        // ZERO — read as a zero budget, which starved an interior IOU->XRP hop.
        // ~1e46 is orders of magnitude above any real amount (< ~1e20) yet leaves
        // ample headroom for the divide.
        out.iou =
            IOUAmount::from_parts(9_999_999_999_999_999, 30, false).unwrap_or(IOUAmount::ZERO);
    }
    out
}

/// The destination's remaining trust-line headroom toward an IOU's issuer,
/// mirroring rippled `creditLimit` (FlowBase.h): `limit - balance` from the
/// holder's perspective, floored at zero. `None` when no cap applies: a native
/// leg, a destination that is itself the issuer, or a missing line (the swap
/// cannot deliver to it anyway). The limit/balance subtraction rounds like
/// modern rippled STAmount arithmetic (`add_round`, 16-digit half-to-even).
fn dest_headroom(ctx: &ApplyContext<'_>, dest: &AccountId, out_tmpl: &Leg) -> Option<Leg> {
    if out_tmpl.is_xrp || *dest == out_tmpl.issuer {
        return None;
    }
    let tl_key = keylet::trust_line(dest, &out_tmpl.issuer, &out_tmpl.currency);
    let bytes = ctx.view.read(&tl_key)?;
    let tl: Value = serde_json::from_slice(&bytes).ok()?;
    let dest_is_low = dest.as_bytes() < out_tmpl.issuer.as_bytes();
    // Balance is stored from the low account's perspective; the holder's view
    // flips sign when it is the high account.
    let bal_str = tl.get("Balance")?.get("value")?.as_str()?;
    let bal_mag =
        IOUAmount::from_decimal_string(bal_str.trim_start_matches('-')).unwrap_or(IOUAmount::ZERO);
    let stored_bal = if bal_str.starts_with('-') {
        bal_mag.negate()
    } else {
        bal_mag
    };
    let balance = if dest_is_low {
        stored_bal
    } else {
        stored_bal.negate()
    };
    let lim_v = tl.get(if dest_is_low { "LowLimit" } else { "HighLimit" })?;
    let limit = IOUAmount::from_decimal_string(lim_v.get("value")?.as_str().unwrap_or("0"))
        .unwrap_or(IOUAmount::ZERO);
    if limit < balance {
        return Some(Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::ZERO,
            currency: out_tmpl.currency,
            issuer: out_tmpl.issuer,
        });
    }
    Some(Leg {
        is_xrp: false,
        drops: 0,
        iou: IOUAmount::add_round(&limit, &balance.negate()).unwrap_or(IOUAmount::ZERO),
        currency: out_tmpl.currency,
        issuer: out_tmpl.issuer,
    })
}

/// A `Leg`'s magnitude as a `Number` (drops as an integer; IOU as its value).
fn leg_to_number(leg: &Leg) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    if leg.is_xrp {
        Number::from_int(leg.drops)
    } else {
        Number::from_iou(&leg.iou)
    }
}

/// Build a `Leg` carrying `mag`, keeping `template`'s currency/issuer/kind. XRP
/// magnitudes floor to an integer drop count.
fn number_to_leg(mag: &rxrpl_amount::number::Number, template: &Leg) -> Leg {
    let mut out = template.clone();
    if template.is_xrp {
        out.drops = mag.to_xrp_drops() as i64;
    } else {
        out.iou = mag.to_iou();
    }
    out
}

/// `a <= b` for two `Number`s.
fn num_le(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    d.is_zero() || d.negative()
}

/// `a > b` for two `Number`s.
fn num_gt(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    !d.is_zero() && !d.negative()
}

/// The AMM pool's holding of one asset as a `Number`: the pseudo-account's XRP
/// `Balance` for native, or its issuer trust-line balance for an IOU.
fn pool_balance_number(
    ctx: &ApplyContext<'_>,
    pool: &AccountId,
    is_xrp: bool,
    cur: &[u8; 20],
    iss: &AccountId,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    if is_xrp {
        let key = keylet::account(pool);
        let bal = ctx
            .view
            .read(&key)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|a| helpers::get_balance(&a))
            .unwrap_or(0);
        Number::from_int(bal as i64)
    } else {
        crate::amm_helpers::iou_holding_number(ctx.view, pool, iss, cur)
    }
}

/// Swap one hop through an AMM pool for the `(budget_in -> demand_out)` pair,
/// delivering up to `demand_out` while spending up to `budget_in`. Distinguish
/// an unavailable AMM from a selected quote whose funded output rounds to zero;
/// the latter ends this quality-band pass before the resting offer executes.
///
/// Mirrors [`super::payment`]'s `try_amm_conversion` (byte-exact `swapAssetIn` /
/// `swapAssetOut` on the live pool reserves), with the multi-hop skip flags: an
/// interior input is not debited from the taker (the previous pool's out-holding
/// already fell by the same amount), and an interior output is not credited to
/// `dest` (it becomes the next pool's input).
#[allow(clippy::too_many_arguments)]
/// Auto-create `holder`'s trust line to `issuer` for `currency` when an AMM swap
/// delivers an IOU the holder has never held (rippled `rippleCredit` ->
/// `trustCreate`). The receiver bears the reserve, so its side carries the
/// reserve flag and (unless it has lsfDefaultRipple) the NoRipple flag; the owner
/// count is bumped on the in-memory `holder_acct` (committed by the caller). The
/// balance is left at zero here — the caller's `set_iou_holding` writes it with
/// the correct low/high sign.
fn can_fund_new_trust_line(account: &Value, next_reserve: u64) -> bool {
    let owner_count = helpers::get_owner_count(account);
    // XRPL waives the incremental owner reserve for an account's first two
    // trust lines. Beyond that bootstrap allowance, an implicit line must be
    // backed by the account reserve just like an explicitly created line.
    owner_count < 2 || helpers::get_balance(account) >= next_reserve
}

fn create_iou_trust_line(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    holder_acct: &mut Value,
    issuer: &AccountId,
    currency: &[u8; 20],
) -> Result<(), TransactionResult> {
    const LSF_LOW_RESERVE: u64 = 0x0001_0000;
    const LSF_HIGH_RESERVE: u64 = 0x0002_0000;
    const LSF_LOW_NO_RIPPLE: u64 = 0x0010_0000;
    const LSF_HIGH_NO_RIPPLE: u64 = 0x0020_0000;
    const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;
    const NO_ACCOUNT: &str = "rrrrrrrrrrrrrrrrrrrrBZbvji";

    let tl_key = keylet::trust_line(holder, issuer, currency);
    let holder_is_low = holder.as_bytes() < issuer.as_bytes();
    let cur_str = currency_code_str(currency);
    let holder_str = encode_account_id(holder);
    let issuer_str = encode_account_id(issuer);

    let holder_lacks_default_ripple = ctx
        .view
        .read(&keylet::account(holder))
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|a| helpers::get_flags(&a) & LSF_DEFAULT_RIPPLE == 0)
        .unwrap_or(true);

    let mut flags: u64 = if holder_is_low {
        LSF_LOW_RESERVE
    } else {
        LSF_HIGH_RESERVE
    };
    if holder_lacks_default_ripple {
        flags |= if holder_is_low {
            LSF_LOW_NO_RIPPLE
        } else {
            LSF_HIGH_NO_RIPPLE
        };
    }

    let holder_limit =
        serde_json::json!({ "currency": cur_str, "issuer": holder_str, "value": "0" });
    let issuer_limit =
        serde_json::json!({ "currency": cur_str, "issuer": issuer_str, "value": "0" });
    let (low_limit, high_limit) = if holder_is_low {
        (holder_limit, issuer_limit)
    } else {
        (issuer_limit, holder_limit)
    };

    let holder_node = add_to_owner_dir(ctx.view, holder, &tl_key)?;
    let issuer_node = add_to_owner_dir(ctx.view, issuer, &tl_key)?;
    let (low_node, high_node) = if holder_is_low {
        (holder_node, issuer_node)
    } else {
        (issuer_node, holder_node)
    };

    let tl = serde_json::json!({
        "LedgerEntryType": "RippleState",
        "Balance": { "currency": cur_str, "issuer": NO_ACCOUNT, "value": "0" },
        "LowLimit": low_limit,
        "HighLimit": high_limit,
        "LowNode": format!("{low_node:016X}"),
        "HighNode": format!("{high_node:016X}"),
        "Flags": flags,
        // Placeholder so the central PreviousTxnID stamping records this tx on
        // the newly created line (it only touches entries already exposing it).
        "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
        "PreviousTxnLgrSeq": 0u32,
    });
    let bytes = serde_json::to_vec(&tl).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .insert(tl_key, bytes)
        .map_err(|_| TransactionResult::TefInternal)?;

    helpers::adjust_owner_count(holder_acct, 1);

    // The line entered the issuer's owner directory; re-touch its account root so
    // the central PreviousTxnID stamping records it (rippled re-writes it).
    let issuer_key = keylet::account(issuer);
    if let Some(issuer_bytes) = ctx.view.read(&issuer_key) {
        let _ = ctx.view.update(issuer_key, issuer_bytes);
    }
    Ok(())
}

/// A pool-balance `Number` as the `IOUAmount` used for `get_rate`/quality: XRP
/// uses its drops integer as the IOU value, an IOU its own value. Mirrors
/// `flow.rs::quality_iou`.
fn num_quality_iou(n: &rxrpl_amount::number::Number, is_xrp: bool) -> IOUAmount {
    if is_xrp {
        IOUAmount::from_decimal_string(&n.to_xrp_drops().to_string()).unwrap_or(IOUAmount::ZERO)
    } else {
        n.to_iou()
    }
}

fn adjusted_amm_quality_allows(
    input: &rxrpl_amount::number::Number,
    in_xrp: bool,
    output: &rxrpl_amount::number::Number,
    out_xrp: bool,
    target: u64,
    round_even: bool,
) -> bool {
    let input = num_quality_iou(input, in_xrp);
    let output = num_quality_iou(output, out_xrp);
    let realised = if round_even {
        rxrpl_amount::get_rate_round_even(&input, &output)
    } else {
        rxrpl_amount::get_rate(&input, &output)
    }
    .unwrap_or(u64::MAX);
    rxrpl_amount::is_better_quality(realised, target)
        || rxrpl_amount::within_relative_distance(realised, target)
}

enum AmmHopResult {
    Unavailable,
    /// The pool wins the quality comparison, but the funded output does not
    /// reach one unit on the destination's amount grid.
    ZeroOutput,
    Filled(Leg, Leg),
}

#[allow(clippy::too_many_arguments)]
fn amm_hop(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
    skip_input_debit: bool,
    skip_output_credit: bool,
    create_missing_dest_line: bool,
    partial: bool,
    reverse_output_limited: bool,
    target_quality: Option<u64>,
    pool_target_quality: Option<u64>,
    cap_ending_spot_price: bool,
    waive_output_transfer_fee: bool,
) -> Result<AmmHopResult, TransactionResult> {
    let in_xrp = budget_in.is_xrp;
    let out_xrp = demand_out.is_xrp;

    let amm_key = keylet::amm(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );
    let Some(amm_bytes) = ctx.view.read(&amm_key) else {
        return Ok(AmmHopResult::Unavailable);
    };
    let Ok(amm): Result<Value, _> = serde_json::from_slice(&amm_bytes) else {
        return Ok(AmmHopResult::Unavailable);
    };
    let Some(pool_str) = amm.get("Account").and_then(|v| v.as_str()) else {
        return Ok(AmmHopResult::Unavailable);
    };
    let Ok(pool_id) = decode_account_id(pool_str) else {
        return Ok(AmmHopResult::Unavailable);
    };
    let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;

    let pool_in = pool_balance_number(
        ctx,
        &pool_id,
        in_xrp,
        &budget_in.currency,
        &budget_in.issuer,
    );
    let pool_out = pool_balance_number(
        ctx,
        &pool_id,
        out_xrp,
        &demand_out.currency,
        &demand_out.issuer,
    );
    if pool_in.is_zero() || pool_out.is_zero() {
        return Ok(AmmHopResult::Unavailable);
    }

    // An intermediate book output is a gross IOU amount just like a direct
    // source payment. Even when the previous hop already performed the debit,
    // the issuer still burns its transfer fee before the net reaches this pool
    // (105500248 6CE529A8, CLOB -> CLOB -> AMM).
    let input_fee_applies = !in_xrp && budget_in.issuer != *taker && budget_in.issuer != pool_id;
    let pool_budget = if input_fee_applies {
        div_nearest(budget_in, &transfer_rate(ctx, &budget_in.issuer), budget_in)
    } else {
        budget_in.clone()
    };
    let budget_num = leg_to_number(&pool_budget);
    let demand_num = leg_to_number(demand_out);
    if budget_num.is_zero() || budget_num.negative() {
        return Ok(AmmHopResult::Unavailable);
    }

    // Spot-price-quality gate (rippled AMMLiquidity::getOffer, AMMLiquidity.cpp:
    // 184-190): an offer crossing may consume the AMM only when its spot quality
    // STRICTLY beats the taker's limit quality and is not within 1e-7 of it —
    // otherwise deliver nothing so cross_offers rests the full offer. The spot
    // is the raw reserve ratio. Trading fees affect the generated synthetic
    // offer, not this initial eligibility check (`AMMLiquidity::getOffer`).
    // Payments pass None when no competing CLOB quality is available.
    if let Some(cq) = pool_target_quality {
        let pool_in_q = num_quality_iou(&pool_in, in_xrp);
        let pool_out_q = num_quality_iou(&pool_out, out_xrp);
        let spq = if ctx.rules.enabled(&feature_id("fixUniversalNumber")) {
            rxrpl_amount::get_rate_round_even(&pool_in_q, &pool_out_q)
        } else {
            rxrpl_amount::get_rate(&pool_in_q, &pool_out_q)
        }
        .ok();
        let Some(spq) = spq else {
            return Ok(AmmHopResult::Unavailable);
        };
        if !rxrpl_amount::is_better_quality(spq, cq)
            || rxrpl_amount::within_relative_distance(spq, cq)
        {
            return Ok(AmmHopResult::Unavailable);
        }
    }

    // Output-limited (deliver the demand) when the input required fits the
    // budget; otherwise input-limited (spend the whole budget). A non-partial
    // (fill-or-kill) crossing is all-or-nothing: it delivers the FULL demand or
    // nothing (never a partial spend), matching rippled's flow() with
    // partialPayment=false.
    // A single-path AMM competes only down to the current CLOB/taker quality.
    // Size the synthetic offer at that boundary before applying sendmax or
    // deliver limits; otherwise tfSell consumes its whole input even after the
    // AMM curve has become worse than the resting book.
    let quality_cap = if let Some(cq) = pool_target_quality {
        if cap_ending_spot_price {
            crate::amm_helpers::change_spot_price_quality(
                &pool_in, &pool_out, cq, tfee, in_xrp, out_xrp,
            )
        } else {
            crate::amm_helpers::limit_input_quality(&pool_in, &pool_out, cq, tfee, in_xrp, out_xrp)
        }
    } else {
        Some((
            budget_num,
            crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &budget_num, tfee, out_xrp),
        ))
    };
    let Some((quality_budget, quality_output)) = quality_cap else {
        return Ok(AmmHopResult::Unavailable);
    };
    let capped_by_quality = num_le(&quality_budget, &budget_num);
    let capped_budget = if capped_by_quality {
        quality_budget
    } else {
        budget_num
    };
    let out_full = if capped_by_quality {
        quality_output
    } else {
        crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &capped_budget, tfee, out_xrp)
    };
    let (spent_num, deliver_num) = if reverse_output_limited {
        (capped_budget, demand_num)
    } else if !partial {
        match crate::amm_helpers::swap_asset_out(&pool_in, &pool_out, &demand_num, tfee, in_xrp) {
            Some(needed) if num_le(&needed, &capped_budget) => (needed, demand_num),
            _ => return Ok(AmmHopResult::Unavailable),
        }
    } else if out_full.sub(&demand_num).negative() {
        (capped_budget, out_full)
    } else {
        match crate::amm_helpers::swap_asset_out(&pool_in, &pool_out, &demand_num, tfee, in_xrp) {
            Some(needed) if num_le(&needed, &budget_num) => (needed, demand_num),
            _ => (capped_budget, out_full),
        }
    };
    if spent_num.is_zero() {
        return Ok(AmmHopResult::Unavailable);
    }
    if number_to_leg(&deliver_num, demand_out).is_zero() {
        return Ok(AmmHopResult::ZeroOutput);
    }

    // The AMM math prices the NET amount received by the pool. When the input
    // issuer charges a transfer fee, the source spends the corresponding GROSS
    // amount. The strand quality check must include that gross debit; comparing
    // the pool input against a sendMax threshold that was itself grossed admits
    // a path one transfer fee worse than the taker's limit (105500536).
    let pool_spent = number_to_leg(&spent_num, budget_in);
    let source_spent = if input_fee_applies {
        let mut gross = pool_spent.clone();
        gross.iou =
            gross_amm_book_input(&pool_spent.iou, transfer_rate_value(ctx, &budget_in.issuer));
        gross
    } else {
        pool_spent.clone()
    };
    if let Some(cq) = target_quality {
        // StrandFlow::limitOut checks the realised quality after integral-grid
        // conversion and source transfer fees. An adjusted output that rounds
        // worse than the requested quality by more than 1e-7 makes the whole
        // strand dry. The CLOB-band spot cap performs its own quality validation
        // in change_spot_price_quality; this gate is for the tail average-quality
        // cap only.
        if !cap_ending_spot_price
            && capped_by_quality
            && !adjusted_amm_quality_allows(
                &leg_to_number(&source_spent),
                in_xrp,
                &deliver_num,
                out_xrp,
                cq,
                ctx.rules.enabled(&feature_id("fixUniversalNumber")),
            )
        {
            return Ok(AmmHopResult::Unavailable);
        }
    }

    // A missing destination line makes this AMM step dry when accepting the
    // output would create an unfunded owned object. Check before moving either
    // side of the swap: Flow discards this liquidity candidate and may continue
    // with another strand, rather than surfacing a trust-line reserve tec.
    if !out_xrp && !skip_output_credit && create_missing_dest_line {
        let tl_key = keylet::trust_line(dest, &demand_out.issuer, &demand_out.currency);
        if ctx.view.read(&tl_key).is_none() {
            let dest_account = if dest == taker {
                Some(taker_acct.clone())
            } else {
                ctx.view
                    .read(&keylet::account(dest))
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            };
            let can_create = dest_account.as_ref().is_some_and(|account| {
                let next = helpers::get_owner_count(account).saturating_add(1);
                can_fund_new_trust_line(account, ctx.fees.account_reserve(next))
            });
            if !can_create {
                return Ok(AmmHopResult::Unavailable);
            }
        }
    }

    // --- Apply the input: move `spent` from the source into the pool. ---
    if in_xrp {
        let drops = spent_num.to_xrp_drops() as i64;
        if !skip_input_debit {
            let bal = helpers::get_balance(taker_acct) as i64 - drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedPayment);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
        credit_xrp(ctx, &pool_id, drops)?;
    } else {
        pay_in(
            ctx,
            taker,
            taker_acct,
            &pool_id,
            &pool_spent,
            ctx.rules.enabled(&feature_id("fixUniversalNumber")),
            PayInOptions {
                skip_taker_debit: skip_input_debit,
                allow_dust_drain: true,
                gross_override: input_fee_applies.then_some(source_spent.iou),
            },
        )?;
    }

    // --- Apply the output: move the pool's GROSS delivery to the recipient. ---
    // OfferCreate's BookOfferCrossingStep waives the AMM output transfer fee.
    // Payment hops retain their existing fee treatment. The realised delivery
    // must match the amount credited to the recipient in either case.
    let delivered = if out_xrp {
        let drops = deliver_num.to_xrp_drops() as i64;
        credit_xrp(ctx, &pool_id, -drops)?;
        if !skip_output_credit {
            if dest == taker {
                helpers::set_balance(taker_acct, helpers::get_balance(taker_acct) + drops as u64);
            } else {
                credit_xrp(ctx, dest, drops)?;
            }
        }
        number_to_leg(&deliver_num, demand_out)
    } else {
        if !skip_output_credit {
            // rippled's rippleCredit auto-creates the receiver's trust line the
            // first time an AMM swap delivers an IOU it has never held.
            if create_missing_dest_line {
                let tl_key = keylet::trust_line(dest, &demand_out.issuer, &demand_out.currency);
                if ctx.view.read(&tl_key).is_none() {
                    create_iou_trust_line(
                        ctx,
                        dest,
                        taker_acct,
                        &demand_out.issuer,
                        &demand_out.currency,
                    )?;
                }
            }
        }
        let gross = number_to_leg(&deliver_num, demand_out);
        let offer_crossing = waive_output_transfer_fee
            && ctx.tx.get("TransactionType").and_then(Value::as_str) == Some("OfferCreate");
        let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
        pay_out_gross(
            ctx,
            taker,
            taker_acct,
            &pool_id,
            dest,
            &gross,
            number_switchover,
            amm_pool_debit_rounding(skip_output_credit),
            amm_recipient_rounding(offer_crossing, number_switchover),
            skip_output_credit,
            TransferFeeRounding::Nearest,
            waive_output_transfer_fee,
        )?
    };

    Ok(AmmHopResult::Filled(delivered, source_spent))
}

/// Per-hop AMM pool snapshot for the reverse-then-forward strand flow.
struct AmmHop {
    pool_id: AccountId,
    in_xrp: bool,
    out_xrp: bool,
    in_cur: [u8; 20],
    in_iss: AccountId,
    out_cur: [u8; 20],
    out_iss: AccountId,
    tfee: u16,
    pool_in: rxrpl_amount::number::Number,
    pool_out: rxrpl_amount::number::Number,
    best_book_quality: Option<u64>,
}

/// Whether the order book identified by `inverse_book` holds at least one
/// resting offer directory page — i.e. the hop carries CLOB liquidity and is not
/// a pure-AMM hop. (succ from quality 0 returns the best-price page; a key that
/// shares the 24-byte book prefix means a page exists.)
fn book_has_resting_offer(ctx: &ApplyContext<'_>, inverse_book: &Hash256) -> bool {
    let probe = book_dir_with_quality(inverse_book, 0);
    match ctx.view.succ(&probe) {
        Some(dir_key) => dir_key.as_bytes()[0..24] == inverse_book.as_bytes()[0..24],
        None => false,
    }
}

/// Move a single AMM hop's exact reverse-computed `(in, out)`: the input flows
/// from the source (or, for an interior hop with `skip_input_debit`, simply
/// lands in this pool — the previous pool already shed the shared intermediate
/// asset) into the pool; the output flows from the pool to `dest` (or, for an
/// interior hop with `skip_output_credit`, stays as the next pool's input).
/// Mirrors the application half of [`amm_hop`] with caller-supplied amounts.
#[allow(clippy::too_many_arguments)]
fn apply_amm_move(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    hop: &AmmHop,
    in_num: &rxrpl_amount::number::Number,
    out_num: &rxrpl_amount::number::Number,
    skip_input_debit: bool,
    skip_output_credit: bool,
    waive_output_transfer_fee: bool,
) -> Result<rxrpl_amount::number::Number, TransactionResult> {
    // Input: source / previous-pool -> this pool.
    if hop.in_xrp {
        let drops = in_num.to_xrp_drops() as i64;
        if !skip_input_debit {
            let bal = helpers::get_balance(taker_acct) as i64 - drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedPayment);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
        credit_xrp(ctx, &hop.pool_id, drops)?;
    } else {
        let amount = number_to_leg(
            in_num,
            &Leg {
                is_xrp: false,
                currency: hop.in_cur,
                issuer: hop.in_iss,
                iou: IOUAmount::ZERO,
                drops: 0,
            },
        );
        pay_in(
            ctx,
            taker,
            taker_acct,
            &hop.pool_id,
            &amount,
            ctx.rules.enabled(&feature_id("fixUniversalNumber")),
            PayInOptions {
                skip_taker_debit: skip_input_debit,
                allow_dust_drain: true,
                gross_override: None,
            },
        )?;
    }
    // Output: this pool -> dest / next pool.
    if hop.out_xrp {
        let drops = out_num.to_xrp_drops() as i64;
        credit_xrp(ctx, &hop.pool_id, -drops)?;
        if !skip_output_credit {
            if dest == taker {
                helpers::set_balance(taker_acct, helpers::get_balance(taker_acct) + drops as u64);
            } else {
                credit_xrp(ctx, dest, drops)?;
            }
        }
        Ok(*out_num)
    } else {
        let amount = number_to_leg(
            out_num,
            &Leg {
                is_xrp: false,
                currency: hop.out_cur,
                issuer: hop.out_iss,
                iou: IOUAmount::ZERO,
                drops: 0,
            },
        );
        if waive_output_transfer_fee && !skip_output_credit {
            if dest == taker
                && amount.issuer != *dest
                && ctx
                    .view
                    .read(&keylet::trust_line(dest, &amount.issuer, &amount.currency))
                    .is_none()
            {
                let next = helpers::get_owner_count(taker_acct).saturating_add(1);
                if !can_fund_new_trust_line(taker_acct, ctx.fees.account_reserve(next)) {
                    return Err(TransactionResult::TecPathDry);
                }
                create_iou_trust_line(ctx, dest, taker_acct, &amount.issuer, &amount.currency)?;
            }
            // BookOfferCrossingStep uses parity for an AMM's output transfer
            // rate. Move the same gross amount off the pool line and onto the
            // taker's line; unlike Payment flow, no issuer fee is burned.
            if amount.issuer != hop.pool_id {
                credit_line_with_rounding(
                    ctx,
                    &hop.pool_id,
                    &amount.issuer,
                    &amount.currency,
                    &amount.iou.negate(),
                    BalanceRounding::Nearest,
                )?;
            }
            if amount.issuer != *dest {
                credit_line_with_rounding(
                    ctx,
                    dest,
                    &amount.issuer,
                    &amount.currency,
                    &amount.iou,
                    BalanceRounding::HalfUpward,
                )?;
            }
            return Ok(*out_num);
        }
        let terminal_transfer_fee = !skip_output_credit
            && hop.out_iss != hop.pool_id
            && hop.out_iss != *dest
            && transfer_rate_value(ctx, &hop.out_iss) > 1_000_000_000;
        let owner_rounding = if skip_input_debit && terminal_transfer_fee {
            // A fee-charged terminal path hop debits the pool at a negative
            // half-ULP tie toward zero (105500249 / TX 126). Without the fee,
            // nearest-even remains exact (105500789 / TX 54).
            BalanceRounding::HalfUpward
        } else {
            amm_pool_debit_rounding(skip_output_credit)
        };
        let net = pay_out_gross(
            ctx,
            taker,
            taker_acct,
            &hop.pool_id,
            dest,
            &amount,
            ctx.rules.enabled(&feature_id("fixUniversalNumber")),
            owner_rounding,
            BalanceRounding::HalfUpward,
            skip_output_credit,
            TransferFeeRounding::Nearest,
            false,
        )?;
        Ok(leg_to_number(&net))
    }
}

/// rippled `StrandFlow` output-limited case for a pure-AMM strand: reverse-price
/// the chain from the demanded output back to the required source input
/// (`swapAssetOut`, round IN up — rippled `DirectStep`/`BookStep` reverse pass),
/// and — when that input fits `budget` — apply the exact per-hop amounts so the
/// full `target_out` is delivered (`tesSUCCESS`). The intermediate amounts thread
/// exactly (each hop's input equals the previous hop's output), reproducing
/// rippled's reverse-rounded intermediates rather than the forward flow's
/// (which round the other way and would over-produce, then strand `TecPathPartial`).
///
/// Returns `Ok(Some((delivered, spent)))` after mutating the ledger, or
/// `Ok(None)` (no mutation) when any hop is not pure-AMM (a resting CLOB offer is
/// present or no AMM pool exists), the pool cannot meet the demand, or the
/// required input exceeds `budget` (input-limited). The caller's forward-only
/// flow then handles those cases.
fn reverse_clob_amounts(
    ctx: &mut ApplyContext<'_>,
    dest: &AccountId,
    boundaries: &[Value],
    target_out: &Leg,
    limit_q: u64,
) -> Option<Vec<Leg>> {
    let n = boundaries.len();
    if n < 2 {
        return None;
    }
    // Cap dest demand at last-hop book size so a PartialPayment Amount larger
    // than resting liquidity does not reverse-price a hop-0 take that then
    // fails `legs_eq` (30000095 256D45D9: 601 USD Amount vs 9.84 USD offer).
    let mut last_in = Leg::parse(&boundaries[n - 2])?;
    let mut last_out = Leg::parse(&boundaries[n - 1])?;
    for hop in (0..n - 1).rev() {
        let hi = Leg::parse(&boundaries[hop])?;
        let ho = Leg::parse(&boundaries[hop + 1])?;
        if peek_best_book_quality(ctx.view, &hi, &ho).is_some() {
            last_in = hi;
            last_out = ho;
            break;
        }
    }
    let mut hop0_q = 0u64;
    for hop in 0..n - 1 {
        let hi = Leg::parse(&boundaries[hop])?;
        let ho = Leg::parse(&boundaries[hop + 1])?;
        if let Some(q) = peek_best_book_quality(ctx.view, &hi, &ho) {
            hop0_q = q;
            break;
        }
    }
    let mut target = target_out.clone();
    if let Some(mut avail) = peek_best_book_gets(ctx, &last_in, &last_out) {
        // TakerGets is gross; dest Amount is net. Capping at Gets then
        // re-grossing the last hop double-counts the issuer fee (30000095
        // bitstamp TransferRate 1.002: 9.84 Gets -> 9.82 net).
        if !avail.is_xrp && last_out.issuer != *dest {
            let tr = transfer_rate(ctx, &last_out.issuer);
            let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
            if tr > one {
                avail.iou = IOUAmount::div_round(&avail.iou, &tr, false).unwrap_or(avail.iou);
            }
        }
        if !leg_ge(&avail, &target) {
            let mut skip = 1usize;
            while !leg_ge(&avail, &target) && skip < 8 {
                if limit_q != 0 {
                    if let Some(pq) = peek_book_quality_skip(ctx.view, &last_in, &last_out, skip) {
                        let cq = if hop0_q == 0 {
                            pq
                        } else {
                            composed_quality(hop0_q, pq)
                        };
                        if cq > limit_q {
                            break;
                        }
                    }
                }
                let Some(off) = peek_funded_offer_skip(ctx, &last_in, &last_out, skip) else {
                    break;
                };
                let Some(g) = Leg::parse(&off["TakerGets"]) else {
                    break;
                };
                avail = leg_add(&avail, &g);
                skip += 1;
            }
            if !leg_ge(&avail, &target) {
                target = avail;
            }
        }
    }
    let mut amts = vec![target];
    let mut last_clob = true;
    for hop in (0..n - 1).rev() {
        let in_tmpl = Leg::parse(&boundaries[hop])?;
        let out_tmpl = Leg::parse(&boundaries[hop + 1])?;
        let Some(q) = peek_best_book_quality(ctx.view, &in_tmpl, &out_tmpl) else {
            // A CLOB reverse quote is only valid when every amount is priced in
            // the asset of its boundary. Copying the downstream amount across
            // a missing book preserves its magnitude but not its issue (for
            // example XRP becomes a zero-valued IOU), which can incorrectly
            // replace the source budget. Let the forward flow price AMM-only
            // and mixed-liquidity strands instead.
            return None;
        };
        let rate = rxrpl_amount::from_rate(q).ok()?;
        if rate.is_zero() {
            return None;
        }
        let is_last_clob = last_clob;
        last_clob = false;
        let mut out_need = amts[0].clone();
        if is_last_clob && !out_tmpl.is_xrp && out_tmpl.issuer != *dest {
            let tr = transfer_rate(ctx, &out_tmpl.issuer);
            let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
            if tr > one {
                out_need.iou = grossed(&out_need.iou, &tr);
                // Exact 10 USD Gets (30000096): gross(net) is 1e15 * 10^-14.
                // +1 ULP oversizes hop 0 by ~9 CNY ULPs. Leftover 9.84 (95)
                // grosses to a different mantissa; keep the bump.
                let recovered_ten = out_need.iou.mantissa() == 1_000_000_000_000_000
                    && out_need.iou.exponent() == -14;
                if !recovered_ten {
                    if let Ok(bumped) = IOUAmount::from_parts(
                        out_need.iou.mantissa() + 1,
                        out_need.iou.exponent(),
                        false,
                    ) {
                        out_need.iou = bumped;
                    }
                }
            }
        }
        let mut in_need = in_for_out(&out_need, &rate, &in_tmpl);
        // Reverse +1 ULP on last-hop Gets can price hop-0 CNY 1 ULP above
        // TakerPays (30000099 0C0344E7). Cap at Pays so the last hop can
        // spend the carry (else tecPATH_PARTIAL, payment deferred).
        if is_last_clob {
            if let Some(tip_gets) = peek_best_book_gets(ctx, &in_tmpl, &out_tmpl) {
                if leg_ge(&tip_gets, &out_need) {
                    if let Some(pays) = peek_best_book_pays(ctx, &in_tmpl, &out_tmpl) {
                        if !leg_ge(&pays, &in_need) {
                            in_need = pays;
                        }
                    }
                } else {
                    let mut acc = peek_best_book_pays(ctx, &in_tmpl, &out_tmpl)
                        .unwrap_or_else(|| in_for_out(&tip_gets, &rate, &in_tmpl));
                    let mut got = tip_gets.clone();
                    let mut skip = 1usize;
                    while !leg_ge(&got, &out_need) && skip < 8 {
                        if limit_q != 0 {
                            if let Some(pq) =
                                peek_book_quality_skip(ctx.view, &in_tmpl, &out_tmpl, skip)
                            {
                                let cq = if hop0_q == 0 {
                                    pq
                                } else {
                                    composed_quality(hop0_q, pq)
                                };
                                if cq > limit_q {
                                    break;
                                }
                            }
                        }
                        let Some(off) = peek_funded_offer_skip(ctx, &in_tmpl, &out_tmpl, skip)
                        else {
                            break;
                        };
                        let Some(p) = Leg::parse(&off["TakerPays"]) else {
                            break;
                        };
                        let Some(g) = Leg::parse(&off["TakerGets"]) else {
                            break;
                        };
                        acc = leg_add(&acc, &p);
                        got = leg_add(&got, &g);
                        skip += 1;
                    }
                    in_need = acc;
                    if !in_need.is_xrp {
                        if let Ok(bumped) = IOUAmount::from_parts(
                            in_need.iou.mantissa() + 1,
                            in_need.iou.exponent(),
                            false,
                        ) {
                            in_need.iou = bumped;
                        }
                    }
                }
            }
        }
        // Hop 0 may span several XLM/XRP qualities (30000103 70135FCE:
        // 1446 XLM tip vs ~2258 needed). Price each page; remainder at
        // the next band's offer amounts so reverse XRP is not short.
        if hop == 0 {
            if let Some(tip_gets) = peek_best_book_gets(ctx, &in_tmpl, &out_tmpl) {
                if !leg_ge(&tip_gets, &out_need) {
                    let mut acc = peek_best_book_pays(ctx, &in_tmpl, &out_tmpl)
                        .unwrap_or_else(|| in_for_out(&tip_gets, &rate, &in_tmpl));
                    let mut got = tip_gets.clone();
                    let mut skip = 1usize;
                    while !leg_ge(&got, &out_need) && skip < 8 {
                        let Some(off) = peek_funded_offer_skip(ctx, &in_tmpl, &out_tmpl, skip)
                        else {
                            break;
                        };
                        let Some(g) = Leg::parse(&off["TakerGets"]) else {
                            break;
                        };
                        let Some(p) = Leg::parse(&off["TakerPays"]) else {
                            break;
                        };
                        let rem = leg_sub(&out_need, &got);
                        if leg_ge(&g, &rem) {
                            if let Some(q) =
                                peek_book_quality_skip(ctx.view, &in_tmpl, &out_tmpl, skip)
                            {
                                if let Ok(r2) = rxrpl_amount::from_rate(q) {
                                    if !r2.is_zero() {
                                        acc = leg_add(&acc, &in_for_out_offer(&p, &rem, &g, &r2));
                                    }
                                }
                            }
                            break;
                        }
                        acc = leg_add(&acc, &p);
                        got = leg_add(&got, &g);
                        skip += 1;
                    }
                    in_need = acc;
                }
            }
        }
        if in_need.is_zero() {
            return None;
        }
        amts.insert(0, in_need);
    }
    (amts.len() == n).then_some(amts)
}

/// Reverse-price a mixed strand whose final hop is AMM-only and whose prefix
/// is carried by order books. The AMM input is computed first, then grossed by
/// the intermediate issuer's transfer rate before the CLOB prefix is priced.
/// This prevents the forward pass from consuming the whole SendMax in the
/// prefix and stranding an interior remainder once the AMM reaches Amount.
fn reverse_clob_then_amm_amounts(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    dest: &AccountId,
    boundaries: &[Value],
    target_out: &Leg,
    limit_q: u64,
) -> Option<Vec<Leg>> {
    let n = boundaries.len();
    if n < 3 {
        return None;
    }
    let amm_hop = n - 2;

    // Keep this planner deliberately narrow: every prefix hop must have a
    // resting book and the terminal hop must be AMM-only. Other mixtures keep
    // using the existing forward liquidity interleaving.
    for h in 0..amm_hop {
        let input = Leg::parse(&boundaries[h])?;
        let output = Leg::parse(&boundaries[h + 1])?;
        peek_best_book_quality(ctx.view, &input, &output)?;
    }
    let amm_in = Leg::parse(&boundaries[amm_hop])?;
    let amm_out = Leg::parse(&boundaries[amm_hop + 1])?;
    if peek_best_book_quality(ctx.view, &amm_in, &amm_out).is_some() {
        return None;
    }

    let amm_key = keylet::amm(
        &amm_in.currency,
        &amm_in.issuer,
        &amm_out.currency,
        &amm_out.issuer,
    );
    let amm: Value = serde_json::from_slice(&ctx.view.read(&amm_key)?).ok()?;
    let pool_id = decode_account_id(amm.get("Account")?.as_str()?).ok()?;
    let tfee = amm.get("TradingFee").and_then(Value::as_u64).unwrap_or(0) as u16;
    let pool_in = pool_balance_number(
        ctx,
        &pool_id,
        amm_in.is_xrp,
        &amm_in.currency,
        &amm_in.issuer,
    );
    let pool_out = pool_balance_number(
        ctx,
        &pool_id,
        amm_out.is_xrp,
        &amm_out.currency,
        &amm_out.issuer,
    );
    if pool_in.is_zero() || pool_out.is_zero() {
        return None;
    }

    let mut pool_target = target_out.clone();
    if !amm_out.is_xrp && amm_out.issuer != pool_id && amm_out.issuer != *dest {
        pool_target.iou = grossed(&pool_target.iou, &transfer_rate(ctx, &amm_out.issuer));
    }
    let required_in = crate::amm_helpers::swap_asset_out(
        &pool_in,
        &pool_out,
        &leg_to_number(&pool_target),
        tfee,
        amm_in.is_xrp,
    )?;
    if required_in.is_zero() || required_in.negative() {
        return None;
    }
    let mut prefix_target = number_to_leg(&required_in, &amm_in);
    if !amm_in.is_xrp && amm_in.issuer != *taker && amm_in.issuer != *dest {
        prefix_target.iou = grossed(&prefix_target.iou, &transfer_rate(ctx, &amm_in.issuer));
    }

    // Treat the intermediate issuer as the prefix destination: its transfer
    // fee was already applied above and must not be charged a second time by
    // the CLOB reverse-pricer's terminal-delivery logic.
    let mut amounts = reverse_clob_amounts(
        ctx,
        &amm_in.issuer,
        &boundaries[..=amm_hop],
        &prefix_target,
        limit_q,
    )?;
    amounts.push(target_out.clone());
    (amounts.len() == n).then_some(amounts)
}

fn reverse_amm_strand(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    boundaries: &[Value],
    target_out: &Leg,
    budget: &Leg,
) -> Result<Option<(Value, Value)>, TransactionResult> {
    use rxrpl_amount::number::Number;
    let n = boundaries.len();
    // Only multi-hop strands need the reverse pass: a single AMM hop already
    // delivers byte-exactly through the forward `amm_hop` (output-limited per
    // hop). Leave single-hop and degenerate chains to the forward flow.
    if n < 3 {
        return Ok(None);
    }

    // Gather an AMM snapshot of every hop. A coexisting CLOB does not by itself
    // disqualify the reverse AMM strand: rippled compares the synthetic AMM
    // offer with the book tip and may route the whole hop through the pool.
    let mut hops: Vec<AmmHop> = Vec::with_capacity(n - 1);
    for h in 0..(n - 1) {
        let in_leg = Leg::parse(&boundaries[h]).ok_or(TransactionResult::TemBadAmount)?;
        let out_leg = Leg::parse(&boundaries[h + 1]).ok_or(TransactionResult::TemBadAmount)?;
        let best_book_quality = peek_best_book_quality(ctx.view, &in_leg, &out_leg);
        let amm_key = keylet::amm(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        let Some(amm_bytes) = ctx.view.read(&amm_key) else {
            return Ok(None);
        };
        let Ok(amm): Result<Value, _> = serde_json::from_slice(&amm_bytes) else {
            return Ok(None);
        };
        let Some(pool_str) = amm.get("Account").and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        let Ok(pool_id) = decode_account_id(pool_str) else {
            return Ok(None);
        };
        let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        let pool_in = pool_balance_number(
            ctx,
            &pool_id,
            in_leg.is_xrp,
            &in_leg.currency,
            &in_leg.issuer,
        );
        let pool_out = pool_balance_number(
            ctx,
            &pool_id,
            out_leg.is_xrp,
            &out_leg.currency,
            &out_leg.issuer,
        );
        if pool_in.is_zero() || pool_out.is_zero() {
            return Ok(None);
        }
        hops.push(AmmHop {
            pool_id,
            in_xrp: in_leg.is_xrp,
            out_xrp: out_leg.is_xrp,
            in_cur: in_leg.currency,
            in_iss: in_leg.issuer,
            out_cur: out_leg.currency,
            out_iss: out_leg.issuer,
            tfee,
            pool_in,
            pool_out,
            best_book_quality,
        });
    }

    // Reverse pass: thread the demanded NET output back to the source input.
    // The last pool must provide the gross amount when its issuer charges a
    // transfer fee; likewise the source pays gross for the first pool's net
    // input when neither side is the issuer.
    let mut ins: Vec<Number> = vec![Number::ZERO; n - 1];
    let mut outs: Vec<Number> = vec![Number::ZERO; n - 1];
    let last = &hops[n - 2];
    let last_fee_applies = !last.out_xrp && last.out_iss != last.pool_id && last.out_iss != *dest;
    let mut pool_target = target_out.clone();
    if last_fee_applies {
        pool_target.iou = grossed(&pool_target.iou, &transfer_rate(ctx, &last.out_iss));
    }
    let mut out_num = leg_to_number(&pool_target);
    for h in (0..(n - 1)).rev() {
        let hop = &hops[h];
        let in_num = match crate::amm_helpers::swap_asset_out(
            &hop.pool_in,
            &hop.pool_out,
            &out_num,
            hop.tfee,
            hop.in_xrp,
        ) {
            Some(v) if !v.is_zero() && !v.negative() => v,
            // Pool can't deliver the demand -> not output-limited; the forward
            // flow handles the (partial) input-limited case.
            _ => return Ok(None),
        };
        outs[h] = out_num;
        ins[h] = in_num;
        // `in_num` is what this pool must receive. The preceding pool must
        // emit the gross intermediate amount when neither pool is the issuer.
        out_num = if h > 0
            && !hop.in_xrp
            && hop.in_iss != hops[h - 1].pool_id
            && hop.in_iss != hop.pool_id
        {
            Number::from_iou(&grossed(&in_num.to_iou(), &transfer_rate(ctx, &hop.in_iss)))
        } else {
            in_num
        };
    }

    let first = &hops[0];
    let first_fee_applies =
        !first.in_xrp && first.in_iss != *taker && first.in_iss != first.pool_id;
    let mut source_spent = number_to_leg(&ins[0], budget);
    if first_fee_applies {
        source_spent.iou = grossed(&source_spent.iou, &transfer_rate(ctx, &first.in_iss));
    }

    // If the requested output does not fit the gross SendMax, run the pure-AMM
    // strand forward from the whole input budget. The first pool receives the
    // net-of-transfer-fee input and the destination receives the last pool's
    // gross output net of its transfer fee.
    if !leg_ge(budget, &source_spent) {
        let mut pool_input = budget.clone();
        if first_fee_applies {
            let rate = transfer_rate(ctx, &first.in_iss);
            pool_input.iou =
                IOUAmount::div_round(&pool_input.iou, &rate, false).unwrap_or(pool_input.iou);
        }
        let mut current = leg_to_number(&pool_input);
        for h in 0..(n - 1) {
            let hop = &hops[h];
            ins[h] = current;
            let out = crate::amm_helpers::swap_asset_in(
                &hop.pool_in,
                &hop.pool_out,
                &current,
                hop.tfee,
                hop.out_xrp,
            );
            if out.is_zero() || out.negative() {
                return Ok(None);
            }
            outs[h] = out;
            // The next pool receives the intermediate output net of the
            // issuer's transfer fee (105500248 6CE529A8).
            current = if h + 1 < n - 1
                && !hop.out_xrp
                && hop.out_iss != hop.pool_id
                && hop.out_iss != hops[h + 1].pool_id
            {
                Number::from_iou(
                    &IOUAmount::div_round(&out.to_iou(), &transfer_rate(ctx, &hop.out_iss), false)
                        .unwrap_or(out.to_iou()),
                )
            } else {
                out
            };
        }
        source_spent = number_to_leg(&ins[0], budget);
        if first_fee_applies {
            source_spent.iou = grossed(&source_spent.iou, &transfer_rate(ctx, &first.in_iss));
        }
    }

    // A coexisting CLOB only wins when its best page is at least as good as the
    // realised AMM quote for that hop. Otherwise the reverse AMM strand remains
    // the exact route and no Offer node is touched.
    for (h, hop) in hops.iter().enumerate() {
        if let Some(book_quality) = hop.best_book_quality {
            let Some(amm_quality) = rxrpl_amount::get_rate(
                &num_quality_iou(&ins[h], hop.in_xrp),
                &num_quality_iou(&outs[h], hop.out_xrp),
            )
            .ok() else {
                return Ok(None);
            };
            if !rxrpl_amount::is_better_quality(amm_quality, book_quality)
                || rxrpl_amount::within_relative_distance(amm_quality, book_quality)
            {
                return Ok(None);
            }
        }
    }

    // Apply each hop with its exact reverse-computed (in, out): the first hop
    // debits the source, the last credits `dest`, interior hops move the shared
    // intermediate asset between adjacent pools.
    for h in 0..(n - 1) {
        let hop = &hops[h];
        apply_amm_move(
            ctx,
            taker,
            taker_acct,
            dest,
            hop,
            &ins[h],
            &outs[h],
            /*skip_input_debit=*/ h != 0,
            /*skip_output_credit=*/ h != n - 2,
            /*waive_output_transfer_fee=*/ false,
        )?;
    }

    let mut delivered_leg = number_to_leg(&outs[n - 2], target_out);
    if last_fee_applies {
        delivered_leg.iou = IOUAmount::div_round(
            &delivered_leg.iou,
            &transfer_rate(ctx, &last.out_iss),
            false,
        )
        .unwrap_or(delivered_leg.iou);
    }
    Ok(Some((
        delivered_leg.with_amount(&delivered_leg.iou, delivered_leg.drops),
        source_spent.with_amount(&source_spent.iou, source_spent.drops),
    )))
}

// ===========================================================================
// Multi-path Flow (rippled `Flow` + `AMMLiquidity` fib-chunked AMM consumption)
// ===========================================================================
//
// When two or more strands are live, rippled consumes a shared AMM pool in
// *fibonacci-sized synthetic offers* (one per multi-pass iteration) clamped to
// the competing CLOB quality, instead of one full output-limited swap. This is
// the difference between over-delivering (one swap) and the byte-exact chunked
// total rippled produces. The fib sizing + per-chunk quality clamp live in
// `super::flow`; the actual pool/holding mutation reuses `apply_amm_move` and
// the CLOB walk reuses `cross_book_hop` verbatim.

use super::flow::{AmmContext, AmmLiquidity};
use rxrpl_amount::number::Number;

/// rippled `Flow` loop bounds (StrandFlow.h).
const FLOW_MAX_TRIES: u32 = 1000;
const FLOW_MAX_OFFERS: u32 = 1500;

/// One hop of a resolved strand: the in→out boundary pair plus the AMM pool
/// (if any) and whether the book carries resting CLOB offers.
#[derive(Clone)]
pub(crate) struct FlowHop {
    in_leg: Leg,
    out_leg: Leg,
    inverse_book: Hash256,
    has_clob: bool,
    /// AMM pool pseudo-account + trading fee, when a pool exists for the pair.
    amm_pool: Option<AccountId>,
    amm_tfee: u16,
    /// Frozen initial-pool snapshot for the fib seed (held stable for the whole
    /// payment); the live balances are refetched each pass.
    liquidity: Option<AmmLiquidity>,
}

/// A resolved strand = an ordered boundary chain of hops.
#[derive(Clone)]
pub(crate) struct FlowStrand {
    hops: Vec<FlowHop>,
}

impl FlowStrand {
    pub(crate) fn terminal_has_amm(&self) -> bool {
        self.hops.last().is_some_and(|hop| hop.amm_pool.is_some())
    }

    pub(crate) fn is_two_hop_clob_path(&self) -> bool {
        self.hops.len() == 2 && self.hops.iter().all(|hop| hop.has_clob)
    }
}

/// The realised result of executing one pass of a strand.
struct StrandPass {
    in_amt: Number,
    out_amt: Number,
    offers_used: u32,
    preserve_output_ulp: bool,
    quality_adjusted: bool,
}

struct FlowSourceBalance {
    key: Hash256,
    balance: IOUAmount,
    holder_is_high: bool,
}

fn flow_source_balance(
    ctx: &ApplyContext<'_>,
    taker: &AccountId,
    strands: &[FlowStrand],
) -> Option<FlowSourceBalance> {
    let source = &strands.first()?.hops.first()?.in_leg;
    if source.is_xrp || source.issuer == *taker {
        return None;
    }
    // Reconciliation is only valid when every strand starts in the same issue
    // and that issue is not revisited later in the path.
    for strand in strands {
        let first = &strand.hops.first()?.in_leg;
        if first.is_xrp || first.currency != source.currency || first.issuer != source.issuer {
            return None;
        }
        if strand.hops.iter().any(|hop| {
            !hop.out_leg.is_xrp
                && hop.out_leg.currency == source.currency
                && hop.out_leg.issuer == source.issuer
        }) {
            return None;
        }
    }
    let key = keylet::trust_line(taker, &source.issuer, &source.currency);
    let line: Value = serde_json::from_slice(&ctx.view.read(&key)?).ok()?;
    let balance = IOUAmount::from_decimal_string(line["Balance"]["value"].as_str()?).ok()?;
    Some(FlowSourceBalance {
        key,
        balance,
        holder_is_high: taker.as_bytes() > source.issuer.as_bytes(),
    })
}

fn flow_destination_balance(
    ctx: &ApplyContext<'_>,
    dest: &AccountId,
    strands: &[FlowStrand],
) -> Option<FlowSourceBalance> {
    let output = &strands.first()?.hops.last()?.out_leg;
    if output.is_xrp || dest == &output.issuer {
        return None;
    }
    let key = keylet::trust_line(dest, &output.issuer, &output.currency);
    let line: Value = serde_json::from_slice(&ctx.view.read(&key)?).ok()?;
    let balance = IOUAmount::from_decimal_string(line["Balance"]["value"].as_str()?).ok()?;
    Some(FlowSourceBalance {
        key,
        balance,
        holder_is_high: dest.as_bytes() > output.issuer.as_bytes(),
    })
}

fn aggregate_flow_source_balance_number(
    balance: &IOUAmount,
    spent: &Number,
    holder_is_high: bool,
) -> IOUAmount {
    let balance = Number::from_iou(balance);
    if holder_is_high {
        balance.add(spent)
    } else {
        balance.sub(spent)
    }
    .to_iou()
}

fn aggregate_flow_source_balance_passes(
    balance: &IOUAmount,
    saved_ins: &[Number],
    holder_is_high: bool,
) -> Option<IOUAmount> {
    saved_ins.iter().try_fold(*balance, |balance, spent| {
        let spent = spent.to_iou();
        let delta = if holder_is_high {
            spent
        } else {
            spent.negate()
        };
        IOUAmount::add_round(&balance, &delta).ok()
    })
}

fn aggregate_flow_destination_balance_passes(
    balance: &IOUAmount,
    saved_outs: &[Number],
    holder_is_high: bool,
) -> Option<IOUAmount> {
    saved_outs.iter().try_fold(*balance, |balance, delivered| {
        let delivered = delivered.to_iou();
        let delta = if holder_is_high {
            delivered.negate()
        } else {
            delivered
        };
        IOUAmount::add_round(&balance, &delta).ok()
    })
}

fn flow_holder_funds(balance: &IOUAmount, holder_is_high: bool) -> Number {
    let holding = if holder_is_high {
        balance.negate()
    } else {
        *balance
    };
    if holding.is_negative() {
        Number::ZERO
    } else {
        Number::from_iou(&holding)
    }
}

fn flow_live_source_funds(ctx: &ApplyContext<'_>, source: &FlowSourceBalance) -> Option<Number> {
    let bytes = ctx.view.read(&source.key)?;
    let line: Value = serde_json::from_slice(&bytes).ok()?;
    let balance = IOUAmount::from_decimal_string(line["Balance"]["value"].as_str()?).ok()?;
    Some(flow_holder_funds(&balance, source.holder_is_high))
}

fn flow_source_pass_budget(accounting: Number, live: Option<Number>) -> Number {
    match live {
        Some(funds) if num_le(&funds, &accounting) => funds,
        _ => accounting,
    }
}

fn source_has_one_ulp_overdraft(
    source: &FlowSourceBalance,
    budget: &Leg,
    final_balance: &IOUAmount,
) -> bool {
    if !flow_holder_funds(&source.balance, source.holder_is_high)
        .sub(&leg_to_number(budget))
        .is_zero()
    {
        return false;
    }
    let holder_final = if source.holder_is_high {
        final_balance.negate()
    } else {
        *final_balance
    };
    let one_ulp =
        IOUAmount::from_parts(1, source.balance.exponent(), false).unwrap_or(IOUAmount::ZERO);
    holder_final.is_negative() && holder_final.abs() <= one_ulp
}

fn clear_one_ulp_source_overdraft(
    ctx: &mut ApplyContext<'_>,
    source: &FlowSourceBalance,
    budget: &Leg,
) {
    let current = ctx
        .view
        .read(&source.key)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|line| IOUAmount::from_decimal_string(line["Balance"]["value"].as_str()?).ok());
    if current.is_some_and(|balance| source_has_one_ulp_overdraft(source, budget, &balance)) {
        set_flow_source_balance(ctx, source, &IOUAmount::ZERO);
    }
}

fn flow_input_limit(send_max: &Number, source: Option<&FlowSourceBalance>) -> Number {
    source
        .map(|source| flow_holder_funds(&source.balance, source.holder_is_high))
        .map(|funds| {
            if num_le(&funds, send_max) {
                funds
            } else {
                *send_max
            }
        })
        .unwrap_or(*send_max)
}

fn flow_source_funds_limit_binds(tx_send_max: Option<&Leg>, input_limit: &Number) -> bool {
    tx_send_max.is_some_and(|max| num_gt(&leg_to_number(max), input_limit))
}

fn reconcile_flow_source_balance(
    ctx: &mut ApplyContext<'_>,
    source: &FlowSourceBalance,
    spent: &Number,
) {
    let balance =
        aggregate_flow_source_balance_number(&source.balance, spent, source.holder_is_high);
    set_flow_source_balance(ctx, source, &balance);
}

fn reconcile_flow_source_balance_passes(
    ctx: &mut ApplyContext<'_>,
    source: &FlowSourceBalance,
    saved_ins: &[Number],
) {
    let Some(balance) =
        aggregate_flow_source_balance_passes(&source.balance, saved_ins, source.holder_is_high)
    else {
        return;
    };
    set_flow_source_balance(ctx, source, &balance);
}

fn reconcile_flow_destination_balance_passes(
    ctx: &mut ApplyContext<'_>,
    destination: &FlowSourceBalance,
    saved_outs: &[Number],
) {
    let Some(balance) = aggregate_flow_destination_balance_passes(
        &destination.balance,
        saved_outs,
        destination.holder_is_high,
    ) else {
        return;
    };
    set_flow_source_balance(ctx, destination, &balance);
}

fn set_flow_source_balance(
    ctx: &mut ApplyContext<'_>,
    source: &FlowSourceBalance,
    balance: &IOUAmount,
) {
    let Some(bytes) = ctx.view.read(&source.key) else {
        return;
    };
    let Ok(mut line) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    line["Balance"]["value"] = Value::String(balance.to_decimal_string());
    if let Ok(bytes) = serde_json::to_vec(&line) {
        let _ = ctx.view.update(source.key, bytes);
    }
}

fn preserve_flow_output_ulp(
    delivered: &Number,
    requested: &Number,
    path_requires_preservation: bool,
) -> bool {
    path_requires_preservation || delivered.sub(requested).is_zero()
}

fn flow_book_input_rate(ctx: &ApplyContext<'_>, leg: &Leg, dest: &AccountId) -> Option<IOUAmount> {
    (!leg.is_xrp && leg.issuer != *dest).then(|| transfer_rate(ctx, &leg.issuer))
}

pub(crate) fn flow_gross_input(amount: &Number, rate: Option<IOUAmount>) -> Number {
    if amount.is_zero() {
        return Number::ZERO;
    }
    rate.and_then(|rate| IOUAmount::mul_round(&amount.to_iou(), &rate, true).ok())
        .map(|amount| Number::from_iou(&amount))
        .unwrap_or(*amount)
}

pub(crate) fn flow_net_input(amount: &Number, rate: Option<IOUAmount>) -> Number {
    rate.and_then(|rate| IOUAmount::div_round(&amount.to_iou(), &rate, false).ok())
        .map(|amount| Number::from_iou(&amount))
        .unwrap_or(*amount)
}

/// An input-limited terminal BookStep divides its carried gross input by the
/// issuer rate in Flow's `Number` domain before landing on the IOU grid.
/// STAmount truncation loses one input ULP for both AMM (105502136/TX32)
/// and CLOB (105502606/TX17) terminal hops.
fn flow_net_input_terminal_forward(amount: &Number, rate: IOUAmount) -> Number {
    Number::from_iou(&amount.div(&Number::from_iou(&rate)).to_iou())
}

/// When a self-payment's source funds cap the first interior IOU book, its
/// forward pass spends the entire net input. Repricing the floored output at
/// book quality can leave one source ULP on the resting offer (105502166/TX31).
fn flow_first_book_spends_source_funds(
    source_funds_limited: bool,
    source_capped: bool,
    first_interior_clob: bool,
    self_payment: bool,
    input_rate: Option<IOUAmount>,
    output_is_xrp: bool,
) -> bool {
    source_funds_limited
        && source_capped
        && first_interior_clob
        && self_payment
        && input_rate == Some(parity_rate())
        && !output_is_xrp
}

/// Whether a directory tier contains at least one offer on any page.
fn book_dir_has_entries(ctx: &ApplyContext<'_>, root: &Hash256) -> bool {
    let mut page = 0u64;
    loop {
        let key = if page == 0 {
            *root
        } else {
            keylet::dir_node(root, page)
        };
        let Some(bytes) = ctx.view.read(&key) else {
            return false;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        if dir
            .get("Indexes")
            .and_then(Value::as_array)
            .is_some_and(|indexes| !indexes.is_empty())
        {
            return true;
        }
        page = dir.get("IndexNext").and_then(Value::as_u64).unwrap_or(0);
        if page == 0 {
            return false;
        }
    }
}

/// Offer indexes in a quality directory, including linked overflow pages.
fn book_dir_indexes(ctx: &ApplyContext<'_>, root: &Hash256) -> Vec<Hash256> {
    let mut indexes = Vec::new();
    let mut page = 0u64;
    loop {
        let key = if page == 0 {
            *root
        } else {
            keylet::dir_node(root, page)
        };
        let Some(bytes) = ctx.view.read(&key) else {
            break;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&bytes) else {
            break;
        };
        indexes.extend(
            dir.get("Indexes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str()?.parse::<Hash256>().ok()),
        );
        page = dir.get("IndexNext").and_then(Value::as_u64).unwrap_or(0);
        if page == 0 {
            break;
        }
    }
    indexes
}

/// The best non-empty page (tip) quality of an order book.
fn book_tip_quality(ctx: &ApplyContext<'_>, inverse_book: &Hash256) -> Option<u64> {
    let prefix = &inverse_book.as_bytes()[0..24];
    let mut probe = book_dir_with_quality(inverse_book, 0);
    while let Some(dir_key) = ctx.view.succ(&probe) {
        if &dir_key.as_bytes()[0..24] != prefix {
            return None;
        }
        probe = dir_key;
        if book_dir_has_entries(ctx, &dir_key) {
            return Some(u64::from_be_bytes(
                dir_key.as_bytes()[24..32].try_into().ok()?,
            ));
        }
    }
    None
}

/// Best quality with an offer that this taker can actually cross. Expired and
/// unfunded offers are reaped while probing so an AMM is compared against the
/// next live CLOB tip, not a stale directory that disappears during the book
/// walk.
fn funded_book_tip_quality(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    inverse_book: &Hash256,
) -> Option<u64> {
    let prefix = &inverse_book.as_bytes()[0..24];
    let mut probe = book_dir_with_quality(inverse_book, 0);
    while let Some(dir_key) = ctx.view.succ(&probe) {
        if &dir_key.as_bytes()[0..24] != prefix {
            return None;
        }
        probe = dir_key;
        let quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().ok()?);
        for offer_key in book_dir_indexes(ctx, &dir_key) {
            let Some(offer_bytes) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&offer_bytes) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(Value::as_str) != Some("Offer") {
                continue;
            }
            let Some(owner) = offer
                .get("Account")
                .and_then(Value::as_str)
                .and_then(|value| decode_account_id(value).ok())
            else {
                continue;
            };
            if owner == *taker {
                continue;
            }
            let (Some(offer_in), Some(offer_out)) = (
                offer.get("TakerPays").and_then(Leg::parse),
                offer.get("TakerGets").and_then(Leg::parse),
            ) else {
                continue;
            };
            let expired = offer
                .get("Expiration")
                .and_then(Value::as_u64)
                .is_some_and(|expiration| expiration <= ctx.view.parent_close_time() as u64);
            let owner_funds = owner_funds_leg(ctx, &owner, &offer_out);
            let unfunded = owner_funds.is_zero();
            let tiny_increased_quality = should_reap_tiny_increased_quality(
                &owner,
                &offer_in,
                &offer_out,
                &owner_funds,
                quality,
            );
            if tiny_increased_quality
                && !tiny_offer_is_permanently_reapable(ctx, &owner, &offer_out, &owner_funds)
            {
                continue;
            }
            if expired || unfunded || tiny_increased_quality {
                let cp = ctx.view.checkpoint();
                if reap_offer(ctx, &owner, &offer_key, &dir_key).is_err() {
                    ctx.view.rollback(cp);
                    return None;
                }
                continue;
            }
            return Some(quality);
        }
    }
    None
}

/// Mirrors `OfferStream::shouldRmSmallIncreasedQOffer`. A severely underfunded
/// offer can round its effective `TakerPays` down to the minimum amount (or
/// zero), worsening its quality enough to block the book. rippled permanently
/// removes that offer while probing, even when an AMM ultimately supplies the
/// swap.
fn should_reap_tiny_increased_quality(
    owner: &AccountId,
    offer_in: &Leg,
    offer_out: &Leg,
    owner_funds: &Leg,
    quality: u64,
) -> bool {
    // OfferStream handles zero owner funds in its ordinary unfunded branch
    // before it asks whether a partially funded offer became too small. Keeping
    // the cases separate prevents an out-of-quality zero-funded tip from being
    // reaped by the special tiny-tip probe (105500440).
    if owner_funds.is_zero()
        || offer_out.is_xrp
        || owner == &offer_out.issuer
        || leg_ge(owner_funds, offer_out)
    {
        return false;
    }
    if !offer_in.is_xrp && leg_ge(offer_in, offer_out) {
        return false;
    }
    let Ok(rate) = from_rate(quality) else {
        return false;
    };
    let Ok(effective_amount) = rxrpl_amount::Amount::mul_round_strict(
        &leg_to_amount(owner_funds),
        &rxrpl_amount::Amount::Iou(rate),
        offer_in.is_xrp,
        false,
    ) else {
        return false;
    };
    let effective_in = amount_to_leg(&effective_amount, offer_in);
    if effective_in.is_zero() {
        return true;
    }
    let above_minimum = if effective_in.is_xrp {
        effective_in.drops > 1
    } else {
        effective_in.iou > IOUAmount::MIN_POSITIVE
    };
    if above_minimum {
        return false;
    }
    get_rate(
        &leg_as_quality_iou(&effective_in),
        &leg_as_quality_iou(owner_funds),
    )
    .is_ok_and(|effective_quality| effective_quality > quality)
}

/// Check a deleted offer against the fee-claim view. Flow keeps a tiny-tip
/// removal even when the crossing later returns a claimed failure, but a
/// partial fill or a newly exhausted maker must remain in the child sandbox.
pub(crate) fn is_permanently_tiny_offer(
    ctx: &mut ApplyContext<'_>,
    offer: &Value,
    owner: &AccountId,
    book_dir: &Hash256,
) -> bool {
    let (Some(offer_in), Some(offer_out)) = (
        offer.get("TakerPays").and_then(Leg::parse),
        offer.get("TakerGets").and_then(Leg::parse),
    ) else {
        return false;
    };
    let funds = owner_funds_leg(ctx, owner, &offer_out);
    let quality = u64::from_be_bytes(book_dir.as_bytes()[24..32].try_into().unwrap());
    should_reap_tiny_increased_quality(owner, &offer_in, &offer_out, &funds, quality)
}

/// FlowOfferStream only adds a tiny reduced-quality offer to its permanent
/// removal set when the cancel view reports the same owner funds. If an earlier
/// step in this transaction made the offer tiny, the stream skips it locally
/// but does not add it to the cross-strand permanent-removal side channel.
fn tiny_offer_is_permanently_reapable(
    ctx: &ApplyContext<'_>,
    owner: &AccountId,
    offer_out: &Leg,
    current_funds: &Leg,
) -> bool {
    let original_funds = original_owner_funds_leg(ctx, owner, offer_out);
    leg_ge(&original_funds, current_funds) && leg_ge(current_funds, &original_funds)
}

/// Build a strand from a boundary chain, snapshotting each hop's AMM pool (with
/// its frozen initial balances) and CLOB presence. Returns `None` if any hop has
/// neither an AMM pool nor a resting offer (a dead hop).
pub(crate) fn build_flow_strand(
    ctx: &ApplyContext<'_>,
    boundaries: &[Value],
) -> Option<FlowStrand> {
    let n = boundaries.len();
    if n < 2 {
        return None;
    }
    let mut hops = Vec::with_capacity(n - 1);
    for h in 0..(n - 1) {
        let in_leg = Leg::parse(&boundaries[h])?;
        let out_leg = Leg::parse(&boundaries[h + 1])?;
        let inverse_book = keylet::book_dir(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        let has_clob = book_has_resting_offer(ctx, &inverse_book);

        let amm_key = keylet::amm(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        let (amm_pool, amm_tfee, liquidity) = match ctx
            .view
            .read(&amm_key)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            Some(amm) => {
                let pool_id = amm
                    .get("Account")
                    .and_then(|v| v.as_str())
                    .and_then(|s| decode_account_id(s).ok());
                let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                match pool_id {
                    Some(pid) => {
                        let pool_in = pool_balance_number(
                            ctx,
                            &pid,
                            in_leg.is_xrp,
                            &in_leg.currency,
                            &in_leg.issuer,
                        );
                        let pool_out = pool_balance_number(
                            ctx,
                            &pid,
                            out_leg.is_xrp,
                            &out_leg.currency,
                            &out_leg.issuer,
                        );
                        if pool_in.is_zero() || pool_out.is_zero() {
                            (None, 0, None)
                        } else {
                            (
                                Some(pid),
                                tfee,
                                Some(AmmLiquidity {
                                    in_is_xrp: in_leg.is_xrp,
                                    out_is_xrp: out_leg.is_xrp,
                                    round_even_quality: ctx
                                        .rules
                                        .enabled(&feature_id("fixUniversalNumber")),
                                    fix_amm_v1_2: ctx.rules.enabled(&feature_id("fixAMMv1_2")),
                                    tfee,
                                    initial_pool_in: pool_in,
                                    initial_pool_out: pool_out,
                                }),
                            )
                        }
                    }
                    None => (None, 0, None),
                }
            }
            None => (None, 0, None),
        };

        // NOTE: a hop with neither an AMM pool nor a resting CLOB offer is a
        // *dry* hop — the strand cannot deliver — but the strand is still
        // syntactically valid and its PRESENCE keeps `multi_path > 1`, which is
        // what triggers rippled's fib-chunked AMM consumption on the OTHER live
        // strand. So we keep it (it simply returns no delivery each pass) rather
        // than dropping it (which would collapse a multi-path payment to a
        // single full swap).
        hops.push(FlowHop {
            in_leg,
            out_leg,
            inverse_book,
            has_clob,
            amm_pool,
            amm_tfee,
            liquidity,
        });
    }
    Some(FlowStrand { hops })
}

/// A strand's a-priori quality upper bound as a `Number` rate (in/out, lower is
/// better): the product of each hop's best-possible rate. Composing left→right,
/// the intermediate magnitudes cancel so the product is the overall
/// input-per-output rate. Used to rank live strands best-first each pass (rippled
/// `StrandFlow.h:444-496` -> `BookStep::qualityUpperBound` -> `getTipQuality`).
///
/// Each hop's rate is the better of its CLOB tip and its AMM offer. The AMM
/// contribution is the *fib-offer* quality (`getAMMOffer(nullopt).quality()`,
/// which folds in the trading fee and the chunk's price impact for the CURRENT
/// `amm_ctx` iteration), NOT the raw spot price — the raw spot over-ranks a
/// pure-AMM strand and flips the first-survivor pick (StrandFlow `getTipQuality`,
/// go-xrpl `step_book.go:869`).
fn strand_quality_ub(
    ctx: &ApplyContext<'_>,
    strand: &FlowStrand,
    dest: &AccountId,
    amm_ctx: &AmmContext,
) -> Option<Number> {
    let mut rate = Number::from_int(1);
    for hop in &strand.hops {
        // AMM fib-offer quality for this iteration (fee + impact), as rippled's
        // `getTipQuality` uses `getAMMOffer(nullopt).quality()`.
        let amm_rate = match (hop.liquidity.as_ref(), hop.amm_pool.as_ref()) {
            (Some(liq), Some(pool)) => {
                let pin = pool_balance_number(
                    ctx,
                    pool,
                    hop.in_leg.is_xrp,
                    &hop.in_leg.currency,
                    &hop.in_leg.issuer,
                );
                let pout = pool_balance_number(
                    ctx,
                    pool,
                    hop.out_leg.is_xrp,
                    &hop.out_leg.currency,
                    &hop.out_leg.issuer,
                );
                liq.get_offer(&pin, &pout, None, amm_ctx)
                    .and_then(|o| from_rate(o.quality).ok())
                    .map(|r| Number::from_iou(&r))
            }
            _ => None,
        };
        // CLOB best-tip rate (in/out) from the book directory quality.
        let clob_rate = book_tip_quality(ctx, &hop.inverse_book)
            .and_then(|q| from_rate(q).ok())
            .map(|r| Number::from_iou(&r));
        let hop_rate = match (amm_rate, clob_rate) {
            (Some(a), Some(c)) => {
                if num_le(&a, &c) {
                    a
                } else {
                    c
                }
            }
            (Some(a), None) => a,
            (None, Some(c)) => c,
            (None, None) => return None,
        };
        // Every collapsed hop represents a payment BookStep. Its preceding
        // step redeems, so the input issuer's transfer rate composes into the
        // upper bound (unless that issuer is the strand destination).
        if !hop.in_leg.is_xrp && hop.in_leg.issuer != *dest {
            rate = rate.mul(&Number::from_iou(&transfer_rate(ctx, &hop.in_leg.issuer)));
        }
        rate = rate.mul(&hop_rate);
    }
    // A Payment ends with an issuer-to-destination DirectStep that may charge
    // a transfer fee. OfferCreate's crossing DirectStep waives that fee; using
    // it here can incorrectly rank a better direct CLOB behind the XRP bridge
    // (105503720 / E5F3CA37).
    let terminal = &strand.hops.last()?.out_leg;
    if !terminal.is_xrp
        && terminal.issuer != *dest
        && ctx.tx.get("TransactionType").and_then(Value::as_str) != Some("OfferCreate")
    {
        rate = rate.mul(&Number::from_iou(&transfer_rate(ctx, &terminal.issuer)));
    }
    Some(rate)
}

/// Initial `ActiveStrands::activateNext` bound for an OfferCreate bridge.
/// At this point Flow still has both the direct and bridge strands, so AMM
/// quotes are Fibonacci chunks and offer crossing waives book transfer fees.
fn offer_crossing_initial_bound(ctx: &ApplyContext<'_>, strand: &FlowStrand) -> Option<Number> {
    let amm_ctx = AmmContext::new(true);
    let mut rate = Number::from_int(1);
    for hop in &strand.hops {
        let clob = book_tip_quality(ctx, &hop.inverse_book);
        let amm = match (hop.liquidity.as_ref(), hop.amm_pool.as_ref()) {
            (Some(liq), Some(pool)) => {
                let pool_in = pool_balance_number(
                    ctx,
                    pool,
                    hop.in_leg.is_xrp,
                    &hop.in_leg.currency,
                    &hop.in_leg.issuer,
                );
                let pool_out = pool_balance_number(
                    ctx,
                    pool,
                    hop.out_leg.is_xrp,
                    &hop.out_leg.currency,
                    &hop.out_leg.issuer,
                );
                liq.get_offer(&pool_in, &pool_out, clob, &amm_ctx)
                    .map(|offer| offer.quality)
            }
            _ => None,
        };
        let best = match (clob, amm) {
            (Some(c), Some(a)) if rxrpl_amount::is_better_quality(a, c) => a,
            (Some(c), _) => c,
            (None, Some(a)) => a,
            (None, None) => return None,
        };
        let hop_rate = from_rate(best).ok().map(|r| Number::from_iou(&r))?;
        rate = rate.mul(&hop_rate);
    }
    Some(rate)
}

/// The first multi-path AMM offer must actually outrank the resting direct
/// book before the shared strand selector takes over an in-quality CLOB walk.
fn first_fib_offer_beats_clob(ctx: &ApplyContext<'_>, strand: &FlowStrand) -> bool {
    let Some(hop) = strand.hops.first() else {
        return false;
    };
    let Some(clob) = book_tip_quality(ctx, &hop.inverse_book) else {
        return false;
    };
    let (Some(liquidity), Some(pool)) = (&hop.liquidity, &hop.amm_pool) else {
        return false;
    };
    let pool_in = pool_balance_number(
        ctx,
        pool,
        hop.in_leg.is_xrp,
        &hop.in_leg.currency,
        &hop.in_leg.issuer,
    );
    let pool_out = pool_balance_number(
        ctx,
        pool,
        hop.out_leg.is_xrp,
        &hop.out_leg.currency,
        &hop.out_leg.issuer,
    );
    liquidity
        .get_offer(&pool_in, &pool_out, Some(clob), &AmmContext::new(true))
        .is_some_and(|offer| rxrpl_amount::is_better_quality(offer.quality, clob))
}

/// Whether the default OfferCreate strand survives Flow's quality-upper-bound
/// gate. BookStep::tip chooses the synthetic AMM offer only when it beats the
/// CLOB tip. BookOfferCrossingStep adjusts the selected AMM's bound for the
/// input transfer rate, but deliberately leaves a CLOB bound fee-free: it must
/// remain an upper bound, including for a self offer within the gross limit.
fn offer_crossing_strand_reaches_limit(
    ctx: &ApplyContext<'_>,
    taker: &AccountId,
    taker_gets: &Value,
    taker_pays: &Value,
    threshold: u64,
) -> bool {
    offer_crossing_strand_reaches_limit_with_amm_mode(
        ctx, taker, taker_gets, taker_pays, threshold, false,
    )
}

/// The first bridge selection quotes both candidate strands with AMM fib
/// offers. A spot-priced single-strand quote can admit a direct AMM whose fee
/// makes its first fib offer worse than the taker's limit (105501906 / TX20).
fn offer_crossing_strand_reaches_limit_with_amm_mode(
    ctx: &ApplyContext<'_>,
    taker: &AccountId,
    taker_gets: &Value,
    taker_pays: &Value,
    threshold: u64,
    multi_path: bool,
) -> bool {
    let Some(strand) = build_flow_strand(ctx, &[taker_gets.clone(), taker_pays.clone()]) else {
        return false;
    };
    let Some(hop) = strand.hops.first() else {
        return false;
    };
    let clob = book_tip_quality(ctx, &hop.inverse_book);
    let amm_threshold = offer_crossing_amm_threshold(clob, Some(threshold), multi_path);
    let amm = match (hop.liquidity.as_ref(), hop.amm_pool.as_ref()) {
        (Some(liq), Some(pool)) => {
            let pool_in = pool_balance_number(
                ctx,
                pool,
                hop.in_leg.is_xrp,
                &hop.in_leg.currency,
                &hop.in_leg.issuer,
            );
            let pool_out = pool_balance_number(
                ctx,
                pool,
                hop.out_leg.is_xrp,
                &hop.out_leg.currency,
                &hop.out_leg.issuer,
            );
            liq.get_offer(
                &pool_in,
                &pool_out,
                amm_threshold,
                &AmmContext::new(multi_path),
            )
            .map(|offer| offer.quality)
        }
        _ => None,
    };
    let (best, is_amm) = match (clob, amm) {
        (Some(c), Some(a)) if rxrpl_amount::is_better_quality(a, c) => (a, true),
        (Some(c), _) => (c, false),
        (None, Some(a)) => (a, true),
        (None, None) => return false,
    };
    let Ok(best_rate) = from_rate(best) else {
        return false;
    };
    let mut rate = Number::from_iou(&best_rate);
    if is_amm && !hop.in_leg.is_xrp && hop.in_leg.issuer != *taker {
        rate = rate.mul(&Number::from_iou(&transfer_rate(ctx, &hop.in_leg.issuer)));
    }
    let Ok(limit) = from_rate(threshold) else {
        return false;
    };
    num_le(&rate, &Number::from_iou(&limit))
}

/// Sum a slice of `Number`s smallest-magnitude first (rippled
/// `StrandFlow.h:619-624`): never a running subtraction. Stable accumulation
/// keeps the byte-exact total independent of pass order.
fn sum_smallest_to_largest(v: &[Number]) -> Number {
    let mut sorted: Vec<Number> = v.to_vec();
    sorted.sort_by(|a, b| {
        let d = a.sub(b);
        if d.is_zero() {
            std::cmp::Ordering::Equal
        } else if d.negative() {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    let mut acc = Number::ZERO;
    for n in &sorted {
        acc = acc.add(n);
    }
    acc
}

fn aggregate_output_remainder(requested: &Leg, outputs: &[Number]) -> (Leg, Number) {
    let requested_num = leg_to_number(requested);
    let delivered = sum_smallest_to_largest(outputs);
    if num_ge(&delivered, &requested_num) {
        return (leg_sub(requested, requested), Number::ZERO);
    }
    let remainder = requested_num.sub(&delivered);
    (number_to_leg(&remainder, requested), remainder)
}

fn next_flow_strands(cur: &[usize], winner_pos: usize) -> Vec<usize> {
    let mut next = Vec::with_capacity(cur.len() - winner_pos);
    next.extend_from_slice(&cur[winner_pos..]);
    next
}

/// A funded IOU remainder can be smaller than one drop at the offer's quality.
/// rippled still consumes that remainder in an XRP -> IOU BookStep: the maker
/// delivers the representable IOU while the taker's input rounds to zero. This
/// must remain a single-hop exception; accepting a zero-input interior hop
/// would manufacture carry between path steps.
fn zero_cost_xrp_clob_fill(spent: &Leg, delivered: &Leg, single_hop: bool) -> bool {
    single_hop && spent.is_xrp && spent.drops == 0 && !delivered.is_zero()
}

/// `a >= b` for two `Number`s.
fn num_ge(a: &Number, b: &Number) -> bool {
    !num_gt(b, a)
}

/// Snapshot a hop's AMM pool as an [`AmmHop`] at the current (live) balances.
fn hop_amm_snapshot(ctx: &ApplyContext<'_>, hop: &FlowHop) -> Option<(AmmHop, Number, Number)> {
    let pool = hop.amm_pool.as_ref()?;
    let pool_in = pool_balance_number(
        ctx,
        pool,
        hop.in_leg.is_xrp,
        &hop.in_leg.currency,
        &hop.in_leg.issuer,
    );
    let pool_out = pool_balance_number(
        ctx,
        pool,
        hop.out_leg.is_xrp,
        &hop.out_leg.currency,
        &hop.out_leg.issuer,
    );
    let amm_hop = AmmHop {
        pool_id: *pool,
        in_xrp: hop.in_leg.is_xrp,
        out_xrp: hop.out_leg.is_xrp,
        in_cur: hop.in_leg.currency,
        in_iss: hop.in_leg.issuer,
        out_cur: hop.out_leg.currency,
        out_iss: hop.out_leg.issuer,
        tfee: hop.amm_tfee,
        pool_in,
        pool_out,
        best_book_quality: None,
    };
    Some((amm_hop, pool_in, pool_out))
}

/// The collapsed XRP bridge cannot feed its next hop when the first reverse
/// quote stops at a single-drop tip while that hop demands more XRP. Retry the
/// first book across quality pages, then carry that scope into its forward walk
/// (105502321/TX7). Other bridge quotes keep rippled's one-band behavior.
fn bridge_xrp_one_drop_tip_needs_more_pages(
    bridge_first_xrp: bool,
    demanded: &Number,
    initial_out: &Number,
) -> bool {
    bridge_first_xrp && demanded.to_xrp_drops() > 1 && initial_out.to_xrp_drops() == 1
}

/// Price a CLOB hop in the reverse direction: how much input the resting
/// offers need to deliver `demanded` output, and what they actually deliver
/// (`min(available capacity, demanded)`). The walk mutates the view (consumes /
/// reaps offers) so it runs on a checkpoint that is immediately rolled back; the
/// forward pass re-walks and commits the real consumption. Returns
/// `(input_needed, output_delivered)` as `Number`s, or `None` when the band is
/// dry. Most hops quote one quality band. A caller may retry the first XRP
/// bridge hop across pages after a one-drop tip. The `skip_*` flags mirror the
/// forward application so the grossed/netted transfer-fee amounts match.
#[allow(clippy::too_many_arguments)]
fn price_clob_band_reverse(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    hop: &FlowHop,
    demanded: &Number,
    source_budget: Option<&Number>,
    is_first: bool,
    is_last: bool,
    permanent_reaps: &mut Vec<PermanentOffer>,
    uncapped_for_source_limit: bool,
    bridge_first_xrp: bool,
    span_quality_pages: bool,
) -> Option<(Number, Number)> {
    let demand_leg = number_to_leg(demanded, &hop.out_leg);
    let single_band = !(bridge_first_xrp && span_quality_pages);
    let first_xrp_input_limited =
        source_budget.is_some() && is_first && !is_last && hop.in_leg.is_xrp && !hop.out_leg.is_xrp;
    // A BookStep reverse pass starts from the requested output, before
    // StrandFlow compares its required input with SendMax. The capped quote
    // below preserves our existing forward pricing, but would stop before
    // invalid offers following a partially filled funded tip. Explore the
    // same output demand without SendMax and retain only permanent removals.
    let mut unbounded_quote = None;
    if source_budget.is_some() {
        let cp = ctx.view.checkpoint();
        let taker_acct_before = taker_acct.clone();
        unbounded_quote = cross_book_hop_with_reaps(
            ctx,
            taker,
            taker_acct,
            dest,
            &demand_leg,
            &unbounded_leg(&hop.in_leg),
            /*skip_input_debit=*/ true,
            /*skip_output_credit=*/ !is_last,
            /*single_band=*/ single_band,
            /*allow_amm=*/ false,
            /*lq_stop=*/ None,
            /*preserve_input_limited_iou_ulp=*/ false,
            /*force_full_input_budget=*/ bridge_first_xrp || first_xrp_input_limited,
            /*source_input_rate=*/ 1_000_000_000,
            /*source_funds_limited=*/ false,
            Some(permanent_reaps),
            false,
            false,
            /*direct_payment_demand=*/ false,
        )
        .ok();
        ctx.view.rollback(cp);
        *taker_acct = taker_acct_before;
    }
    let budget_leg = source_budget
        .map(|budget| number_to_leg(budget, &hop.in_leg))
        .unwrap_or_else(|| unbounded_leg(&hop.in_leg));
    let cp = ctx.view.checkpoint();
    let taker_acct_before = taker_acct.clone();
    let res = cross_book_hop_with_reaps(
        ctx,
        taker,
        taker_acct,
        dest,
        &demand_leg,
        &budget_leg,
        // A multi-hop reverse pass only quotes this book. In particular, its
        // first hop may require more than the account currently holds: the
        // source budget limits that quote during the forward pass below. Do not
        // reject the speculative walk as `tecUNFUNDED_OFFER` before that cap is
        // applied. A single-hop quote keeps the terminal bookkeeping semantics.
        /*skip_input_debit=*/
        !is_first || !is_last,
        /*skip_output_credit=*/ !is_last,
        /*single_band=*/ single_band,
        /*allow_amm=*/ false,
        /*lq_stop=*/ None,
        /*preserve_input_limited_iou_ulp=*/ false,
        /*force_full_input_budget=*/ bridge_first_xrp || first_xrp_input_limited,
        /*source_input_rate=*/ 1_000_000_000,
        /*source_funds_limited=*/ false,
        Some(permanent_reaps),
        false,
        false,
        /*direct_payment_demand=*/ false,
    );
    ctx.view.rollback(cp);
    *taker_acct = taker_acct_before;
    let (delivered, spent) = res.ok()?;
    if delivered.is_zero()
        || (spent.is_zero() && !zero_cost_xrp_clob_fill(&spent, &delivered, is_first && is_last))
    {
        return None;
    }
    // BookStep::rev quotes the requested output before StrandFlow applies
    // SendMax. For an input-limited IOU -> XRP payment, retaining only the
    // capped quote hides that limit and the forward pass re-prices the rounded
    // XRP output, leaving spendable IOU dust at the source (105501273).
    if uncapped_for_source_limit
        && let (Some(budget), Some((full_out, full_in))) = (source_budget, unbounded_quote)
        && num_gt(&leg_to_number(&full_in), budget)
        && num_gt(&leg_to_number(&full_out), &leg_to_number(&delivered))
    {
        return Some((leg_to_number(&full_in), leg_to_number(&full_out)));
    }
    Some((leg_to_number(&spent), leg_to_number(&delivered)))
}

fn apply_permanent_reaps(ctx: &mut ApplyContext<'_>, reaps: &[PermanentOffer]) {
    let mut seen = HashSet::new();
    for &(owner, offer_key, dir_key) in reaps {
        if !seen.insert(offer_key) {
            continue;
        }
        let Some(bytes) = ctx.view.read(&offer_key) else {
            continue;
        };
        let Ok(offer) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Some(offer_out) = offer.get("TakerGets").and_then(Leg::parse) else {
            continue;
        };
        let Some(offer_in) = offer.get("TakerPays").and_then(Leg::parse) else {
            continue;
        };
        let expired = offer
            .get("Expiration")
            .and_then(Value::as_u64)
            .is_some_and(|exp| exp <= ctx.view.parent_close_time() as u64);
        let funds = owner_funds_leg(ctx, &owner, &offer_out);
        let originally_unfunded = original_owner_funds_leg(ctx, &owner, &offer_out).is_zero();
        let quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        let tiny =
            should_reap_tiny_increased_quality(&owner, &offer_in, &offer_out, &funds, quality)
                && tiny_offer_is_permanently_reapable(ctx, &owner, &offer_out, &funds);
        if !expired && !tiny && (!funds.is_zero() || !originally_unfunded) {
            continue;
        }
        let cp = ctx.view.checkpoint();
        if reap_offer(ctx, &owner, &offer_key, &dir_key).is_err() {
            ctx.view.rollback(cp);
        }
    }
}

/// The AMM quality cap for a BookOfferCrossingStep at the current CLOB tip.
fn offer_crossing_amm_threshold(
    clob_quality: Option<u64>,
    limit_quality: Option<u64>,
    multi_path: bool,
) -> Option<u64> {
    // BookOfferCrossingStep::qualityThreshold leaves the CLOB tip out of AMM
    // offer generation when a single strand's transaction limit is better
    // than that tip. In a multi-path pass, the CLOB tip always caps the AMM.
    if !multi_path
        && clob_quality
            .zip(limit_quality)
            .is_some_and(|(clob, limit)| rxrpl_amount::is_better_quality(limit, clob))
    {
        None
    } else {
        clob_quality
    }
}

#[allow(clippy::too_many_arguments)]
fn limit_strand_output_by_quality(
    ctx: &mut ApplyContext<'_>,
    dest: &AccountId,
    strand: &FlowStrand,
    amm_hops: &[Option<AmmHop>],
    remaining_out: &Number,
    limit_quality: u64,
    amm_ctx: &AmmContext,
) -> Number {
    use rxrpl_amount::number::{RoundModeGuard, RoundingMode};

    // rippled `QualityFunction`: average output/input quality is linear in the
    // terminal output, q(out) = m*out + b. Compose the collapsed DirectStep /
    // BookStep chain in forward order, then solve it at the taker's limit.
    let mut m = Number::ZERO;
    let mut b = Number::from_int(1);
    for (h, hop) in strand.hops.iter().enumerate() {
        if let Some(rate) = flow_book_input_rate(ctx, &hop.in_leg, dest) {
            b = b.div(&Number::from_iou(&rate));
        }

        // `limitOut` builds its QualityFunction from `BookStep::tip()`, which
        // reads the raw BookTip in a local sandbox. OfferStream filters expired
        // and unfunded offers only when the strand is actually executed.
        let clob_quality = book_tip_quality(ctx, &hop.inverse_book);
        let amm_threshold =
            offer_crossing_amm_threshold(clob_quality, Some(limit_quality), amm_ctx.multi_path());
        let amm_offer = match (hop.liquidity.as_ref(), amm_hops[h].as_ref()) {
            (Some(liq), Some(amm)) => {
                liq.get_offer(&amm.pool_in, &amm.pool_out, amm_threshold, amm_ctx)
            }
            _ => None,
        };
        // BookStep::tip uses the raw CLOB tip for limitOut whenever it is at
        // least as good as the synthetic AMM offer. A constant-quality CLOB
        // tip leaves the requested output unchanged, so the Flow tolerance
        // for an adjusted limit must not apply in that case.
        let amm_offer = amm_offer.filter(|offer| {
            clob_quality.is_none_or(|clob| rxrpl_amount::is_better_quality(offer.quality, clob))
        });

        let (step_m, step_b) = if let (Some(offer), Some(amm)) = (amm_offer, amm_hops[h].as_ref()) {
            if amm_ctx.multi_path() {
                let rate =
                    Number::from_iou(&from_rate(offer.quality).ok().unwrap_or(IOUAmount::ZERO));
                if rate.is_zero() {
                    return *remaining_out;
                }
                (Number::ZERO, Number::from_int(1).div(&rate))
            } else {
                let fee = Number::from_int(hop.amm_tfee as i64).div(&Number::from_int(100_000));
                let fee_mult = Number::from_int(1).sub(&fee);
                (
                    fee_mult.negate().div(&amm.pool_in),
                    amm.pool_out.mul(&fee_mult).div(&amm.pool_in),
                )
            }
        } else if let Some(quality) = clob_quality {
            let Ok(rate) = from_rate(quality) else {
                return *remaining_out;
            };
            let rate = Number::from_iou(&rate);
            if rate.is_zero() {
                return *remaining_out;
            }
            (Number::ZERO, Number::from_int(1).div(&rate))
        } else {
            return *remaining_out;
        };
        m = m.add(&b.mul(&step_m));
        b = b.mul(&step_b);
    }

    // BookOfferCrossingStep waives the AMM output transfer fee (`trOut` is
    // parity). The terminal issue therefore contributes no extra constant
    // quality term here, even though settlement still moves the gross pool
    // output through the issuer's trust lines.
    if m.is_zero() {
        return *remaining_out;
    }
    let Ok(rate) = from_rate(limit_quality) else {
        return *remaining_out;
    };
    let rate = Number::from_iou(&rate);
    if rate.is_zero() {
        return *remaining_out;
    }
    let raw = {
        let _round = RoundModeGuard::new(RoundingMode::Upward);
        Number::from_int(1).div(&rate).sub(&b).div(&m)
    };
    if raw.is_zero() || raw.negative() {
        return *remaining_out;
    }
    let output = {
        let out_leg = &strand.hops.last().expect("non-empty strand").out_leg;
        if out_leg.is_xrp {
            let _round = RoundModeGuard::new(RoundingMode::Downward);
            Number::from_int(raw.to_xrp_drops_mode() as i64)
        } else {
            // StrandFlow converts an IOU output under the ambient rounding
            // mode after outFromAvgQ restores its temporary Upward guard.
            Number::from_iou(&raw.to_iou())
        }
    };
    let delta = output.sub(remaining_out);
    let abs_delta = if delta.negative() {
        delta.negate()
    } else {
        delta
    };
    let tolerance = remaining_out.div(&Number::from_int(1_000_000_000));
    if num_le(&abs_delta, &tolerance) {
        *remaining_out
    } else if num_le(&output, remaining_out) {
        output
    } else {
        *remaining_out
    }
}

/// A calculated quality cap ends a strand only when this pass actually
/// delivers up to it. An earlier book band may limit the pass first, leaving
/// other bands available to the next pass.
fn strand_quality_cap_reached(requested: &Number, capped: &Number, delivered: &Number) -> bool {
    !capped.sub(requested).is_zero() && num_ge(delivered, capped)
}

/// Execute ONE pass of a (multi-hop, AMM and/or CLOB) strand with rippled's
/// reverse-then-forward `StrandFlow` algorithm (StrandFlow.h:80-281).
///
/// Ordinarily each hop consumes one quality band per pass. The first XRP hop
/// of an OfferCreate bridge can span pages in this collapsed execution when a
/// one-drop tip leaves the downstream demand unmet (105502321/TX7). This is
/// a local representation of the route; rippled's BookStep ordinarily keeps
/// one quality per pass. BookStep tries the AMM first at the
/// CLOB tip quality ([`AmmLiquidity::get_offer`]); the AMM
/// declines (returns `None`) when its fib chunk is no better than the resting
/// book, and the band is then filled by the CLOB tip alone. A pure-AMM hop is
/// the fib chunk; a pure-CLOB hop normally takes the tip band; a dry hop
/// (neither) fails the strand this pass.
///
/// **Reverse pass**: from the demanded strand output `remaining_out`, pull the
/// demand back through every hop. An AMM hop's chunk is clamped to the demand via
/// `limit_out`; a CLOB hop is priced by `cross_book_hop` on a
/// checkpoint that is immediately rolled back. The chunk/band's required input
/// becomes the upstream hop's demand. The hop whose band cannot meet the
/// back-propagated demand is the *limiting step*. The source (hop 0) is also
/// limited when its required input exceeds the `remaining_in` budget.
///
/// **Forward pass** (mutates the checkpointed view): hops `0..=limiting` keep
/// their reverse-priced amounts (which already thread — each hop's output equals
/// the next hop's input); hops past the limiting step are re-priced forward
/// against the reduced carry (AMM via `limit_in`, CLOB by an input-limited
/// one-band walk), so the intermediate asset is conserved exactly. The AMM
/// mutation reuses [`apply_amm_move`]; the CLOB mutation/reap reuses
/// [`cross_book_hop`]. The SHARED `amm_ctx` threads `curIters` across all AMM
/// hops of the strand.
///
/// Returns the realised `(source-in, delivered-out)`, or `None` when the strand
/// cannot deliver this pass (a pool is frozen, a hop is dry, or the demand cannot
/// be priced).
#[allow(clippy::too_many_arguments)]
fn execute_strand_pass(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    strand: &FlowStrand,
    remaining_in: &Number,
    remaining_out: &Number,
    amm_ctx: &mut AmmContext,
    offer_create_bridge: bool,
    source_funds_limited: bool,
    limit_quality: Option<u64>,
    permanent_reaps: &mut Vec<PermanentOffer>,
) -> Option<StrandPass> {
    let trace = std::env::var("RXRPL_FLOW_TRACE").is_ok();
    let n = strand.hops.len();
    if n == 0 {
        return None;
    }

    // Per-hop AMM snapshot (live pool balances at the start of this pass), or
    // `None` for a pure-CLOB hop. A hop with neither AMM nor CLOB is dry.
    let mut amm_hops: Vec<Option<AmmHop>> = Vec::with_capacity(n);
    for hop in &strand.hops {
        amm_hops.push(hop_amm_snapshot(ctx, hop).map(|(a, _, _)| a));
    }
    let requested_out = *remaining_out;
    let remaining_out = limit_quality
        .filter(|_| !amm_ctx.multi_path())
        .map(|quality| {
            limit_strand_output_by_quality(
                ctx,
                dest,
                strand,
                &amm_hops,
                remaining_out,
                quality,
                amm_ctx,
            )
        })
        .unwrap_or(*remaining_out);

    // Whether a hop consumes its AMM fib chunk this pass, or its CLOB tip band.
    // rippled's `BookStep` tries the AMM FIRST at the CLOB tip quality; the AMM
    // declines (`get_offer` -> None) when it is no better than the book, and the
    // band is then filled by the resting CLOB offers alone.
    #[derive(Clone, Copy, PartialEq)]
    enum HopKindR {
        Amm,
        Clob,
    }

    // --- REVERSE PASS: back-propagate the demand, find the limiting step. ---
    let mut ins: Vec<Number> = vec![Number::ZERO; n];
    let mut outs: Vec<Number> = vec![Number::ZERO; n];
    let mut kinds: Vec<HopKindR> = vec![HopKindR::Amm; n];
    let mut bridge_xrp_spans_pages = vec![false; n];
    let mut first_xrp_budgeted_quote = false;
    // The lowest-index hop whose band cannot meet the demand pulled back to it;
    // `n` means "no interior limit" (only the source budget may bind).
    let mut limiting = n;
    let mut demanded = remaining_out;
    for h in (0..n).rev() {
        let hop = &strand.hops[h];
        let is_first = h == 0;
        let is_last = h == n - 1;
        let clob_quality = funded_book_tip_quality(ctx, taker, &hop.inverse_book);

        // Try the AMM fib chunk first, gated to the CLOB tip quality.
        // BookOfferCrossingStep compares the AMM's synthetic offer with the
        // funded CLOB tip even on an XRP autobridge hop (105500934).
        let amm_threshold = if offer_create_bridge {
            offer_crossing_amm_threshold(clob_quality, limit_quality, amm_ctx.multi_path())
        } else {
            clob_quality
        };
        let amm_offer = match (hop.liquidity.as_ref(), amm_hops[h].as_ref()) {
            (Some(liq), Some(amm)) => {
                liq.get_offer(&amm.pool_in, &amm.pool_out, amm_threshold, amm_ctx)
            }
            _ => None,
        };
        if let Some(offer) = amm_offer {
            kinds[h] = HopKindR::Amm;
            let pool_id = amm_hops[h].as_ref()?.pool_id;
            // `demanded` is the NET amount the destination must receive, while
            // an AMM synthetic offer's output is the GROSS amount removed from
            // the pool. On the terminal hop, gross the reverse demand when the
            // output issuer charges a transfer fee. The forward application
            // then burns that fee and returns the original net carry.
            let pool_demanded = if is_last
                && !offer_create_bridge
                && !hop.out_leg.is_xrp
                && hop.out_leg.issuer != pool_id
                && hop.out_leg.issuer != *dest
            {
                Number::from_iou(&grossed(
                    &demanded.to_iou(),
                    &transfer_rate(ctx, &hop.out_leg.issuer),
                ))
            } else {
                demanded
            };
            if num_ge(&offer.out_num, &pool_demanded) {
                // The chunk covers the demand: clamp output to demand at constant
                // quality (`ceilOutStrict`, round the cost up).
                let (i, o) = offer.limit_out(&pool_demanded, true);
                ins[h] = i;
                outs[h] = o;
            } else {
                // The chunk is smaller than the demand: this hop limits the pass.
                ins[h] = offer.in_num;
                outs[h] = offer.out_num;
                limiting = h;
            }
        } else if clob_quality.is_some() {
            // AMM declined (or absent) but the book has resting offers: price the
            // CLOB tip band for the demand pulled to this hop. Done on a
            // checkpoint that is rolled back — the forward pass re-runs the walk
            // and commits the offer consumption / reaps.
            if is_last && hop.out_leg.is_xrp {
                permanent_reaps.extend(reap_expired_reverse_xrp_book_tail(
                    ctx,
                    taker,
                    &hop.in_leg,
                    &hop.out_leg,
                    &number_to_leg(&demanded, &hop.out_leg),
                ));
            }
            kinds[h] = HopKindR::Clob;
            let source_budget =
                (n == 1 && is_first && hop.liquidity.is_some()).then_some(remaining_in);
            let uncapped_for_source_limit =
                !offer_create_bridge && n == 1 && !hop.in_leg.is_xrp && hop.out_leg.is_xrp;
            let bridge_first_xrp =
                offer_create_bridge && is_first && !is_last && hop.out_leg.is_xrp;
            let mut quote = price_clob_band_reverse(
                ctx,
                taker,
                taker_acct,
                dest,
                hop,
                &demanded,
                source_budget,
                is_first,
                is_last,
                permanent_reaps,
                uncapped_for_source_limit,
                bridge_first_xrp,
                false,
            )?;
            // The first XRP book is reverse-priced without SendMax. Only when
            // that quote exceeds the source budget must BookStep rerun it with
            // an input limit, spending the full drop budget and flooring its
            // IOU output at the fixed directory quality (105504137). Keep the
            // unbounded quote for all other strands.
            if n == 2
                && is_first
                && hop.in_leg.is_xrp
                && !hop.out_leg.is_xrp
                && num_gt(&quote.0, remaining_in)
            {
                quote = price_clob_band_reverse(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    hop,
                    &demanded,
                    Some(remaining_in),
                    is_first,
                    is_last,
                    permanent_reaps,
                    uncapped_for_source_limit,
                    bridge_first_xrp,
                    false,
                )?;
                first_xrp_budgeted_quote = true;
            }
            // A sell has an unbounded output demand: consume each quality
            // page as a separate pass so the next page is checked against the
            // taker's quality limit before it can dilute a favorable tip.
            let sell_bridge = offer_create_bridge
                && ctx.tx.get("Flags").and_then(Value::as_u64).unwrap_or(0) & TF_SELL != 0;
            if !sell_bridge
                && bridge_xrp_one_drop_tip_needs_more_pages(bridge_first_xrp, &demanded, &quote.1)
                && let Some(expanded) = price_clob_band_reverse(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    hop,
                    &demanded,
                    source_budget,
                    is_first,
                    is_last,
                    permanent_reaps,
                    uncapped_for_source_limit,
                    bridge_first_xrp,
                    true,
                )
                && num_gt(&expanded.1, &quote.1)
            {
                quote = expanded;
                bridge_xrp_spans_pages[h] = true;
            }
            let (i, o) = quote;
            ins[h] = i;
            outs[h] = o;
            // The band delivered less than demanded -> this hop limits the pass.
            if num_gt(&demanded, &o) {
                limiting = h;
            }
        } else {
            // Dry hop: neither AMM liquidity nor a resting CLOB offer.
            return None;
        }
        let zero_cost_xrp_clob = n == 1
            && kinds[h] == HopKindR::Clob
            && hop.in_leg.is_xrp
            && ins[h].is_zero()
            && !outs[h].is_zero();
        if (ins[h].is_zero() && !zero_cost_xrp_clob) || outs[h].is_zero() || ins[h].negative() {
            return None;
        }
        demanded = if h > 0 {
            flow_gross_input(&ins[h], flow_book_input_rate(ctx, &hop.in_leg, dest))
        } else {
            ins[h]
        };
    }

    // The source DirectStep feeds the first BookStep. When that book's IOU
    // input charges a transfer rate, SendMax is the gross source amount while
    // `ins[0]` is the net amount received by the book/pool.
    let source = &strand.hops[0].in_leg;
    let source_rate = flow_book_input_rate(ctx, source, dest);
    let required_source = flow_gross_input(&ins[0], source_rate);
    let source_capped = num_gt(&required_source, remaining_in);
    let source_available = if source_capped {
        flow_net_input(remaining_in, source_rate)
    } else {
        *remaining_in
    };
    if source_capped {
        limiting = 0;
    }
    let first_xrp_source_budget_binds =
        first_xrp_budgeted_quote && limiting == 0 && num_ge(&ins[0], remaining_in);

    // --- FORWARD PASS: apply, threading the intermediate so it conserves. ---
    let mut source_in = ins[0];
    let mut carry = Number::ZERO;
    for h in 0..n {
        let hop = &strand.hops[h];
        let is_first = h == 0;
        let is_last = h == n - 1;
        let clob_q = funded_book_tip_quality(ctx, taker, &hop.inverse_book);
        let amm_threshold = if offer_create_bridge {
            offer_crossing_amm_threshold(clob_q, limit_quality, amm_ctx.multi_path())
        } else {
            clob_q
        };
        let available_input = if is_first {
            source_available
        } else {
            let input_rate = flow_book_input_rate(ctx, &hop.in_leg, dest);
            if is_last
                && h > limiting
                && let Some(rate) = input_rate.filter(|rate| *rate > parity_rate())
            {
                flow_net_input_terminal_forward(&carry, rate)
            } else {
                flow_net_input(&carry, input_rate)
            }
        };

        match kinds[h] {
            HopKindR::Amm => {
                let amm = amm_hops[h].as_ref()?;
                let (in_num, out_num) = if is_first && source_capped {
                    // Source-limited: re-price hop 0 forward from the budget.
                    let liq = hop.liquidity.as_ref()?;
                    let offer =
                        liq.get_offer(&amm.pool_in, &amm.pool_out, amm_threshold, amm_ctx)?;
                    offer.limit_in(&source_available, false)
                } else if h <= limiting {
                    // Reverse-priced; conserved by construction (outs[h-1] == ins[h]).
                    (ins[h], outs[h])
                } else {
                    // Past the limiting step: re-price forward against the reduced
                    // carry (`limitStepIn`, round the output down), keeping quality.
                    let liq = hop.liquidity.as_ref()?;
                    let offer =
                        liq.get_offer(&amm.pool_in, &amm.pool_out, amm_threshold, amm_ctx)?;
                    if num_ge(&offer.in_num, &available_input) {
                        offer.limit_in(&available_input, false)
                    } else {
                        (offer.in_num, offer.out_num)
                    }
                };
                if in_num.is_zero() || out_num.is_zero() || in_num.negative() || out_num.negative()
                {
                    return None;
                }
                let actual_out = apply_amm_move(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    amm,
                    &in_num,
                    &out_num,
                    /*skip_input_debit=*/ !is_first,
                    /*skip_output_credit=*/ !is_last,
                    /*waive_output_transfer_fee=*/ offer_create_bridge && is_last,
                )
                .ok()?;
                amm_ctx.set_amm_used();
                if trace {
                    eprintln!(
                        "[flow]     hop{h} AMM in={} out={} net={} (iter={} limiting={limiting})",
                        in_num.to_iou().to_decimal_string(),
                        out_num.to_iou().to_decimal_string(),
                        actual_out.to_iou().to_decimal_string(),
                        amm_ctx.cur_iters(),
                    );
                }
                if is_first {
                    source_in = in_num;
                }
                carry = actual_out;
            }
            HopKindR::Clob => {
                // Drive the CLOB band: at/below the limiting step deliver the
                // reverse-priced output; above it (or source-capped) drive by the
                // available input (`BookStep::fwd`, input-limited).
                let bridge_xrp_reverse =
                    offer_create_bridge && is_first && !is_last && hop.out_leg.is_xrp;
                // Reuse the exact directory scope selected by the reverse
                // quote; other bridge fills stay within their first band.
                let single_band = !bridge_xrp_spans_pages[h];
                let (demand_leg, budget_leg) = if is_first && source_capped {
                    (
                        unbounded_leg(&hop.out_leg),
                        number_to_leg(&source_available, &hop.in_leg),
                    )
                } else if h <= limiting {
                    let budget = if is_first {
                        number_to_leg(&ins[0], &hop.in_leg)
                    } else {
                        unbounded_leg(&hop.in_leg)
                    };
                    (number_to_leg(&outs[h], &hop.out_leg), budget)
                } else {
                    (
                        unbounded_leg(&hop.out_leg),
                        number_to_leg(&available_input, &hop.in_leg),
                    )
                };
                let (delivered, spent) = cross_book_hop_with_reaps(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    &demand_leg,
                    &budget_leg,
                    /*skip_input_debit=*/ !is_first,
                    /*skip_output_credit=*/ !is_last,
                    /*single_band=*/ single_band,
                    /*allow_amm=*/ false,
                    /*lq_stop=*/ None,
                    /*preserve_input_limited_iou_ulp=*/
                    preserve_flow_input_limited_iou_ulp(
                        h,
                        limiting,
                        hop.in_leg.is_xrp,
                        is_last
                            && taker == dest
                            && ctx.rules.enabled(&feature_id("fixUniversalNumber")),
                    ),
                    // OfferCreate autobridging conserves the discrete XRP
                    // produced by the first book: its terminal XRP -> IOU
                    // BookStep forwards the entire drop budget instead of
                    // repricing the floored IOU output one drop lower
                    // (105500249 4C84DE5C).
                    /*force_full_input_budget=*/
                    bridge_xrp_reverse
                        || (is_first && first_xrp_source_budget_binds)
                        || flow_first_book_spends_source_funds(
                            source_funds_limited,
                            source_capped,
                            is_first && !is_last,
                            taker == dest,
                            source_rate,
                            hop.out_leg.is_xrp,
                        ),
                    /*source_input_rate=*/ 1_000_000_000,
                    /*source_funds_limited=*/ false,
                    /*permanent_reaps=*/ None,
                    /*reverse_direct_tail=*/ false,
                    // The source DirectStep retains its Number residual when
                    // a charged source finishes an AMM + CLOB crossing. A
                    // local dust-drain heuristic would erase that nonzero
                    // residual and delete the trust line (105504158/TX41).
                    /*preserve_source_residual=*/
                    offer_create_bridge
                        && is_first
                        && source_rate.is_some_and(|rate| rate > parity_rate())
                        && amm_ctx.cur_iters() > 0,
                    /*direct_payment_demand=*/ false,
                )
                .ok()?;
                if delivered.is_zero()
                    || (spent.is_zero() && !zero_cost_xrp_clob_fill(&spent, &delivered, n == 1))
                {
                    return None;
                }
                if trace {
                    eprintln!(
                        "[flow]     hop{h} CLOB in={} out={} (limiting={limiting})",
                        leg_to_number(&spent).to_iou().to_decimal_string(),
                        leg_to_number(&delivered).to_iou().to_decimal_string(),
                    );
                }
                if is_first {
                    source_in = leg_to_number(&spent);
                }
                carry = leg_to_number(&delivered);
            }
        }
    }

    let zero_cost_xrp_clob = n == 1
        && kinds[0] == HopKindR::Clob
        && strand.hops[0].in_leg.is_xrp
        && source_in.is_zero()
        && !carry.is_zero();
    if (source_in.is_zero() && !zero_cost_xrp_clob) || carry.is_zero() {
        return None;
    }
    source_in = flow_gross_input(&source_in, source_rate);
    let quality_adjusted = strand_quality_cap_reached(&requested_out, &remaining_out, &carry);
    Some(StrandPass {
        in_amt: source_in,
        out_amt: carry,
        offers_used: 0,
        quality_adjusted,
        preserve_output_ulp: n > 1
            && limiting < n - 1
            && kinds[n - 2] == HopKindR::Amm
            && kinds[n - 1] == HopKindR::Clob,
    })
}

/// rippled `Flow` multi-pass loop (sorted / first-survivor, `FlowSortStrands`
/// active). Each pass ranks the live strands best-quality-first, recomputes the
/// `multi_path` toggle from the live count, then picks the FIRST strand that
/// delivers (checkpointing before each trial, rolling back failed ones, keeping
/// the winner's mutation). `amm_ctx` is cleared per trial and advanced once per
/// committed winner — this drives the fib index. Remaining in/out are always
/// recomputed via `sum_smallest_to_largest`, never a running subtraction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flow_multi(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    strands: &[FlowStrand],
    deliver_req: &Number,
    send_max: &Number,
    limit_rate: Option<&Number>,
    amm_ctx: &mut AmmContext,
) -> (Number, Number, bool) {
    let trace = std::env::var("RXRPL_FLOW_TRACE").is_ok();
    let source_balance = flow_source_balance(ctx, taker, strands);
    let destination_balance = flow_destination_balance(ctx, dest, strands);
    // SendMax is only the transaction's ceiling. An IOU source DirectStep is
    // additionally bounded by the holder's balance at Flow entry. Keep that
    // cap across every pass: recomputing only `SendMax - savedIn` can otherwise
    // launch a second strand pass after the first one has drained the source.
    let input_limit = flow_input_limit(send_max, source_balance.as_ref());
    // Payment may already have capped the `send_max` argument to accountFunds.
    // Compare against the signed transaction's original SendMax instead.
    let tx_send_max = ctx.tx.get("SendMax").and_then(Leg::parse);
    let source_funds_limited = source_balance.is_some()
        && flow_source_funds_limit_binds(tx_send_max.as_ref(), &input_limit);
    if trace {
        eprintln!(
            "[flow] ENTER strands={} deliver_req={} send_max={} input_limit={}",
            strands.len(),
            deliver_req.to_iou().to_decimal_string(),
            send_max.to_iou().to_decimal_string(),
            input_limit.to_iou().to_decimal_string(),
        );
        for (si, s) in strands.iter().enumerate() {
            let desc: Vec<String> = s
                .hops
                .iter()
                .map(|h| format!("amm={} clob={}", h.amm_pool.is_some(), h.has_clob))
                .collect();
            eprintln!(
                "[flow]   strand {si}: {} hops [{}]",
                s.hops.len(),
                desc.join(" | ")
            );
        }
    }
    let mut saved_ins: Vec<Number> = Vec::new();
    let mut saved_outs: Vec<Number> = Vec::new();
    let mut offers_considered = 0u32;
    let mut permanent_reaps = Vec::new();
    let mut preserve_output_ulp = false;
    let mut next: Vec<usize> = (0..strands.len()).collect();

    for cur_try in 0..FLOW_MAX_TRIES {
        let remaining_out = {
            let d = deliver_req.sub(&sum_smallest_to_largest(&saved_outs));
            if d.is_zero() || d.negative() {
                break;
            }
            d
        };
        let remaining_in = {
            let accounting = input_limit.sub(&sum_smallest_to_largest(&saved_ins));
            // The source DirectStep checks accountFunds on every pass. Its
            // live trust-line balance can be one IOU ULP below SendMax minus
            // the rounded pass totals; use the smaller value before quoting
            // the next offer (105503522/ABC2668D).
            let r = flow_source_pass_budget(
                accounting,
                source_balance
                    .as_ref()
                    .and_then(|source| flow_live_source_funds(ctx, source)),
            );
            if r.is_zero() || r.negative() {
                break;
            }
            r
        };

        // (A) activateNext: discard dry/over-limit strands and rank the survivors.
        // After a winner is chosen only that strand and the untried suffix remain
        // eligible, matching rippled's `ActiveStrands` lifecycle.
        let mut cur: Vec<usize> = next
            .drain(..)
            .filter(|&i| {
                strand_quality_ub(ctx, &strands[i], dest, amm_ctx)
                    .is_some_and(|q| limit_rate.is_none_or(|limit| !num_gt(&q, limit)))
            })
            .collect();
        if cur.len() > 1 {
            cur.sort_by(|&a, &b| {
                let ra = strand_quality_ub(ctx, &strands[a], dest, amm_ctx).unwrap();
                let rb = strand_quality_ub(ctx, &strands[b], dest, amm_ctx).unwrap();
                let d = ra.sub(&rb);
                if d.is_zero() {
                    std::cmp::Ordering::Equal
                } else if d.negative() {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            });
        }
        if cur.is_empty() {
            break;
        }
        if trace {
            let qs: Vec<String> = cur
                .iter()
                .map(|&i| {
                    format!(
                        "s{i}={}",
                        strand_quality_ub(ctx, &strands[i], dest, amm_ctx)
                            .unwrap()
                            .to_iou()
                            .to_decimal_string()
                    )
                })
                .collect();
            eprintln!("[flow]   try={cur_try} rank [{}]", qs.join(" "));
        }
        // OfferCreate keeps the syntactic XRP bridge in its AMM sizing even
        // when that bridge misses the taker's limit (105504158). Payment's
        // existing strand selector sizes from the surviving active paths;
        // changing it to the original count regresses earlier payments.
        let offer_crossing =
            ctx.tx.get("TransactionType").and_then(Value::as_str) == Some("OfferCreate");
        amm_ctx.set_multi_path(if offer_crossing {
            strands.len() > 1
        } else {
            cur.len() > 1
        });

        // (D) pick the FIRST surviving strand (checkpoint per trial; roll back
        // losers; keep the winner's mutation).
        let mut winner: Option<(usize, StrandPass)> = None;
        let mut winner_pos = None;
        for (pos, &si) in cur.iter().enumerate() {
            let cp = ctx.view.checkpoint();
            let taker_acct_before = taker_acct.clone();
            amm_ctx.clear();
            match execute_strand_pass(
                ctx,
                taker,
                taker_acct,
                dest,
                &strands[si],
                &remaining_in,
                &remaining_out,
                amm_ctx,
                /*offer_create_bridge=*/
                ctx.tx.get("TransactionType").and_then(Value::as_str) == Some("OfferCreate"),
                source_funds_limited,
                None,
                &mut permanent_reaps,
            ) {
                Some(res) if !res.out_amt.is_zero() => {
                    let realised_rate = res.in_amt.div(&res.out_amt);
                    if limit_rate.is_some_and(|limit| num_gt(&realised_rate, limit)) {
                        if trace {
                            eprintln!(
                                "[flow]   try={cur_try} strand={si} misses limit realised={} limit={}",
                                realised_rate.to_iou().to_decimal_string(),
                                limit_rate.unwrap().to_iou().to_decimal_string(),
                            );
                        }
                        ctx.view.rollback(cp);
                        *taker_acct = taker_acct_before;
                        amm_ctx.clear();
                        continue;
                    }
                    offers_considered += res.offers_used;
                    winner = Some((si, res));
                    winner_pos = Some(pos);
                    break;
                }
                other => {
                    if let Some(r) = other {
                        offers_considered += r.offers_used;
                    }
                    if trace {
                        eprintln!("[flow]   try={cur_try} strand={si} FAILED pass");
                    }
                    ctx.view.rollback(cp);
                    *taker_acct = taker_acct_before;
                }
            }
        }

        let Some((si, res)) = winner else {
            break;
        };
        let pos = winner_pos.expect("winner position");
        next = next_flow_strands(&cur, pos);
        if trace {
            eprintln!(
                "[flow] try={cur_try} strand={si} multi={} amm_iter={} in={} out={}",
                amm_ctx.multi_path(),
                amm_ctx.cur_iters(),
                res.in_amt.to_iou().to_decimal_string(),
                res.out_amt.to_iou().to_decimal_string(),
            );
        }
        saved_ins.push(res.in_amt);
        saved_outs.push(res.out_amt);
        preserve_output_ulp |= res.preserve_output_ulp;
        // (E) advance the fib counter once per committed winner.
        amm_ctx.update();

        if offers_considered >= FLOW_MAX_OFFERS {
            break;
        }
    }

    // FlowOfferStream's permanent removal set survives reverse checkpoints and
    // losing strand sandboxes. Apply it to the selected flow after all passes;
    // a forward pass may already have removed one of the same offers.
    apply_permanent_reaps(ctx, &permanent_reaps);

    let actual_out = sum_smallest_to_largest(&saved_outs);
    let actual_in = sum_smallest_to_largest(&saved_ins);
    let input_exhausted = actual_in.sub(&input_limit).is_zero();
    // rippled's source DirectStep settles the aggregate Flow input. Our
    // collapsed book/AMM hop settles each synthetic offer separately, which
    // can accumulate a one-ULP drift on a nearly drained trust line. Restore
    // the source endpoint from its pre-flow balance and the canonical
    // smallest-to-largest total while leaving pool/maker settlement per pass.
    // Self-payments retain the source DirectStep's pass-order rounding rather
    // than replacing it with a single sorted aggregate. This matters both when
    // the source is exhausted (105500439) and when several AMM passes consume a
    // small fraction of SendMax (105500548).
    if saved_ins.len() > 1
        && let Some(source) = source_balance.as_ref()
    {
        if taker == dest {
            reconcile_flow_source_balance_passes(ctx, source, &saved_ins);
        } else if !input_exhausted {
            reconcile_flow_source_balance(ctx, source, &actual_in);
        }
    }
    // A single input-capped self-payment also settles its source DirectStep
    // with the exact gross SendMax. Reconstructing the gross from the rounded
    // AMM net can leave one IOU ULP unspent (105500929).
    if saved_ins.len() == 1
        && input_exhausted
        && taker == dest
        && let Some(source) = source_balance.as_ref()
        && destination_balance
            .as_ref()
            .is_none_or(|destination| source.key != destination.key)
    {
        reconcile_flow_source_balance_passes(ctx, source, &saved_ins);
    }
    // Each winning rippled strand applies its destination DirectStep to the
    // accumulated sandbox immediately. For an input-exhausted self-payment,
    // replay those credits in commit order: replacing them with one aggregate
    // credit loses an ULP against a large existing balance (105500595).
    if taker == dest
        && input_exhausted
        && saved_outs.len() > 1
        && let Some(destination) = destination_balance.as_ref()
        && source_balance
            .as_ref()
            .is_none_or(|source| source.key != destination.key)
    {
        reconcile_flow_destination_balance_passes(ctx, destination, &saved_outs);
    }
    let preserve_output_ulp = preserve_flow_output_ulp(
        &actual_out,
        deliver_req,
        preserve_output_ulp || input_exhausted || (taker == dest && saved_outs.len() > 1),
    );
    (actual_out, actual_in, preserve_output_ulp)
}

/// `a + b` for like-typed legs (same asset).
fn leg_add(a: &Leg, b: &Leg) -> Leg {
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops + b.drops;
    } else {
        out.iou = IOUAmount::add(&a.iou, &b.iou).unwrap_or(a.iou);
    }
    out
}

/// A leg's magnitude as an IOUAmount for quality math (drops become an integer).
fn leg_as_quality_iou(leg: &Leg) -> IOUAmount {
    if leg.is_xrp {
        IOUAmount::from_decimal_string(&leg.drops.to_string()).unwrap_or(IOUAmount::ZERO)
    } else {
        leg.iou
    }
}

/// A leg as a unified `Amount` (XRP drops or IOU).
fn leg_to_amount(leg: &Leg) -> rxrpl_amount::Amount {
    if leg.is_xrp {
        rxrpl_amount::Amount::Xrp(leg.drops)
    } else {
        rxrpl_amount::Amount::Iou(leg.iou)
    }
}

/// Build a leg carrying `amount`, keeping `template`'s currency/issuer/kind.
fn amount_to_leg(amount: &rxrpl_amount::Amount, template: &Leg) -> Leg {
    let mut out = template.clone();
    match amount {
        rxrpl_amount::Amount::Xrp(d) => out.drops = *d,
        rxrpl_amount::Amount::Iou(v) => out.iou = *v,
    }
    out
}

/// The smaller of two like-typed legs.
fn leg_min(a: &Leg, b: &Leg) -> Leg {
    if leg_ge(a, b) { b.clone() } else { a.clone() }
}

/// `a >= b` for like-typed legs.
fn leg_ge(a: &Leg, b: &Leg) -> bool {
    if a.is_xrp {
        a.drops >= b.drops
    } else {
        a.iou >= b.iou
    }
}

/// True when `part` is below one ULP of a 16-digit `whole` (STAmount). 30000020
/// IOC 4F0BD7AA leftover 1e-14 EUR on a 50 EUR offer; mainnet stores 0 and reaps.
fn remaining_is_filled(remaining: &Leg, demand: &Leg) -> bool {
    remaining_is_filled_at(remaining, demand, -30)
}

/// Flow's 16-digit output total can satisfy a buy offer while the collapsed
/// per-liquidity-source subtraction leaves a sub-ULP IOU remainder. Clear that
/// remainder before scaling a resting offer; otherwise it can create a dust
/// offer from a fully delivered buy (105503215/CB282698).
fn flow_buy_remaining_output(
    remaining: &Leg,
    requested: &Leg,
    is_sell: bool,
    crossed: bool,
) -> Leg {
    if crossed && !is_sell && remaining_is_filled(remaining, requested) {
        leg_sub(remaining, remaining)
    } else {
        remaining.clone()
    }
}

fn remaining_is_filled_at(remaining: &Leg, demand: &Leg, max_exp: i32) -> bool {
    if remaining.is_zero() {
        return true;
    }
    if remaining.is_xrp || demand.is_xrp || demand.is_zero() {
        return false;
    }
    IOUAmount::divide(&remaining.iou, &demand.iou)
        .map(|q| q.exponent() <= max_exp)
        .unwrap_or(false)
}

fn leftover_is_dust(part: &Leg, whole: &Leg) -> bool {
    if part.is_zero() {
        return true;
    }
    if part.is_xrp || whole.is_xrp || whole.is_zero() {
        return false;
    }
    match IOUAmount::divide(&part.iou, &whole.iou) {
        Ok(q) => q.is_zero() || q.exponent() <= -16,
        Err(_) => false,
    }
}

/// Whether an IOU debit exhausts the source after Flow's 16-digit arithmetic.
///
/// The quality calculation can price a funds-limited take a few representable
/// ULPs below the holder's balance. rippled treats a relative remainder at the
/// STAmount precision boundary as fully consumed instead of persisting dust on
/// the trust line.
fn debit_drains_source(source_funds: &Leg, debit: &Leg) -> bool {
    if source_funds.is_zero() {
        return false;
    }
    if leg_ge(debit, source_funds) {
        return true;
    }
    let remainder = leftover_leg(source_funds, debit, true);
    let Ok(relative) = IOUAmount::divide(&remainder.iou, &source_funds.iou) else {
        return false;
    };
    let precision = IOUAmount::from_parts(1_000_000_000_000_000, -30, false)
        .expect("1e-15 is a canonical IOU amount");
    relative <= precision
}

/// A self-payment's complete offer take spends its exact TakerPays. Keep a
/// smaller source remainder even when it falls below the dust threshold
/// (105502095 / TX16). OfferCreate retains its FlowCross drain behavior.
fn book_payment_dust_drain(full_take: bool, self_payment: bool) -> bool {
    !self_payment || !full_take
}

fn hop_out_done(remaining: &Leg, demand: &Leg) -> bool {
    remaining.is_zero() || remaining_is_filled(remaining, demand)
}

/// `a - b` for like-typed legs (same currency).
fn leg_sub(a: &Leg, b: &Leg) -> Leg {
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops - b.drops;
    } else {
        out.iou = IOUAmount::sub(&a.iou, &b.iou).unwrap_or(IOUAmount::ZERO);
    }
    out
}

/// `a - b` for like-typed legs, rounding the IOU result to nearest (modern
/// Flow / `Number` semantics) instead of the legacy truncating [`leg_sub`].
/// XRP drops are exact integers, so the two agree on the native side.
fn leg_sub_round(a: &Leg, b: &Leg) -> Leg {
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops - b.drops;
    } else {
        out.iou = IOUAmount::add_round(&a.iou, &b.iou.negate()).unwrap_or(IOUAmount::ZERO);
    }
    out
}

/// A reverse-priced BookStep retains directional canonicalisation when it
/// records the unconsumed sides of the resting offer.
fn leg_sub_directional(a: &Leg, b: &Leg, mode: RoundingMode) -> Leg {
    use rxrpl_amount::number::{Number, RoundModeGuard};
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops - b.drops;
    } else {
        let _round = RoundModeGuard::new(mode);
        out.iou = Number::from_iou(&a.iou)
            .sub(&Number::from_iou(&b.iou))
            .to_iou();
    }
    out
}

/// Resting-offer leftover after a partial take. Pre-`fixUniversalNumber` STAmount
/// subtraction truncates; post-amendment Number rounds to nearest. Ledger
/// 30000008's leftover TakerPays is one ULP higher under truncation
/// (`2.512242529714367` vs `…366`).
fn leftover_leg(a: &Leg, b: &Leg, number_switchover: bool) -> Leg {
    if number_switchover {
        leg_sub_round(a, b)
    } else {
        leg_sub(a, b)
    }
}

/// Move the taker's input to the offer owner: XRP via balances, IOU via the
/// grossed trust-line debit / net credit.
#[derive(Clone, Copy)]
struct PayInOptions {
    skip_taker_debit: bool,
    allow_dust_drain: bool,
    gross_override: Option<IOUAmount>,
}

fn pay_in(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    amount: &Leg,
    round: bool,
    options: PayInOptions,
) -> Result<(), TransactionResult> {
    if amount.is_xrp {
        if !options.skip_taker_debit {
            let bal = helpers::get_balance(taker_acct) as i64 - amount.drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedOffer);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
        return credit_xrp(ctx, owner, amount.drops);
    }
    if !options.skip_taker_debit && taker != &amount.issuer {
        // Redeeming an IOU directly to its issuer carries no transfer fee.
        // Likewise, an issuer paying its own IOU creates value without a
        // self-line debit. Only a third-party-to-third-party transfer is
        // grossed by the issuer's TransferRate.
        let rate = if owner == &amount.issuer {
            IOUAmount::from_parts(1_000_000_000, -9, false).unwrap()
        } else {
            transfer_rate(ctx, &amount.issuer)
        };
        let gross = options
            .gross_override
            .unwrap_or_else(|| grossed(&amount.iou, &rate));
        let source_funds = owner_funds_leg(ctx, taker, amount);
        let mut gross_leg = amount.clone();
        gross_leg.iou = gross;
        let drains_source =
            options.allow_dust_drain && debit_drains_source(&source_funds, &gross_leg);
        credit_line(
            ctx,
            taker,
            &amount.issuer,
            &amount.currency,
            &gross.negate(),
            round,
        )?;
        if drains_source {
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                taker,
                &amount.issuer,
                &amount.currency,
                &Number::ZERO,
            )?;
        }
        // Flow's rippleCredit immediately removes a default trust line once
        // the source debit drains it to zero. Besides releasing the reserve,
        // this updates both owner directories and threads the issuer account.
        crate::handlers::trust_set::maybe_delete_drained_trust_line(
            ctx,
            taker,
            taker_acct,
            &amount.issuer,
            &amount.currency,
        )?;
    }
    if owner == &amount.issuer {
        return Ok(());
    }
    let tl_key = keylet::trust_line(owner, &amount.issuer, &amount.currency);
    if ctx.view.read(&tl_key).is_none() {
        let owner_key = keylet::account(owner);
        let ob = ctx
            .view
            .read(&owner_key)
            .ok_or(TransactionResult::TefInternal)?;
        let mut oacct: Value =
            serde_json::from_slice(&ob).map_err(|_| TransactionResult::TefInternal)?;
        create_iou_trust_line(ctx, owner, &mut oacct, &amount.issuer, &amount.currency)?;
        let nb = serde_json::to_vec(&oacct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(owner_key, nb)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    credit_line(
        ctx,
        owner,
        &amount.issuer,
        &amount.currency,
        &amount.iou,
        round,
    )
}

/// Settle a Payment BookStep whose input transfer fee is accounted separately
/// from the resting offer amount. `net` reduces TakerPays and is credited to the
/// maker; `gross` is charged to the source and consumes its SendMax/funds.
#[allow(clippy::too_many_arguments)]
fn pay_in_book_payment(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    net: &Leg,
    gross: &Leg,
    round: bool,
    skip_taker_debit: bool,
) -> Result<(), TransactionResult> {
    debug_assert!(!net.is_xrp && !gross.is_xrp);
    debug_assert_eq!(net.currency, gross.currency);
    debug_assert_eq!(net.issuer, gross.issuer);

    if !skip_taker_debit && taker != &net.issuer {
        credit_line(
            ctx,
            taker,
            &net.issuer,
            &net.currency,
            &gross.iou.negate(),
            round,
        )?;
    }
    pay_in(
        ctx,
        taker,
        taker_acct,
        owner,
        net,
        round,
        PayInOptions {
            skip_taker_debit: true,
            allow_dust_drain: true,
            gross_override: None,
        },
    )
}

/// Move the offer owner's output to `recipient`: XRP via balances, IOU via the
/// grossed owner debit / net recipient credit (the difference is the burned
/// fee). When `recipient` is the taker, its XRP credit goes through `taker_acct`
/// (the caller's working copy) so the apply's final account write does not
/// clobber it; a distinct recipient (cross-currency payment to another account)
/// is credited through the view instead.
///
/// This is the legacy Taker (OfferCreate crossing) path: `amount` is the NET
/// the recipient receives and the offer's TakerGets reduction, while the owner
/// is unconditionally debited the grossed-up amount (`amount * transfer_rate`).
/// The cross-currency Payment path uses [`pay_out_gross`] instead, where the
/// fee is charged out of a gross input and skipped when the issuer is a party.
fn pay_out(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    recipient: &AccountId,
    amount: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    if amount.is_xrp {
        credit_xrp(ctx, owner, -amount.drops)?;
        if recipient == taker {
            helpers::set_balance(
                taker_acct,
                helpers::get_balance(taker_acct) + amount.drops as u64,
            );
        } else {
            credit_xrp(ctx, recipient, amount.drops)?;
        }
        return Ok(());
    }
    let rate = transfer_rate(ctx, &amount.issuer);
    let gross = grossed(&amount.iou, &rate);
    // When the offer owner is itself the output issuer it has no trust line on
    // its side to debit — it issues its own IOU directly (rippled's rippleCredit
    // skips the issuer's self-line). Debiting the nonexistent self-line otherwise
    // fails the whole crossing with tecPATH_DRY and the taker's offer rests
    // uncrossed instead of consuming the issuer's resting offers.
    if owner != &amount.issuer {
        credit_line(
            ctx,
            owner,
            &amount.issuer,
            &amount.currency,
            &gross.negate(),
            round,
        )?;
        // FlowCross settles the maker's IOU debit through rippleCredit. A
        // default trust line drained by that debit is deleted before the
        // exhausted offer is reaped, releasing both owner reserves.
        let owner_key = keylet::account(owner);
        let owner_bytes = ctx
            .view
            .read(&owner_key)
            .ok_or(TransactionResult::TefInternal)?;
        let mut owner_acct: Value =
            serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
        if crate::handlers::trust_set::maybe_delete_drained_trust_line(
            ctx,
            owner,
            &mut owner_acct,
            &amount.issuer,
            &amount.currency,
        )? {
            let owner_data =
                serde_json::to_vec(&owner_acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(owner_key, owner_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
    }
    // FlowCross settles the taker's output through rippleCredit, which creates
    // a zero-limit trust line on demand when this is the first delivery of the
    // IOU. Without it the first maker debit is rolled back as tecPATH_DRY and
    // an IOC offer can be deferred until the book has already been consumed.
    if recipient != &amount.issuer {
        let tl_key = keylet::trust_line(recipient, &amount.issuer, &amount.currency);
        if ctx.view.read(&tl_key).is_none() {
            if recipient == taker {
                create_iou_trust_line(
                    ctx,
                    recipient,
                    taker_acct,
                    &amount.issuer,
                    &amount.currency,
                )?;
            } else {
                let recipient_key = keylet::account(recipient);
                let bytes = ctx
                    .view
                    .read(&recipient_key)
                    .ok_or(TransactionResult::TefInternal)?;
                let mut recipient_acct: Value =
                    serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
                create_iou_trust_line(
                    ctx,
                    recipient,
                    &mut recipient_acct,
                    &amount.issuer,
                    &amount.currency,
                )?;
                let bytes = serde_json::to_vec(&recipient_acct)
                    .map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(recipient_key, bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        }
    }
    credit_line(
        ctx,
        recipient,
        &amount.issuer,
        &amount.currency,
        &amount.iou,
        round,
    )
}

/// Move the offer owner's output to `recipient` and return the NET delivered
/// (what `recipient` actually receives). Used by the cross-currency Payment
/// path, where `amount` is the GROSS the offer provides — exactly the offer's
/// TakerGets reduction and the owner's debit. The output issuer's transfer fee
/// is charged out of that gross unless the caller waives it: the recipient
/// receives `amount / transfer_rate` with the caller's execution-path rounding,
/// and the difference is the burned fee. rippled skips the fee whenever the
/// issuer is itself a party (owner or recipient), so the net then equals the
/// gross. XRP carries no transfer fee, so
/// net == gross there. When `recipient` is the taker, its XRP credit goes
/// through `taker_acct` (the caller's working copy) so the apply's final account
/// write does not clobber it; a distinct recipient (cross-currency payment to
/// another account) is credited through the view instead.
#[derive(Clone, Copy)]
enum TransferFeeRounding {
    Down,
    BookStepRatio,
    Nearest,
}

fn book_output_fee_rounding(budget_binds: bool, skip_output_credit: bool) -> TransferFeeRounding {
    // Input-limited terminal BookSteps retain ratio digits whether the book
    // was reached from a preceding step (105502369/TX41) or directly from an
    // XRP source (105502605/TX24). Demand-limited hops keep truncation.
    if budget_binds && !skip_output_credit {
        TransferFeeRounding::BookStepRatio
    } else {
        TransferFeeRounding::Down
    }
}

/// Positive IOU `mulRatio(gross, QUALITY_ONE, rate, false)` for a BookStep
/// output. rippled retains ratio digits up to its signed 64-bit intermediate
/// before IOUAmount normalizes through Number. Truncating the quotient at the
/// original 16-digit mantissa loses the guard digits needed for that rounding.
fn book_output_net_ratio(gross: &IOUAmount, rate: u32) -> IOUAmount {
    if gross.is_zero() {
        return *gross;
    }
    // This caller has a positive canonical IOU (mantissa < 10^16) and
    // transfer_rate_value returns a fee rate > 10^9. The unscaled quotient is
    // below 10^16; mulRatio's decimal expansion reaches at most 10^18,
    // which fits the signed 64-bit intermediate used by rippled.
    debug_assert!(!gross.sign_bit());
    debug_assert!(rate > 1_000_000_000);
    let denominator = u128::from(rate);
    let product = u128::from(gross.mantissa()) * 1_000_000_000;
    let mut quotient = product / denominator;
    let mut remainder = product % denominator;
    let mut exponent = gross.exponent();
    if remainder != 0 {
        // IOUAmount.cpp mulRatio: kFl64 - ceil(log10(quotient)). The
        // intermediate is truncated only after filling this available room.
        let ceil_log10 = (quotient - 1).ilog10() + 1;
        let room = 18u32.saturating_sub(ceil_log10);
        if room > 0 {
            let scale = 10u128.pow(room);
            quotient *= scale;
            remainder *= scale;
            quotient += remainder / denominator;
            exponent -= room as i32;
        }
    }
    debug_assert!(quotient <= i64::MAX as u128);
    rxrpl_amount::number::Number::new(false, quotient as u64, exponent).to_iou()
}

#[allow(clippy::too_many_arguments)]
fn pay_out_gross(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    recipient: &AccountId,
    amount: &Leg,
    round: bool,
    owner_rounding: BalanceRounding,
    recipient_rounding: BalanceRounding,
    skip_recipient_credit: bool,
    fee_rounding: TransferFeeRounding,
    waive_transfer_fee: bool,
) -> Result<Leg, TransactionResult> {
    if amount.is_xrp {
        credit_xrp(ctx, owner, -amount.drops)?;
        if !skip_recipient_credit {
            if recipient == taker {
                helpers::set_balance(
                    taker_acct,
                    helpers::get_balance(taker_acct) + amount.drops as u64,
                );
            } else {
                credit_xrp(ctx, recipient, amount.drops)?;
            }
        }
        return Ok(amount.clone());
    }
    // The fee applies only when neither party is the issuer (rippled charges no
    // transfer fee on issuance/redemption). When it applies, the recipient nets
    // `gross / rate` using the selected ratio rounding; otherwise net == gross.
    // For an intermediate hop (`skip_recipient_credit`) the output funds the next book, so
    // there is no fee-charged recipient and the net carried forward is the gross.
    let fee_applies = !waive_transfer_fee
        && !skip_recipient_credit
        && amount.issuer != *owner
        && amount.issuer != *recipient;
    let net = if fee_applies {
        let rate = transfer_rate(ctx, &amount.issuer);
        let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
        if rate > one {
            match fee_rounding {
                TransferFeeRounding::Down => amount
                    .iou
                    .mul_ratio(
                        1_000_000_000,
                        transfer_rate_value(ctx, &amount.issuer),
                        false,
                    )
                    .unwrap_or(amount.iou),
                TransferFeeRounding::BookStepRatio => {
                    book_output_net_ratio(&amount.iou, transfer_rate_value(ctx, &amount.issuer))
                }
                TransferFeeRounding::Nearest if round => {
                    let _scale = rxrpl_amount::number::MantissaScaleGuard::new(
                        rxrpl_amount::number::MantissaScale::Large,
                    );
                    rxrpl_amount::number::Number::from_iou(&amount.iou)
                        .div(&rxrpl_amount::number::Number::from_iou(&rate))
                        .to_iou()
                }
                TransferFeeRounding::Nearest => rxrpl_amount::number::Number::from_iou(&amount.iou)
                    .div(&rxrpl_amount::number::Number::from_iou(&rate))
                    .to_iou(),
            }
        } else {
            amount.iou
        }
    } else {
        amount.iou
    };
    // Debit the owner the full gross it provides (the offer's TakerGets move).
    // When a party to the delivery is itself the output issuer it has no trust
    // line on its side to move — it issues (owner) or redeems (recipient) its own
    // IOU directly (rippled's rippleCredit skips the issuer self-line). Debiting a
    // nonexistent self-line otherwise fails the crossing with tecPATH_DRY, exactly
    // as `pay_out` did for issuer-owned offers before the guard.
    if amount.issuer != *owner {
        credit_line_with_rounding(
            ctx,
            owner,
            &amount.issuer,
            &amount.currency,
            &amount.iou.negate(),
            owner_rounding,
        )?;
        // BookPaymentStep settles the maker's TakerGets through rippleCredit.
        // If this redemption drains a default trust line, trustDelete runs
        // immediately: release its reserve, unlink it from both owner
        // directories, and thread the issuer account.
        let owner_key = keylet::account(owner);
        let owner_bytes = ctx
            .view
            .read(&owner_key)
            .ok_or(TransactionResult::TefInternal)?;
        let mut owner_acct: Value =
            serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
        if crate::handlers::trust_set::maybe_delete_drained_trust_line(
            ctx,
            owner,
            &mut owner_acct,
            &amount.issuer,
            &amount.currency,
        )? {
            let owner_data =
                serde_json::to_vec(&owner_acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(owner_key, owner_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
    }
    if !skip_recipient_credit && amount.issuer != *recipient {
        credit_line_with_rounding(
            ctx,
            recipient,
            &amount.issuer,
            &amount.currency,
            &net,
            recipient_rounding,
        )?;
    }
    let mut net_leg = amount.clone();
    net_leg.iou = net;
    Ok(net_leg)
}

/// `amount * rate` using rippled's non-rounding STAmount multiply.
fn grossed(amount: &IOUAmount, rate: &IOUAmount) -> IOUAmount {
    match rxrpl_amount::Amount::multiply(
        &rxrpl_amount::Amount::Iou(*amount),
        &rxrpl_amount::Amount::Iou(*rate),
        false,
    ) {
        Ok(rxrpl_amount::Amount::Iou(v)) => v,
        _ => *amount,
    }
}

/// BookStep's input transfer fee uses rippled `mulRatio(..., roundUp=true)`.
/// Its signed 64-bit intermediate retains available guard digits before IOU
/// normalization;
/// a nonzero final remainder then advances the normalized amount once more.
/// At 105503160/TX21, BTC 0.001079810327359199 at rate 1.0015 first
/// normalizes to gross ...238 and the remaining ratio fraction makes ...239.
/// STAmount multiplication and the simpler IOU mul_ratio both yield ...238.
fn gross_amm_book_input(net: &IOUAmount, rate: u32) -> IOUAmount {
    if net.is_zero() || rate <= 1_000_000_000 || net.sign_bit() {
        return *net;
    }
    let denominator = 1_000_000_000u128;
    let product = u128::from(net.mantissa()) * u128::from(rate);
    let mut quotient = product / denominator;
    let mut remainder = product % denominator;
    let mut exponent = net.exponent();
    if remainder != 0 {
        // IOUAmount.cpp: roomToGrow = floor(log10(INT64_MAX)) - ceil(log10(low)).
        let ceil_log10 = (quotient - 1).ilog10() + 1;
        let room = 18u32.saturating_sub(ceil_log10);
        if room > 0 {
            let scale = 10u128.pow(room);
            quotient *= scale;
            remainder *= scale;
            quotient += remainder / denominator;
            remainder %= denominator;
            exponent -= room as i32;
        }
    }
    debug_assert!(quotient <= i64::MAX as u128);
    let normalized = rxrpl_amount::number::Number::new(false, quotient as u64, exponent).to_iou();
    if remainder == 0 {
        return normalized;
    }
    IOUAmount::from_parts(normalized.mantissa() + 1, normalized.exponent(), false)
        .unwrap_or(normalized)
}

fn gross_book_output_cap_nets_short(gross: &IOUAmount, demand: &IOUAmount, rate: u32) -> bool {
    gross
        .mul_ratio(1_000_000_000, rate, false)
        .is_ok_and(|net| net < *demand)
}

/// Convert a BookPaymentStep's remaining gross source budget to the net input
/// that can be offered to the current CLOB offer.
fn net_book_payment_input(gross: &Leg, rate: u32) -> Leg {
    if gross.is_xrp || rate == 1_000_000_000 {
        return gross.clone();
    }
    let mut net = gross.clone();
    net.iou = gross
        .iou
        .mul_ratio(1_000_000_000, rate, false)
        .unwrap_or(IOUAmount::ZERO);
    net
}

/// OfferCreate tracks the unfilled offer in net units and Flow's source
/// allowance in gross units. A direct BookStep may spend only the smaller net
/// amount after the input issuer's transfer fee is removed.
fn net_offer_input_budget(remaining_net: &Leg, remaining_gross: &Leg, rate: u32) -> Leg {
    leg_min(
        remaining_net,
        &net_book_payment_input(remaining_gross, rate),
    )
}

/// Gross the net TakerPays amount for BookPaymentStep source accounting. If the
/// gross source budget binds, rippled consumes it exactly after limiting the
/// offer with the corresponding round-down net amount.
fn gross_book_payment_input(net: &Leg, remaining_gross: &Leg, rate: u32, binds: bool) -> Leg {
    if net.is_xrp || rate == 1_000_000_000 {
        return net.clone();
    }
    if binds {
        return remaining_gross.clone();
    }
    let mut gross = net.clone();
    gross.iou = net
        .iou
        .mul_ratio(rate, 1_000_000_000, true)
        .unwrap_or(net.iou);
    leg_min(&gross, remaining_gross)
}

fn reap_walk_offer(
    ctx: &mut ApplyContext<'_>,
    owner: &AccountId,
    taker: &AccountId,
    taker_acct: &mut Value,
    offer_key: &rxrpl_primitives::Hash256,
    book_dir: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    reap_offer(ctx, owner, offer_key, book_dir)?;
    if owner == taker {
        helpers::adjust_owner_count(taker_acct, -1);
    }
    Ok(())
}

pub(crate) fn reap_offer(
    ctx: &mut ApplyContext<'_>,
    owner: &AccountId,
    offer_key: &rxrpl_primitives::Hash256,
    book_dir: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    crate::owner_dir::remove_from_owner_dir(ctx.view, owner, offer_key)?;
    remove_from_book_dir(ctx.view, book_dir, offer_key)?;
    let _ = ctx.view.erase(offer_key);
    let owner_key = keylet::account(owner);
    if let Some(b) = ctx.view.read(&owner_key) {
        if let Ok(mut acct) = serde_json::from_slice::<Value>(&b) {
            helpers::adjust_owner_count(&mut acct, -1);
            if let Ok(nb) = serde_json::to_vec(&acct) {
                let _ = ctx.view.update(owner_key, nb);
            }
        }
    }
    Ok(())
}

/// Extract a 20-byte currency code and issuer AccountId from a TakerPays /
/// TakerGets value. XRP — represented as a bare drops string — maps to
/// all-zero currency and issuer.
fn currency_and_issuer(amount: &Value) -> ([u8; 20], AccountId) {
    if amount.is_string() {
        return ([0u8; 20], AccountId::from([0u8; 20]));
    }
    let currency_str = amount
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut currency = [0u8; 20];
    if currency_str.len() == 3 {
        let b = currency_str.as_bytes();
        currency[12] = b[0];
        currency[13] = b[1];
        currency[14] = b[2];
    } else if currency_str.len() == 40 {
        if let Ok(decoded) = hex::decode(currency_str) {
            if decoded.len() == 20 {
                currency.copy_from_slice(&decoded);
            }
        }
    }
    let issuer = amount
        .get("issuer")
        .and_then(|v| v.as_str())
        .and_then(|s| decode_account_id(s).ok())
        .unwrap_or_else(|| AccountId::from([0u8; 20]));
    (currency, issuer)
}

/// PermissionedDEX domain membership check for a domain-scoped offer.
///
/// rippled: an offer carrying a `DomainID` is authorised only for members of
/// that domain. The `DomainID` is the `PermissionedDomain` object's ledger
/// index; membership requires holding an *accepted* credential whose
/// `(Issuer, CredentialType)` matches one of the domain's `AcceptedCredentials`.
fn check_domain_membership(
    ctx: &PreclaimContext<'_>,
    trader_id: &AccountId,
    domain_id: &str,
) -> Result<(), TransactionResult> {
    use std::str::FromStr;
    let domain_key = rxrpl_primitives::Hash256::from_str(domain_id)
        .map_err(|_| TransactionResult::TemMalformed)?;
    let domain_bytes = ctx
        .view
        .read(&domain_key)
        .ok_or(TransactionResult::TecNoPermission)?; // domain must exist
    let domain: Value =
        serde_json::from_slice(&domain_bytes).map_err(|_| TransactionResult::TemMalformed)?;
    let accepted = domain
        .get("AcceptedCredentials")
        .and_then(|v| v.as_array())
        .ok_or(TransactionResult::TecNoPermission)?;

    for entry in accepted {
        // The SLE wraps each pair as `{"Credential": {"Issuer", "CredentialType"}}`.
        let inner = entry.get("Credential").unwrap_or(entry);
        let (Some(issuer_str), Some(ct_str)) = (
            inner.get("Issuer").and_then(|v| v.as_str()),
            inner.get("CredentialType").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Ok(issuer_id) = decode_account_id(issuer_str) else {
            continue;
        };
        // CredentialType is stored hex-encoded; the keylet hashes the raw bytes.
        let ct_bytes = hex::decode(ct_str).unwrap_or_else(|_| ct_str.as_bytes().to_vec());
        let cred_key = keylet::credential(trader_id, &issuer_id, &ct_bytes);
        if let Some(cred_bytes) = ctx.view.read(&cred_key) {
            if let Ok(cred) = serde_json::from_slice::<Value>(&cred_bytes) {
                // The credential must be accepted (lsfAccepted); an issued-but-not-
                // accepted credential does not confer membership.
                if helpers::get_flags(&cred) & crate::handlers::credentials::LSF_ACCEPTED != 0 {
                    return Ok(());
                }
            }
        }
    }

    Err(TransactionResult::TecNoPermission) // not a domain member
}

/// Test whether `owner` currently holds the `TakerGets` amount in full.
/// XRP: AccountRoot.Balance ≥ amount. IOU: holder-side trust line balance ≥ value.
/// The owner's spendable funds in the offer's `gets` currency, as a `Leg`.
/// A zero result means the offer is unfunded (the caller reaps it); otherwise
/// the fill is clamped to this amount (rippled fills against owner funds rather
/// than requiring the whole offer to be funded). XRP uses the raw balance; an
/// IOU issuer can always issue its own currency (not a binding constraint); an
/// IOU holder is bounded by its trust-line balance.
/// rippled `isDeepFrozen(view, account, asset)`: a line the account holds for
/// `asset` is deep-frozen when either side set the deep-freeze flag (a
/// deep-frozen line can neither receive nor send). XRP and the issuer's own
/// asset are never deep-frozen.
fn is_deep_frozen(ctx: &mut ApplyContext<'_>, account: &AccountId, asset: &Leg) -> bool {
    const LSF_LOW_DEEP_FREEZE: u64 = 0x0200_0000;
    const LSF_HIGH_DEEP_FREEZE: u64 = 0x0400_0000;
    if asset.is_xrp || &asset.issuer == account {
        return false;
    }
    let Some(tl_bytes) =
        ctx.view
            .read(&keylet::trust_line(account, &asset.issuer, &asset.currency))
    else {
        return false;
    };
    let Ok(tl) = serde_json::from_slice::<Value>(&tl_bytes) else {
        return false;
    };
    let flags = tl.get("Flags").and_then(Value::as_u64).unwrap_or(0);
    flags & (LSF_LOW_DEEP_FREEZE | LSF_HIGH_DEEP_FREEZE) != 0
}

fn offer_cannot_pay(funds: &Leg, offer_out: &Leg, flags: u64, is_self: bool) -> bool {
    if funds.is_zero() {
        return true;
    }
    is_self && flags & LSF_SELL != 0 && !leg_ge(funds, offer_out)
}

fn reap_own_unfunded_inverse_tip(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    inverse_book: &Hash256,
    taker_pays: &Value,
    taker_gets: &Value,
) -> Result<(), TransactionResult> {
    let prefix = inverse_book.as_bytes()[0..24].to_vec();
    let mut probe = book_dir_with_quality(inverse_book, 0);
    let mut best_dir: Option<Hash256> = None;
    while let Some(dir_key) = ctx.view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != prefix[..] {
            break;
        }
        probe = dir_key;
        let Some(bytes) = ctx.view.read(&dir_key) else {
            continue;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let empty = dir
            .get("Indexes")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if empty {
            continue;
        }
        best_dir = Some(dir_key);
        break;
    }
    let Some(best_dir) = best_dir else {
        return Ok(());
    };
    let Some(out_leg) = Leg::parse(taker_pays) else {
        return Ok(());
    };
    let Some(in_leg) = Leg::parse(taker_gets) else {
        return Ok(());
    };
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let passive = ctx.tx.get("Flags").and_then(Value::as_u64).unwrap_or(0) & TF_PASSIVE != 0;
    let threshold =
        flow_crossing_threshold(ctx, taker, &in_leg, &out_leg, number_switchover, passive);
    let best_quality = u64::from_be_bytes(best_dir.as_bytes()[24..32].try_into().unwrap());
    // Before fixReducedOffersV1, the active path advanced through an
    // underfunded own tip before its quality gate (ledger 30000048). The fixed
    // path gates first; passive offers have always tightened the threshold.
    let enforce_quality = passive || ctx.rules.enabled(&feature_id("fixReducedOffersV1"));
    if !own_tip_can_be_reaped(best_quality, threshold, enforce_quality) {
        return Ok(());
    }
    let entries = crate::owner_dir::collect_owner_dir_entries(ctx.view, taker);
    for s in entries {
        let Ok(key) = s.parse::<Hash256>() else {
            continue;
        };
        let Some(bytes) = ctx.view.read(&key) else {
            continue;
        };
        let Ok(offer) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
            continue;
        }
        let flags = offer.get("Flags").and_then(Value::as_u64).unwrap_or(0);
        if flags & LSF_SELL == 0 {
            continue;
        }
        let Some(book) = offer
            .get("BookDirectory")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Hash256>().ok())
        else {
            continue;
        };
        if book != best_dir {
            continue;
        }
        let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
            continue;
        };
        let funds = owner_funds_leg(ctx, taker, &offer_out);
        if !leg_ge(&funds, &offer_out) {
            reap_offer(ctx, taker, &key, &book)?;
            helpers::adjust_owner_count(taker_acct, -1);
        }
    }
    Ok(())
}

/// After an exhausted take, reap the maker's other lsfSell offers of the same
/// asset that can no longer deliver TakerGets (30000060 D995194A seq 258).
fn reap_maker_short_sells(
    ctx: &mut ApplyContext<'_>,
    owner: &AccountId,
    sold: &Leg,
) -> Result<(), TransactionResult> {
    let entries = crate::owner_dir::collect_owner_dir_entries(ctx.view, owner);
    for s in entries {
        let Ok(key) = s.parse::<Hash256>() else {
            continue;
        };
        let Some(bytes) = ctx.view.read(&key) else {
            continue;
        };
        let Ok(offer) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
            continue;
        }
        let flags = offer.get("Flags").and_then(Value::as_u64).unwrap_or(0);
        if flags & LSF_SELL == 0 {
            continue;
        }
        let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
            continue;
        };
        if offer_out.is_xrp != sold.is_xrp
            || offer_out.currency != sold.currency
            || offer_out.issuer != sold.issuer
        {
            continue;
        }
        let funds = owner_funds_leg(ctx, owner, &offer_out);
        if !funds.is_zero() && leg_ge(&funds, &offer_out) {
            continue;
        }
        let Some(book) = offer
            .get("BookDirectory")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Hash256>().ok())
        else {
            continue;
        };
        reap_offer(ctx, owner, &key, &book)?;
    }
    Ok(())
}

fn owner_funds_leg(ctx: &mut ApplyContext<'_>, owner: &AccountId, gets: &Leg) -> Leg {
    // PaymentSandbox keeps the highest owner count reached during a crossing.
    // Deleting an exhausted offer must not immediately release its XRP reserve
    // for another offer from the same maker in this transaction.
    let reserve_count_floor = if gets.is_xrp {
        ctx.view
            .read_parent(&keylet::account(owner))
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|a| helpers::get_owner_count(&a))
            .unwrap_or(0)
    } else {
        0
    };
    owner_funds_leg_with(ctx.fees, owner, gets, reserve_count_floor, |key| {
        ctx.view.read(key)
    })
}

fn original_owner_funds_leg(ctx: &ApplyContext<'_>, owner: &AccountId, gets: &Leg) -> Leg {
    owner_funds_leg_with(ctx.fees, owner, gets, 0, |key| ctx.view.read_parent(key))
}

fn owner_funds_leg_with(
    fees: &crate::fees::FeeSettings,
    owner: &AccountId,
    gets: &Leg,
    reserve_count_floor: u32,
    mut read: impl FnMut(&Hash256) -> Option<Vec<u8>>,
) -> Leg {
    let zero = Leg {
        is_xrp: gets.is_xrp,
        drops: 0,
        iou: IOUAmount::ZERO,
        currency: gets.currency,
        issuer: gets.issuer,
    };
    if gets.is_xrp {
        let key = keylet::account(owner);
        let acct = read(&key).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let bal = acct
            .as_ref()
            .and_then(|a| {
                a.get("Balance")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);
        // rippled `accountFunds` for XRP is `xrpLiquid(owner, 0)` = balance minus
        // the account reserve, clamped at zero: an offer can only sell the owner's
        // spendable XRP, not the reserve-locked portion. Using the raw balance let
        // a reserve-constrained offer over-fill (execute instead of being reaped).
        let owner_count = acct
            .as_ref()
            .map(helpers::get_owner_count)
            .unwrap_or(0)
            .max(reserve_count_floor);
        let liquid = bal - fees.account_reserve(owner_count) as i64;
        if liquid <= 0 {
            return zero;
        }
        return Leg {
            is_xrp: true,
            drops: liquid,
            iou: IOUAmount::ZERO,
            currency: gets.currency,
            issuer: gets.issuer,
        };
    }
    if &gets.issuer == owner {
        return gets.clone();
    }
    let tl_key = keylet::trust_line(owner, &gets.issuer, &gets.currency);
    let Some(tl_bytes) = read(&tl_key) else {
        return zero;
    };
    let Ok(tl) = serde_json::from_slice::<Value>(&tl_bytes) else {
        return zero;
    };
    let raw_str = tl
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let raw_f: f64 = raw_str.parse().unwrap_or(0.0);
    // Balance is stored from the low account's perspective; the holder's view
    // flips sign when the issuer is the low account.
    let issuer_is_low = gets.issuer.as_bytes() < owner.as_bytes();
    let holder_positive = if issuer_is_low {
        raw_f < 0.0
    } else {
        raw_f > 0.0
    };
    if !holder_positive {
        return zero;
    }
    // rippled `accountFundsHelper` prices a maker's IOU with
    // `ZeroIfFrozen`/`ZeroIfUnauthorized`: a maker cannot deliver an IOU it
    // can't move, so the offer is found unfunded (0) and reaped. Mirror that —
    // the issuer global-froze, or froze this line, or requires auth and hasn't
    // authorized it.
    const LSF_LOW_FREEZE: u64 = 0x0040_0000;
    const LSF_HIGH_FREEZE: u64 = 0x0080_0000;
    const LSF_LOW_AUTH: u64 = 0x0004_0000;
    const LSF_HIGH_AUTH: u64 = 0x0008_0000;
    const LSF_GLOBAL_FREEZE: u64 = 0x0040_0000;
    const LSF_REQUIRE_AUTH: u64 = 0x0004_0000;
    let tl_flags = tl.get("Flags").and_then(Value::as_u64).unwrap_or(0);
    let issuer_flags = read(&keylet::account(&gets.issuer))
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|a| a.get("Flags").and_then(Value::as_u64))
        .unwrap_or(0);
    let line_frozen = if issuer_is_low {
        tl_flags & LSF_LOW_FREEZE != 0
    } else {
        tl_flags & LSF_HIGH_FREEZE != 0
    };
    if line_frozen || issuer_flags & LSF_GLOBAL_FREEZE != 0 {
        return zero;
    }
    if issuer_flags & LSF_REQUIRE_AUTH != 0 {
        let authorized = if issuer_is_low {
            tl_flags & LSF_LOW_AUTH != 0
        } else {
            tl_flags & LSF_HIGH_AUTH != 0
        };
        if !authorized {
            return zero;
        }
    }
    let mag = raw_str.trim_start_matches('-');
    Leg {
        is_xrp: false,
        drops: 0,
        iou: IOUAmount::from_decimal_string(mag).unwrap_or(IOUAmount::ZERO),
        currency: gets.currency,
        issuer: gets.issuer,
    }
}

/// Remove an offer hash from a book directory (inverse of `add_to_dir`).
fn remove_from_book_dir(
    view: &mut dyn crate::view::apply_view::ApplyView,
    book_root: &rxrpl_primitives::Hash256,
    offer_key: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    let entry_hex = offer_key.to_string();
    let mut page = 0u64;
    loop {
        let page_key = if page == 0 {
            *book_root
        } else {
            keylet::dir_node(book_root, page)
        };
        let bytes = match view.read(&page_key) {
            Some(b) => b,
            None => return Ok(()),
        };
        let mut dir: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let next_page = dir.get("IndexNext").and_then(|v| v.as_u64()).unwrap_or(0);
        let removed = if let Some(indexes) = dir.get_mut("Indexes").and_then(|v| v.as_array_mut()) {
            let original = indexes.len();
            indexes.retain(|v| v.as_str() != Some(entry_hex.as_str()));
            indexes.len() != original
        } else {
            false
        };
        if removed {
            let empty = dir
                .get("Indexes")
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(true);
            if empty && page == 0 && next_page == 0 {
                let _ = view.erase(&page_key);
            } else {
                let new_bytes =
                    serde_json::to_vec(&dir).map_err(|_| TransactionResult::TefInternal)?;
                let _ = view.update(page_key, new_bytes);
            }
            return Ok(());
        }
        if next_page == 0 {
            return Ok(());
        }
        page = next_page;
    }
}

// ============================================================================
// Legacy Taker offer-crossing (pre-`FlowCross` era).
//
// rippled runs two offer-crossing engines gated on the `FlowCross` amendment
// (`CreateOffer::cross`): the modern Flow/`BookStep` path (`cross_offers` above)
// when it is enabled, and the legacy `BasicTaker`/`Taker` (`bridged_cross` /
// `direct_cross`) when it is not. Every mainnet ledger past 2018 has `FlowCross`
// enabled, so this arm is dead there; it exists for byte-exact replay of
// pre-`FlowCross` ledgers. The port mirrors rippled 2.2.0
// `Taker.cpp` / `CreateOffer.cpp`; the crux is that the legacy pricers round to
// NEAREST (`STAmount::multiply`/`divide`) rather than the strict-directional
// rounding the Flow path uses.
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TakerCrossType {
    XrpToIou,
    IouToXrp,
    IouToIou,
}

/// rippled `BasicTaker::Flow`: `order` = the net offer/taker amounts, `issuers` =
/// the grossed amounts moved at the issuer (used only for the pricing clamps —
/// settlement re-derives the fee inside `pay_in`/`pay_out`).
struct TakerFlow {
    order_in: Leg,
    order_out: Leg,
    issuers_in: Leg,
    issuers_out: Leg,
}

/// The unit transfer rate (1.0) as an `IOUAmount`.
fn parity_rate() -> IOUAmount {
    IOUAmount::from_parts(1_000_000_000, -9, false).unwrap()
}

/// rippled `BasicTaker::effective_rate`: the issuer's transfer fee applies only
/// when neither party is the issuer and the two parties differ (and it is not
/// XRP). Otherwise parity.
fn effective_rate(
    ctx: &ApplyContext<'_>,
    issuer: &AccountId,
    from: &AccountId,
    to: &AccountId,
) -> IOUAmount {
    if issuer == &AccountId::from([0u8; 20]) || from == to || from == issuer || to == issuer {
        return parity_rate();
    }
    transfer_rate(ctx, issuer)
}

/// A zero amount carrying `tmpl`'s asset (issue/kind).
fn zero_leg(tmpl: &Leg) -> Leg {
    let mut z = tmpl.clone();
    z.drops = 0;
    z.iou = IOUAmount::ZERO;
    z
}

/// `a * b` (round-to-nearest `STAmount::multiply`), result in `tmpl`'s asset.
fn mul_nearest(a: &Leg, b: &IOUAmount, tmpl: &Leg) -> Leg {
    rxrpl_amount::Amount::multiply(
        &leg_to_amount(a),
        &rxrpl_amount::Amount::Iou(*b),
        tmpl.is_xrp,
    )
    .map(|x| amount_to_leg(&x, tmpl))
    .unwrap_or_else(|_| zero_leg(tmpl))
}

/// `a / b` (round-to-nearest `STAmount::divide`), result in `tmpl`'s asset.
fn div_nearest(a: &Leg, b: &IOUAmount, tmpl: &Leg) -> Leg {
    rxrpl_amount::Amount::divide(
        &leg_to_amount(a),
        &rxrpl_amount::Amount::Iou(*b),
        tmpl.is_xrp,
    )
    .map(|x| amount_to_leg(&x, tmpl))
    .unwrap_or_else(|_| zero_leg(tmpl))
}

/// `a * rate` gross in `a`'s own asset (rippled `multiply(amount, rate)`).
fn gross_leg(a: &Leg, rate: &IOUAmount) -> Leg {
    mul_nearest(a, rate, a)
}

/// rippled `qual_mul(amount, quality, output)` = `min(multiply(amount, rate),
/// output)`: the input required for `amount` output at `rate` (= in/out),
/// clamped to `output`'s current value and issue.
fn qual_mul(amount: &Leg, rate: &IOUAmount, output: &Leg) -> Leg {
    leg_min(&mul_nearest(amount, rate, output), output)
}

/// rippled `qual_div(amount, quality, output)` = `min(divide(amount, rate),
/// output)`: the output deliverable for `amount` input at `rate`, clamped.
fn qual_div(amount: &Leg, rate: &IOUAmount, output: &Leg) -> Leg {
    leg_min(&div_nearest(amount, rate, output), output)
}

/// The larger of two like-typed legs.
fn leg_max(a: &Leg, b: &Leg) -> Leg {
    if leg_ge(a, b) { a.clone() } else { b.clone() }
}

/// `a > b` for like-typed legs.
fn leg_gt(a: &Leg, b: &Leg) -> bool {
    !leg_ge(b, a)
}

/// Whether this fill consumes the resting offer itself, as opposed to merely
/// exhausting the maker's currently available funds. The budget/demand
/// shortcut is valid only when the entire TakerGets side is funded.
fn full_offer_take(
    take_out: &Leg,
    avail_out: &Leg,
    remaining_in: &Leg,
    remaining_out: &Leg,
    offer_in: &Leg,
    offer_out: &Leg,
) -> bool {
    leg_ge(avail_out, offer_out)
        && (leg_ge(take_out, offer_out)
            || (leg_ge(remaining_in, offer_in) && leg_ge(remaining_out, offer_out)))
}

/// Pack an `IOUAmount` rate into a book-directory quality u64 (rippled
/// `Quality(uint64)` layout: `(exponent+100)<<56 | mantissa`).
fn pack_rate(rate: &IOUAmount) -> u64 {
    if rate.is_zero() {
        return 0;
    }
    let exp_biased = (rate.exponent() + 100) as u64;
    (exp_biased << 56) | rate.mantissa()
}

/// rippled `composed_quality(lhs, rhs)` = pack(`mulRound(lhs.rate(), rhs.rate(),
/// roundUp=true)`), the quality of two bridged offers chained through XRP. The
/// round-UP (worse) matches rippled's Quality.cpp:135 and keeps the bridge from
/// looking marginally better than it is at a band boundary.
fn composed_quality(q1: u64, q2: u64) -> u64 {
    compose_packed_rates(q1, q2, true)
}

fn compose_packed_rates(q1: u64, q2: u64, round_up: bool) -> u64 {
    let r1 = from_rate(q1).unwrap_or(IOUAmount::ZERO);
    let r2 = from_rate(q2).unwrap_or(IOUAmount::ZERO);
    match rxrpl_amount::Amount::mul_round(
        &rxrpl_amount::Amount::Iou(r1),
        &rxrpl_amount::Amount::Iou(r2),
        false,
        round_up,
    ) {
        Ok(rxrpl_amount::Amount::Iou(v)) => pack_rate(&v),
        _ => 0,
    }
}

/// rippled `CreateOffer::select_path`: pick the better of the direct tip and the
/// composed bridge quality. Better quality = smaller packed rate. Returns
/// `(use_direct, chosen_quality)`.
fn select_path(have_direct: bool, direct_q: u64, have_bridge: bool, bridge_q: u64) -> (bool, u64) {
    if !have_bridge {
        return (true, direct_q);
    }
    // rippled: `if (bridged_quality < direct_quality) use direct`. A Quality
    // compares `<` when its rate is LARGER (worse), so bridged worse than direct
    // is `bridge_q > direct_q`.
    if have_direct && bridge_q > direct_q {
        return (true, direct_q);
    }
    (false, bridge_q)
}

/// rippled `Taker::reject`: a quality worse (larger rate) than the threshold is
/// not crossed.
fn reject_quality(quality: u64, threshold: u64) -> bool {
    quality > threshold
}

fn own_tip_can_be_reaped(quality: u64, threshold: u64, enforce_quality: bool) -> bool {
    threshold != 0 && (!enforce_quality || !reject_quality(quality, threshold))
}

/// Tighten a passive offer's crossing limit by one packed-rate ULP. This makes
/// equal-quality direct and autobridged liquidity fail `reject_quality`, while
/// strictly better liquidity remains crossable.
fn crossing_threshold(quality: u64, passive: bool) -> u64 {
    if passive {
        quality.saturating_sub(1)
    } else {
        quality
    }
}

/// OfferCreate's Flow quality limit includes the source asset's transfer fee.
/// rippled computes `sendMax = multiplyRound(TakerGets, rate, roundUp=true)`
/// before constructing `Quality{TakerPays, sendMax}`. Omitting that gross-up
/// rejects liquidity that the fee-adjusted threshold admits (ledger 105500432).
fn flow_crossing_threshold(
    ctx: &ApplyContext<'_>,
    taker: &AccountId,
    input: &Leg,
    output: &Leg,
    number_switchover: bool,
    passive: bool,
) -> u64 {
    let mut send_max = leg_as_quality_iou(input);
    if !input.is_xrp && taker != &input.issuer {
        let rate = transfer_rate(ctx, &input.issuer);
        send_max = IOUAmount::mul_round(&send_max, &rate, true).unwrap_or(send_max);
    }
    quality_threshold(&send_max, output, number_switchover, passive)
}

fn crossing_quality_threshold(
    input: &Leg,
    output: &Leg,
    number_switchover: bool,
    passive: bool,
) -> u64 {
    let input = leg_as_quality_iou(input);
    quality_threshold(&input, output, number_switchover, passive)
}

fn quality_threshold(
    input: &IOUAmount,
    output: &Leg,
    number_switchover: bool,
    passive: bool,
) -> u64 {
    let output = leg_as_quality_iou(output);
    let quality = if number_switchover {
        rxrpl_amount::get_rate_round_even(input, &output)
    } else {
        rxrpl_amount::get_rate(input, &output)
    }
    .unwrap_or(0);
    crossing_threshold(quality, passive)
}

/// rippled `BasicTaker::done` (funds limb handled by the pricer clamps): the
/// taker stops when its input is spent, or (buy) its output demand is met.
fn taker_done(is_sell: bool, remaining_out: &Leg, remaining_in: &Leg) -> bool {
    remaining_in.is_zero() || (!is_sell && remaining_out.is_zero())
}

/// A tfSell fill can deliver past the remaining demand; clamp at zero rather than
/// underflow (mirrors the Flow path).
fn sell_clamp_sub(remaining: &Leg, order: &Leg, is_sell: bool) -> Leg {
    if is_sell && leg_ge(order, remaining) {
        leg_sub(remaining, remaining)
    } else {
        leg_sub(remaining, order)
    }
}

/// rippled `BasicTaker::flow_xrp_to_iou` (T.cpp:214): taker gives XRP, receives an
/// IOU. Clamp cascade owner-balance -> taker-output(buy) -> taker-funds ->
/// taker-input(XrpToIou only), each re-priced NEAREST.
#[allow(clippy::too_many_arguments)]
fn flow_xrp_to_iou(
    offer_in: &Leg,
    offer_out: &Leg,
    rate: &IOUAmount,
    owner_funds: &Leg,
    taker_funds: &Leg,
    out_rate: &IOUAmount,
    is_sell: bool,
    remaining_out: &Leg,
    remaining_in: &Leg,
    cross_type: TakerCrossType,
) -> TakerFlow {
    let mut order_in = offer_in.clone();
    let mut order_out = offer_out.clone();
    let mut issuers_out = gross_leg(&order_out, out_rate);
    if !leg_ge(owner_funds, &issuers_out) {
        issuers_out = owner_funds.clone();
        order_out = div_nearest(&issuers_out, out_rate, offer_out);
        order_in = qual_mul(&order_out, rate, &order_in);
    }
    if !is_sell && !leg_ge(remaining_out, &order_out) {
        order_out = remaining_out.clone();
        order_in = qual_mul(&order_out, rate, &order_in);
        issuers_out = gross_leg(&order_out, out_rate);
    }
    if !leg_ge(taker_funds, &order_in) {
        order_in = taker_funds.clone();
        order_out = qual_div(&order_in, rate, &order_out);
        issuers_out = gross_leg(&order_out, out_rate);
    }
    if cross_type == TakerCrossType::XrpToIou && !leg_ge(remaining_in, &order_in) {
        order_in = remaining_in.clone();
        order_out = qual_div(&order_in, rate, &order_out);
        issuers_out = gross_leg(&order_out, out_rate);
    }
    let issuers_in = order_in.clone();
    TakerFlow {
        order_in,
        order_out,
        issuers_in,
        issuers_out,
    }
}

/// rippled `BasicTaker::flow_iou_to_xrp` (T.cpp:268): taker gives an IOU, receives
/// XRP. Note the clamp order (input before funds) and that the taker-output clamp
/// fires only for a direct IouToXrp cross, not a bridge's first leg.
#[allow(clippy::too_many_arguments)]
fn flow_iou_to_xrp(
    offer_in: &Leg,
    offer_out: &Leg,
    rate: &IOUAmount,
    owner_funds: &Leg,
    taker_funds: &Leg,
    in_rate: &IOUAmount,
    is_sell: bool,
    remaining_out: &Leg,
    remaining_in: &Leg,
    cross_type: TakerCrossType,
) -> TakerFlow {
    let mut order_in = offer_in.clone();
    let mut order_out = offer_out.clone();
    let mut issuers_in = gross_leg(&order_in, in_rate);
    if !leg_ge(owner_funds, &order_out) {
        order_out = owner_funds.clone();
        order_in = qual_mul(&order_out, rate, &order_in);
        issuers_in = gross_leg(&order_in, in_rate);
    }
    if !is_sell && cross_type == TakerCrossType::IouToXrp && !leg_ge(remaining_out, &order_out) {
        order_out = remaining_out.clone();
        order_in = qual_mul(&order_out, rate, &order_in);
        issuers_in = gross_leg(&order_in, in_rate);
    }
    if !leg_ge(remaining_in, &order_in) {
        order_in = remaining_in.clone();
        issuers_in = gross_leg(&order_in, in_rate);
        order_out = qual_div(&order_in, rate, &order_out);
    }
    if !leg_ge(taker_funds, &issuers_in) {
        issuers_in = taker_funds.clone();
        order_in = div_nearest(&issuers_in, in_rate, offer_in);
        order_out = qual_div(&order_in, rate, &order_out);
    }
    let issuers_out = order_out.clone();
    TakerFlow {
        order_in,
        order_out,
        issuers_in,
        issuers_out,
    }
}

/// rippled `BasicTaker::flow_iou_to_iou` (T.cpp:325): both sides IOU, both grossed.
#[allow(clippy::too_many_arguments)]
fn flow_iou_to_iou(
    offer_in: &Leg,
    offer_out: &Leg,
    rate: &IOUAmount,
    owner_funds: &Leg,
    taker_funds: &Leg,
    in_rate: &IOUAmount,
    out_rate: &IOUAmount,
    is_sell: bool,
    remaining_out: &Leg,
    remaining_in: &Leg,
) -> TakerFlow {
    let mut order_in = offer_in.clone();
    let mut order_out = offer_out.clone();
    let mut issuers_in = gross_leg(&order_in, in_rate);
    let mut issuers_out = gross_leg(&order_out, out_rate);
    if !leg_ge(owner_funds, &issuers_out) {
        issuers_out = owner_funds.clone();
        order_out = div_nearest(&issuers_out, out_rate, offer_out);
        order_in = qual_mul(&order_out, rate, &order_in);
        issuers_in = gross_leg(&order_in, in_rate);
    }
    if !is_sell && !leg_ge(remaining_out, &order_out) {
        order_out = remaining_out.clone();
        order_in = qual_mul(&order_out, rate, &order_in);
        issuers_out = gross_leg(&order_out, out_rate);
        issuers_in = gross_leg(&order_in, in_rate);
    }
    if !leg_ge(remaining_in, &order_in) {
        order_in = remaining_in.clone();
        issuers_in = gross_leg(&order_in, in_rate);
        order_out = qual_div(&order_in, rate, &order_out);
        issuers_out = gross_leg(&order_out, out_rate);
    }
    if !leg_ge(taker_funds, &issuers_in) {
        issuers_in = taker_funds.clone();
        order_in = div_nearest(&issuers_in, in_rate, offer_in);
        order_out = qual_div(&order_in, rate, &order_out);
        issuers_out = gross_leg(&order_out, out_rate);
    }
    TakerFlow {
        order_in,
        order_out,
        issuers_in,
        issuers_out,
    }
}

/// The next crossable offer in a book, with its book-directory quality.
#[derive(Clone)]
struct OfferTip {
    key: Hash256,
    dir: Hash256,
    quality: u64,
    owner: AccountId,
    offer_in: Leg,  // TakerPays (what the resting offer wants)
    offer_out: Leg, // TakerGets (what the resting offer gives)
}

/// A best-price-first walk over one order book, reaping expired/empty/frozen/
/// unfunded offers as it advances (rippled `OfferStream`). Holds only the cursor
/// state; every step takes `ctx` so no view borrow is retained across streams.
struct OfferStream {
    prefix: [u8; 24],
    probe: Hash256,
    dir: Hash256,
    quality: u64,
    page: VecDeque<Hash256>,
}

impl OfferStream {
    fn new(book_base: &Hash256) -> Self {
        OfferStream {
            prefix: book_base.as_bytes()[0..24].try_into().unwrap(),
            probe: book_dir_with_quality(book_base, 0),
            dir: *book_base,
            quality: 0,
            page: VecDeque::new(),
        }
    }

    /// Advance to the next fundable offer. `skip_self` removes offers owned by the
    /// taker (used for the direct book; the bridge legs take self-offers).
    fn step(
        &mut self,
        ctx: &mut ApplyContext<'_>,
        mut skip_self: Option<(&AccountId, &mut Value)>,
        threshold: u64,
    ) -> Result<Option<OfferTip>, TransactionResult> {
        loop {
            if self.page.is_empty() {
                let Some(dir_key) = ctx.view.succ(&self.probe) else {
                    return Ok(None);
                };
                if dir_key.as_bytes()[0..24] != self.prefix {
                    return Ok(None);
                }
                self.probe = dir_key;
                self.dir = dir_key;
                self.quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
                let Some(bytes) = ctx.view.read(&dir_key) else {
                    continue;
                };
                let Ok(dir) = serde_json::from_slice::<Value>(&bytes) else {
                    continue;
                };
                self.page = dir
                    .get("Indexes")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().and_then(|s| s.parse().ok()))
                            .collect()
                    })
                    .unwrap_or_default();
                if self.page.is_empty() {
                    continue;
                }
            }
            let offer_key = self.page.pop_front().unwrap();
            let Some(ob) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }
            let owner_str = offer.get("Account").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(owner) = decode_account_id(owner_str) else {
                continue;
            };
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
                continue;
            };
            let Some(offer_in) = Leg::parse(&offer["TakerPays"]) else {
                continue;
            };
            if let Some(exp) = offer.get("Expiration").and_then(|v| v.as_u64()) {
                if exp <= ctx.view.parent_close_time() as u64 {
                    reap_offer(ctx, &owner, &offer_key, &self.dir)?;
                    continue;
                }
            }
            if offer_out.is_zero() || offer_in.is_zero() {
                reap_offer(ctx, &owner, &offer_key, &self.dir)?;
                continue;
            }
            if is_deep_frozen(ctx, &owner, &offer_in) {
                reap_offer(ctx, &owner, &offer_key, &self.dir)?;
                continue;
            }
            let funds = owner_funds_leg(ctx, &owner, &offer_out);
            let is_self = skip_self.as_ref().is_some_and(|(t, _)| *t == &owner);
            if funds.is_zero() {
                reap_offer(ctx, &owner, &offer_key, &self.dir)?;
                continue;
            }
            if should_reap_tiny_increased_quality(
                &owner,
                &offer_in,
                &offer_out,
                &funds,
                self.quality,
            ) {
                reap_offer(ctx, &owner, &offer_key, &self.dir)?;
                continue;
            }
            if is_self {
                // Taker::unfunded is true for owner==taker: cancel self-crossing
                // inverse offers instead of skipping them (30000073 3D88107D).
                // Only within the new offer's quality; worse-page own offers stay.
                if reject_quality(self.quality, threshold) {
                    return Ok(None);
                }
                reap_offer(ctx, &owner, &offer_key, &self.dir)?;
                if let Some((_, acct)) = skip_self.as_mut() {
                    helpers::adjust_owner_count(acct, -1);
                }
                continue;
            }
            return Ok(Some(OfferTip {
                key: offer_key,
                dir: self.dir,
                quality: self.quality,
                owner,
                offer_in,
                offer_out,
            }));
        }
    }
}

/// rippled `BasicTaker::do_cross` single-offer (T.cpp:385): dispatch to the pricer
/// for the taker's cross type. Funds are read by the caller.
#[allow(clippy::too_many_arguments)]
fn do_cross_single(
    ctx: &ApplyContext<'_>,
    taker: &AccountId,
    t: &OfferTip,
    cross_type: TakerCrossType,
    is_sell: bool,
    remaining_out: &Leg,
    remaining_in: &Leg,
    owner_funds: &Leg,
    taker_funds: &Leg,
) -> TakerFlow {
    let rate = from_rate(t.quality).unwrap_or(IOUAmount::ZERO);
    let out_rate = effective_rate(ctx, &t.offer_out.issuer, &t.owner, taker);
    let in_rate = effective_rate(ctx, &t.offer_in.issuer, &t.owner, taker);
    match cross_type {
        TakerCrossType::XrpToIou => flow_xrp_to_iou(
            &t.offer_in,
            &t.offer_out,
            &rate,
            owner_funds,
            taker_funds,
            &out_rate,
            is_sell,
            remaining_out,
            remaining_in,
            cross_type,
        ),
        TakerCrossType::IouToXrp => flow_iou_to_xrp(
            &t.offer_in,
            &t.offer_out,
            &rate,
            owner_funds,
            taker_funds,
            &in_rate,
            is_sell,
            remaining_out,
            remaining_in,
            cross_type,
        ),
        TakerCrossType::IouToIou => flow_iou_to_iou(
            &t.offer_in,
            &t.offer_out,
            &rate,
            owner_funds,
            taker_funds,
            &in_rate,
            &out_rate,
            is_sell,
            remaining_out,
            remaining_in,
        ),
    }
}

/// rippled `BasicTaker::do_cross` bridged (T.cpp:434): price leg1 (in->XRP) and
/// leg2 (XRP->out), apply the self-ownership overrides, then equalize the XRP
/// flow so `flow1.out == flow2.in`.
#[allow(clippy::too_many_arguments)]
fn do_cross_bridged(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    t1: &OfferTip,
    t2: &OfferTip,
    is_sell: bool,
    remaining_out: &Leg,
    remaining_in: &Leg,
) -> (TakerFlow, TakerFlow) {
    let mut leg1_in_funds = owner_funds_leg(ctx, taker, &t1.offer_in);
    if taker == &t1.owner {
        leg1_in_funds = leg_max(&leg1_in_funds, &t1.offer_in);
    }
    let mut leg2_out_funds = owner_funds_leg(ctx, &t2.owner, &t2.offer_out);
    if taker == &t2.owner {
        leg2_out_funds = leg_max(&leg2_out_funds, &t2.offer_out);
    }
    let mut xrp_funds = owner_funds_leg(ctx, &t1.owner, &t1.offer_out);
    if t1.owner == t2.owner {
        xrp_funds = leg_max(&t1.offer_out, &t2.offer_in);
    }
    let leg1_rate = effective_rate(ctx, &t1.offer_in.issuer, &t1.owner, taker);
    let leg2_rate = effective_rate(ctx, &t2.offer_out.issuer, &t2.owner, taker);
    let q1 = from_rate(t1.quality).unwrap_or(IOUAmount::ZERO);
    let q2 = from_rate(t2.quality).unwrap_or(IOUAmount::ZERO);
    let mut flow1 = flow_iou_to_xrp(
        &t1.offer_in,
        &t1.offer_out,
        &q1,
        &xrp_funds,
        &leg1_in_funds,
        &leg1_rate,
        is_sell,
        remaining_out,
        remaining_in,
        TakerCrossType::IouToIou,
    );
    let mut flow2 = flow_xrp_to_iou(
        &t2.offer_in,
        &t2.offer_out,
        &q2,
        &leg2_out_funds,
        &xrp_funds,
        &leg2_rate,
        is_sell,
        remaining_out,
        remaining_in,
        TakerCrossType::IouToIou,
    );
    if leg_gt(&flow2.order_in, &flow1.order_out) {
        flow2.order_in = flow1.order_out.clone();
        flow2.order_out = qual_div(&flow2.order_in, &q2, &flow2.order_out);
        flow2.issuers_out = gross_leg(&flow2.order_out, &leg2_rate);
    } else if leg_gt(&flow1.order_out, &flow2.order_in) {
        flow1.order_out = flow2.order_in.clone();
        flow1.order_in = qual_mul(&flow1.order_out, &q1, &flow1.order_in);
        flow1.issuers_in = gross_leg(&flow1.order_in, &leg1_rate);
    }
    (flow1, flow2)
}

/// Reduce or reap a resting offer consumed by a fill (no settlement). Mirrors the
/// consume/reap tail of `cross_offers`, using exact (legacy) subtraction.
fn consume_leg(
    ctx: &mut ApplyContext<'_>,
    t: &OfferTip,
    order_out: &Leg,
    order_in: &Leg,
    avail_out: &Leg,
) -> Result<(), TransactionResult> {
    let _ = avail_out;
    let Some(ob) = ctx.view.read(&t.key) else {
        return Ok(());
    };
    let Ok(mut offer) = serde_json::from_slice::<Value>(&ob) else {
        return Ok(());
    };
    // `OfferTip` is the first-seen snapshot. Bridged_cross reuses the same
    // leg1 (or leg2) tip across the other stream's fills, so leftover must
    // subtract this fill from the current SLE, not from that snapshot.
    let current_out = Leg::parse(&offer["TakerGets"]).unwrap_or_else(|| t.offer_out.clone());
    let current_in = Leg::parse(&offer["TakerPays"]).unwrap_or_else(|| t.offer_in.clone());
    let leftover_out = if leg_ge(order_out, &current_out) {
        zero_leg(&current_out)
    } else {
        leg_sub(&current_out, order_out)
    };
    let leftover_in = if leg_ge(order_in, &current_in) {
        zero_leg(&current_in)
    } else {
        leg_sub(&current_in, order_in)
    };
    offer["TakerGets"] = leftover_out.with_amount(&leftover_out.iou, leftover_out.drops);
    offer["TakerPays"] = leftover_in.with_amount(&leftover_in.iou, leftover_in.drops);
    let cb = serde_json::to_vec(&offer).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(t.key, cb)
        .map_err(|_| TransactionResult::TefInternal)?;
    // rippled CreateOffer::dry_offer: fully consumed, or the owner cannot fund
    // leftover TakerGets (transfer-fee gross drained the line).
    if leftover_out.is_zero()
        || leftover_in.is_zero()
        || (leftover_in.is_xrp
            && leftover_in.drops <= 1
            && leftover_is_dust(&leftover_out, &t.offer_out))
        || owner_funds_leg(ctx, &t.owner, &t.offer_out).is_zero()
    {
        reap_offer(ctx, &t.owner, &t.key, &t.dir)?;
    }
    Ok(())
}

fn offer_is_dry(
    ctx: &mut ApplyContext<'_>,
    owner: &AccountId,
    offer_out: &Leg,
    order_out: &Leg,
) -> bool {
    if leg_ge(order_out, offer_out) {
        return true;
    }
    let leftover = leg_sub(offer_out, order_out);
    leftover.is_zero() || owner_funds_leg(ctx, owner, offer_out).is_zero()
}

/// rippled `Taker::fill` input settlement (taker -> owner), a `redeemIOU` plus
/// `issueIOU` pair (or `transferXRP`). `order_in` is the NET the owner receives;
/// `issuers_in` the GROSS the taker's line is debited (grossed at the EFFECTIVE
/// rate by the pricer). Each IOU leg skips the issuer's own (nonexistent)
/// self-line, exactly as rippled's rippleCredit (`account == issue.account` ->
/// no-op). Distinct from the shared `pay_in`, which grosses at the raw
/// `transfer_rate` and lacks the taker/owner self-issuer skip — the FlowCross path
/// keeps `pay_in`; only this legacy arm needs the fee-party-exempt semantics.
fn settle_in_legacy(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    order_in: &Leg,
    issuers_in: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    if order_in.is_xrp {
        if taker != owner && order_in.drops != 0 {
            let bal = helpers::get_balance(taker_acct) as i64 - order_in.drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedOffer);
            }
            helpers::set_balance(taker_acct, bal as u64);
            credit_xrp(ctx, owner, order_in.drops)?;
        }
        return Ok(());
    }
    let issuer = order_in.issuer;
    if taker != &issuer {
        credit_line(
            ctx,
            taker,
            &issuer,
            &order_in.currency,
            &issuers_in.iou.negate(),
            round,
        )?;
        // rippled's rippleCredit deletes the payer's line when it drains to a
        // default zero-balance state (View.cpp:994) — the taker just paid out.
        crate::handlers::trust_set::maybe_delete_drained_trust_line(
            ctx,
            taker,
            taker_acct,
            &issuer,
            &order_in.currency,
        )?;
    }
    if owner != &issuer {
        // rippleCredit createOnDemand for the maker: 30000015 OfferCreate
        // 512120C8 delivers Gatehub BTC to rH3uSRUJ who had no BTC line.
        // Missing-line TecPathDry left that take unapplied, so later
        // IOU-selling OfferCreates (BBA2D098) were tecUNFUNDED.
        let tl_key = keylet::trust_line(owner, &issuer, &order_in.currency);
        if ctx.view.read(&tl_key).is_none() {
            let owner_key = keylet::account(owner);
            let ob = ctx
                .view
                .read(&owner_key)
                .ok_or(TransactionResult::TefInternal)?;
            let mut oacct: Value =
                serde_json::from_slice(&ob).map_err(|_| TransactionResult::TefInternal)?;
            create_iou_trust_line(ctx, owner, &mut oacct, &issuer, &order_in.currency)?;
            let nb = serde_json::to_vec(&oacct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(owner_key, nb)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
        credit_line(
            ctx,
            owner,
            &issuer,
            &order_in.currency,
            &order_in.iou,
            round,
        )?;
    }
    Ok(())
}

/// rippled `Taker::fill` output settlement (owner -> taker). `order_out` is the NET
/// the taker receives; `issuers_out` the GROSS the owner's line is debited. Issuer
/// self-lines skipped; a taker XRP credit routes through `taker_acct`.
fn settle_out_legacy(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    order_out: &Leg,
    issuers_out: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    if order_out.is_xrp {
        if owner != taker && order_out.drops != 0 {
            credit_xrp(ctx, owner, -order_out.drops)?;
            helpers::set_balance(
                taker_acct,
                helpers::get_balance(taker_acct) + order_out.drops as u64,
            );
        }
        return Ok(());
    }
    let issuer = order_out.issuer;
    if owner != &issuer {
        credit_line(
            ctx,
            owner,
            &issuer,
            &order_out.currency,
            &issuers_out.iou.negate(),
            round,
        )?;
        // Owner paid the IOU out; rippleCredit deletes a drained default line.
        let owner_key = keylet::account(owner);
        if let Some(ob) = ctx.view.read(&owner_key) {
            let mut oacct: Value =
                serde_json::from_slice(&ob).map_err(|_| TransactionResult::TefInternal)?;
            if crate::handlers::trust_set::maybe_delete_drained_trust_line(
                ctx,
                owner,
                &mut oacct,
                &issuer,
                &order_out.currency,
            )? {
                let nb = serde_json::to_vec(&oacct).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(owner_key, nb)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        }
    }
    if taker != &issuer {
        // rippleCredit createOnDemand: first delivery of an IOU the taker
        // has never held writes a CreatedNode RippleState.
        let tl_key = keylet::trust_line(taker, &issuer, &order_out.currency);
        if ctx.view.read(&tl_key).is_none() {
            create_iou_trust_line(ctx, taker, taker_acct, &issuer, &order_out.currency)?;
        }
        credit_line(
            ctx,
            taker,
            &issuer,
            &order_out.currency,
            &order_out.iou,
            round,
        )?;
    }
    Ok(())
}

/// rippled `Taker::fill` direct (T.cpp:683): move taker input to the owner, owner
/// output to the taker, then consume/reap.
fn fill_direct(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    t: &OfferTip,
    flow: &TakerFlow,
    avail_out: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    settle_in_legacy(
        ctx,
        taker,
        taker_acct,
        &t.owner,
        &flow.order_in,
        &flow.issuers_in,
        round,
    )?;
    settle_out_legacy(
        ctx,
        taker,
        taker_acct,
        &t.owner,
        &flow.order_out,
        &flow.issuers_out,
        round,
    )?;
    consume_leg(ctx, t, &flow.order_out, &flow.order_in, avail_out)
}

/// rippled `Taker::fill` bridged (T.cpp:739): taker -> leg1 (IOU), leg1 -> leg2
/// (XRP, owner to owner), leg2 -> taker (IOU), then consume/reap both legs.
#[allow(clippy::too_many_arguments)]
fn fill_bridged(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    t1: &OfferTip,
    flow1: &TakerFlow,
    avail1: &Leg,
    t2: &OfferTip,
    flow2: &TakerFlow,
    avail2: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    if &t1.owner != taker {
        settle_in_legacy(
            ctx,
            taker,
            taker_acct,
            &t1.owner,
            &flow1.order_in,
            &flow1.issuers_in,
            round,
        )?;
    }
    // leg1 owner -> leg2 owner: a discrete XRP transfer (rippled transferXRP).
    let drops = flow1.order_out.drops;
    if drops != 0 && t1.owner != t2.owner {
        if &t1.owner == taker {
            let bal = helpers::get_balance(taker_acct) as i64 - drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedOffer);
            }
            helpers::set_balance(taker_acct, bal as u64);
        } else {
            credit_xrp(ctx, &t1.owner, -drops)?;
        }
        if &t2.owner == taker {
            helpers::set_balance(taker_acct, helpers::get_balance(taker_acct) + drops as u64);
        } else {
            credit_xrp(ctx, &t2.owner, drops)?;
        }
    }
    if &t2.owner != taker {
        settle_out_legacy(
            ctx,
            taker,
            taker_acct,
            &t2.owner,
            &flow2.order_out,
            &flow2.issuers_out,
            round,
        )?;
    }
    consume_leg(ctx, t1, &flow1.order_out, &flow1.order_in, avail1)?;
    consume_leg(ctx, t2, &flow2.order_out, &flow2.order_in, avail2)
}

/// rippled `BasicTaker::remaining_offer` (T.cpp:141): rescale the leftover back to
/// the ORIGINAL offer quality so the shared placement path keys the book
/// directory on the unchanged rate. Returns tx-oriented `(pays, gets)`.
fn remaining_offer(
    is_sell: bool,
    orig_quality: u64,
    remaining_out: &Leg,
    remaining_in: &Leg,
    out_leg: &Leg,
    in_leg: &Leg,
    crossed: bool,
) -> (Value, Value) {
    if remaining_out.is_zero() || remaining_in.is_zero() {
        return (
            out_leg.with_amount(&IOUAmount::ZERO, 0),
            in_leg.with_amount(&IOUAmount::ZERO, 0),
        );
    }
    if !crossed {
        return (
            remaining_out.with_amount(&remaining_out.iou, remaining_out.drops),
            remaining_in.with_amount(&remaining_in.iou, remaining_in.drops),
        );
    }
    let rate = from_rate(orig_quality).unwrap_or(IOUAmount::ZERO);
    if is_sell {
        let gets = remaining_in.clone();
        let pays = rxrpl_amount::Amount::div_round(
            &leg_to_amount(remaining_in),
            &rxrpl_amount::Amount::Iou(rate),
            out_leg.is_xrp,
            false,
        )
        .map(|x| amount_to_leg(&x, out_leg))
        .unwrap_or_else(|_| out_leg.clone());
        (
            pays.with_amount(&pays.iou, pays.drops),
            gets.with_amount(&gets.iou, gets.drops),
        )
    } else {
        let pays = remaining_out.clone();
        let gets = rxrpl_amount::Amount::mul_round(
            &leg_to_amount(remaining_out),
            &rxrpl_amount::Amount::Iou(rate),
            in_leg.is_xrp,
            true,
        )
        .map(|x| amount_to_leg(&x, in_leg))
        .unwrap_or_else(|_| in_leg.clone());
        (
            pays.with_amount(&pays.iou, pays.drops),
            gets.with_amount(&gets.iou, gets.drops),
        )
    }
}

/// rippled `CreateOffer::direct_cross` (CO.cpp:511): a single-book best-price walk
/// for XrpToIou / IouToXrp takers.
#[allow(clippy::too_many_arguments)]
fn direct_cross(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    out_leg: &Leg,
    in_leg: &Leg,
    book: &Hash256,
    threshold: u64,
    is_sell: bool,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    round: bool,
) -> Result<bool, TransactionResult> {
    let _ = out_leg;
    let cross_type = if in_leg.is_xrp {
        TakerCrossType::XrpToIou
    } else {
        TakerCrossType::IouToXrp
    };
    let mut stream = OfferStream::new(book);
    let mut crossed = false;
    let mut direct_crossings: u32 = 0;
    let mut tip = stream.step(ctx, Some((taker, taker_acct)), threshold)?;
    while let Some(t) = tip.take() {
        if reject_quality(t.quality, threshold) {
            break;
        }
        let owner_funds = owner_funds_leg(ctx, &t.owner, &t.offer_out);
        let taker_funds = owner_funds_leg(ctx, taker, &t.offer_in);
        let avail_out = leg_min(&t.offer_out, &owner_funds);
        let flow = do_cross_single(
            ctx,
            taker,
            &t,
            cross_type,
            is_sell,
            remaining_out,
            remaining_in,
            &owner_funds,
            &taker_funds,
        );
        let did = !flow.order_out.is_zero() || !flow.order_in.is_zero();
        if did {
            fill_direct(ctx, taker, taker_acct, &t, &flow, &avail_out, round)?;
            crossed = true;
            direct_crossings += 1;
        }
        *remaining_out = sell_clamp_sub(remaining_out, &flow.order_out, is_sell);
        *remaining_in = leg_sub(remaining_in, &flow.order_in);
        if offer_is_dry(ctx, &t.owner, &t.offer_out, &flow.order_out) {
            reap_maker_short_sells(ctx, &t.owner, &t.offer_out)?;
        }
        if taker_done(is_sell, remaining_out, remaining_in) {
            break;
        }
        if direct_crossings >= 850 {
            break;
        }
        if !offer_is_dry(ctx, &t.owner, &t.offer_out, &flow.order_out) {
            break;
        }
        tip = stream.step(ctx, Some((taker, taker_acct)), threshold)?;
    }
    Ok(crossed)
}

/// rippled `CreateOffer::bridged_cross` (CO.cpp:322): the direct book plus the
/// XRP autobridge (leg1 in->XRP, leg2 XRP->out), `select_path`-chosen per band.
#[allow(clippy::too_many_arguments)]
fn bridged_cross(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    out_leg: &Leg,
    in_leg: &Leg,
    direct_book: &Hash256,
    threshold: u64,
    is_sell: bool,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    round: bool,
) -> Result<bool, TransactionResult> {
    let xrp_ccy = [0u8; 20];
    let xrp_iss = AccountId::from([0u8; 20]);
    let leg1_book = keylet::book_dir(&in_leg.currency, &in_leg.issuer, &xrp_ccy, &xrp_iss);
    let leg2_book = keylet::book_dir(&xrp_ccy, &xrp_iss, &out_leg.currency, &out_leg.issuer);
    let mut sd = OfferStream::new(direct_book);
    let mut s1 = OfferStream::new(&leg1_book);
    let mut s2 = OfferStream::new(&leg2_book);
    let mut crossed = false;
    let mut direct_crossings: u32 = 0;
    let mut bridge_crossings: u32 = 0;
    let mut direct_tip = sd.step(ctx, Some((taker, taker_acct)), threshold)?;
    let mut leg1_tip = s1.step(ctx, None, threshold)?;
    let mut leg2_tip = s2.step(ctx, None, threshold)?;
    loop {
        let have_direct = direct_tip.is_some();
        let have_bridge = leg1_tip.is_some() && leg2_tip.is_some();
        if !have_direct && !have_bridge {
            break;
        }
        let direct_q = direct_tip.as_ref().map(|t| t.quality).unwrap_or(0);
        let bridge_q = if have_bridge {
            composed_quality(
                leg1_tip.as_ref().unwrap().quality,
                leg2_tip.as_ref().unwrap().quality,
            )
        } else {
            0
        };
        let (use_direct, quality) = select_path(have_direct, direct_q, have_bridge, bridge_q);
        if reject_quality(quality, threshold) {
            break;
        }
        let mut advanced = false;
        if use_direct {
            let t = direct_tip.clone().unwrap();
            let owner_funds = owner_funds_leg(ctx, &t.owner, &t.offer_out);
            let taker_funds = owner_funds_leg(ctx, taker, &t.offer_in);
            let avail_out = leg_min(&t.offer_out, &owner_funds);
            let flow = do_cross_single(
                ctx,
                taker,
                &t,
                TakerCrossType::IouToIou,
                is_sell,
                remaining_out,
                remaining_in,
                &owner_funds,
                &taker_funds,
            );
            let did = !flow.order_out.is_zero() || !flow.order_in.is_zero();
            if did {
                fill_direct(ctx, taker, taker_acct, &t, &flow, &avail_out, round)?;
                crossed = true;
                direct_crossings += 1;
            }
            *remaining_out = sell_clamp_sub(remaining_out, &flow.order_out, is_sell);
            *remaining_in = leg_sub(remaining_in, &flow.order_in);
            if offer_is_dry(ctx, &t.owner, &t.offer_out, &flow.order_out) {
                direct_tip = sd.step(ctx, Some((taker, taker_acct)), threshold)?;
                advanced = true;
            }
        } else {
            let t1 = leg1_tip.clone().unwrap();
            let t2 = leg2_tip.clone().unwrap();
            let owner1_funds = owner_funds_leg(ctx, &t1.owner, &t1.offer_out);
            let owner2_funds = owner_funds_leg(ctx, &t2.owner, &t2.offer_out);
            let avail1 = leg_min(&t1.offer_out, &owner1_funds);
            let avail2 = leg_min(&t2.offer_out, &owner2_funds);
            let (flow1, flow2) =
                do_cross_bridged(ctx, taker, &t1, &t2, is_sell, remaining_out, remaining_in);
            let did = !flow1.order_in.is_zero() || !flow2.order_out.is_zero();
            if did {
                fill_bridged(
                    ctx, taker, taker_acct, &t1, &flow1, &avail1, &t2, &flow2, &avail2, round,
                )?;
                crossed = true;
                bridge_crossings += 1;
            }
            *remaining_out = sell_clamp_sub(remaining_out, &flow2.order_out, is_sell);
            *remaining_in = leg_sub(remaining_in, &flow1.order_in);
            if offer_is_dry(ctx, &t1.owner, &t1.offer_out, &flow1.order_out) {
                leg1_tip = s1.step(ctx, None, threshold)?;
                advanced = true;
            }
            if offer_is_dry(ctx, &t2.owner, &t2.offer_out, &flow2.order_out) {
                leg2_tip = s2.step(ctx, None, threshold)?;
                advanced = true;
            }
        }
        if taker_done(is_sell, remaining_out, remaining_in) {
            break;
        }
        if direct_crossings + 2 * bridge_crossings >= 850 {
            break;
        }
        // If no stream advanced this round the taker is either done (checked above)
        // or unfunded; stop rather than re-crossing the same tips. Mirrors rippled's
        // done()/unfunded() limb and the "an offer must be fully consumed"
        // postcondition of bridged_cross.
        if !advanced {
            break;
        }
    }
    Ok(crossed)
}

/// rippled `CreateOffer::takerCross` (CO.cpp:627): the legacy `!FlowCross` arm.
/// Returns the same tx-oriented `(remaining_pays, remaining_gets, crossed)` tuple
/// as `cross_offers`, so the shared placement tail in `apply()` is unchanged.
#[allow(clippy::too_many_arguments)]
fn taker_cross(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    taker_pays: &Value,
    taker_gets: &Value,
    inverse_book: &Hash256,
    is_sell: bool,
    _domain_only: bool,
) -> Result<(Value, Value, bool), TransactionResult> {
    let out_leg = match Leg::parse(taker_pays) {
        Some(l) => l,
        None => return Ok((taker_pays.clone(), taker_gets.clone(), false)),
    };
    let in_leg = match Leg::parse(taker_gets) {
        Some(l) => l,
        None => return Ok((taker_pays.clone(), taker_gets.clone(), false)),
    };
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let pays_iou = leg_as_quality_iou(&out_leg);
    let gets_iou = leg_as_quality_iou(&in_leg);
    let orig_quality = if number_switchover {
        rxrpl_amount::get_rate_round_even(&gets_iou, &pays_iou)
    } else {
        rxrpl_amount::get_rate(&gets_iou, &pays_iou)
    }
    .unwrap_or(0);
    if orig_quality == 0 {
        return Ok((taker_pays.clone(), taker_gets.clone(), false));
    }
    // rippled tightens the crossing threshold by one rate ULP for a passive offer
    // so equal-quality resting offers are NOT consumed (`++threshold_`, T.cpp:79).
    // `Quality::operator++` is `--m_value` ("advance to the next higher quality"),
    // and a lower packed rate is a better quality, so the bump is a decrement here:
    // reject (`dir_quality > threshold`) then fires on an equal-quality offer.
    let passive = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_PASSIVE != 0;
    let threshold = crossing_threshold(orig_quality, passive);

    let mut remaining_out = out_leg.clone();
    let mut remaining_in = in_leg.clone();
    let crossed = if !in_leg.is_xrp && !out_leg.is_xrp {
        bridged_cross(
            ctx,
            taker,
            taker_acct,
            &out_leg,
            &in_leg,
            inverse_book,
            threshold,
            is_sell,
            &mut remaining_out,
            &mut remaining_in,
            number_switchover,
        )?
    } else {
        direct_cross(
            ctx,
            taker,
            taker_acct,
            &out_leg,
            &in_leg,
            inverse_book,
            threshold,
            is_sell,
            &mut remaining_out,
            &mut remaining_in,
            number_switchover,
        )?
    };

    let (remaining_pays, remaining_gets) = remaining_offer(
        is_sell,
        orig_quality,
        &remaining_out,
        &remaining_in,
        &out_leg,
        &in_leg,
        crossed,
    );
    Ok((remaining_pays, remaining_gets, crossed))
}

#[cfg(test)]
mod taker_crossing_tests {
    use super::{
        BalanceRounding, Leg, TakerCrossType, add_iou_balance, add_iou_balance_downward,
        add_iou_balance_half_upward, aggregate_flow_source_balance_number,
        aggregate_output_remainder, amm_pool_debit_rounding, amm_recipient_rounding,
        amm_step_allows_clob, book_destination_credit, book_payment_dust_drain,
        book_step_input_exhausted, bridge_funded_xrp_input, clob_band_allows_terminal_amm,
        composed_quality, consumes_full_offer, crossing_quality_threshold, crossing_threshold,
        debit_drains_source, flow_first_book_spends_source_funds, flow_iou_to_iou, flow_iou_to_xrp,
        flow_net_input, flow_source_funds_limit_binds, flow_xrp_to_iou, full_offer_take,
        funds_limited_xrp_rounds_up, gross_book_payment_input, in_for_out, in_for_out_offer,
        in_for_out_round, input_limited_uses_full_budget, leftover_is_dust, leftover_leg,
        leg_as_quality_iou, leg_ge, leg_gt, leg_max, leg_to_number,
        legacy_intermediate_xrp_rounding_applies, net_book_payment_input, net_offer_input_budget,
        number_to_leg, out_for_in, own_tip_can_be_reaped, pack_rate, parity_rate,
        preserve_flow_input_limited_iou_ulp, preserve_next_quality_self_offer, qual_div, qual_mul,
        reject_quality, remaining_is_filled, remaining_is_filled_at, remaining_offer,
        scale_funds_limited_input, select_path, sell_clamp_sub, should_bump_partial_path_credit,
        should_reap_tiny_increased_quality, should_trim_input_limited_iou_leftover,
        sum_smallest_to_largest, terminal_amm_reaches_deferred_clob, terminal_budget_binds,
        zero_cost_funded_xrp_offer, zero_cost_xrp_clob_fill,
    };
    use rxrpl_amount::number::Number;
    use rxrpl_amount::{IOUAmount, from_rate, get_rate, get_rate_round_even, offer_quality};
    use rxrpl_primitives::AccountId;
    use rxrpl_protocol::keylet;

    fn iou(v: &str) -> Leg {
        Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::from_decimal_string(v).unwrap(),
            currency: [1u8; 20],
            issuer: AccountId::from([2u8; 20]),
        }
    }

    fn xrp(d: i64) -> Leg {
        Leg {
            is_xrp: true,
            drops: d,
            iou: IOUAmount::ZERO,
            currency: [0u8; 20],
            issuer: AccountId::from([0u8; 20]),
        }
    }

    #[test]
    fn ledger_105500329_reaps_sub_drop_funded_offer() {
        let owner = AccountId::from([3u8; 20]);
        let offer_in = xrp(1_230_000);
        let offer_out = iou("1232257.040353");
        let owner_funds = iou("0.200820528");
        let quality = get_rate(
            &leg_as_quality_iou(&offer_in),
            &leg_as_quality_iou(&offer_out),
        )
        .unwrap();

        assert!(should_reap_tiny_increased_quality(
            &owner,
            &offer_in,
            &offer_out,
            &owner_funds,
            quality,
        ));

        assert!(!should_reap_tiny_increased_quality(
            &owner,
            &offer_in,
            &offer_out,
            &iou("0"),
            quality,
        ));

        let material_funds = iou("100");
        assert!(!should_reap_tiny_increased_quality(
            &owner,
            &offer_in,
            &offer_out,
            &material_funds,
            quality,
        ));
    }

    #[test]
    fn ledger_105501028_reaps_positive_sub_drop_offers() {
        let owner = AccountId::from([3u8; 20]);
        let offer_in = xrp(232_432_013);
        let offer_out = iou("4321039.7364774");
        let quality = 6_202_332_163_343_885_242;
        for balance in ["0.0071883", "0.0128505", "0.0061701", "0.0102685"] {
            assert!(should_reap_tiny_increased_quality(
                &owner,
                &offer_in,
                &offer_out,
                &iou(balance),
                quality,
            ));
        }
    }

    fn rate(inn: &str, out: &str) -> IOUAmount {
        from_rate(
            get_rate(
                &IOUAmount::from_decimal_string(inn).unwrap(),
                &IOUAmount::from_decimal_string(out).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn ledger_105500002_destination_balance_uses_number_rounding() {
        let before = IOUAmount::from_decimal_string("-2178.562819378854").unwrap();
        let delta = IOUAmount::from_decimal_string("-0.004283181667522685").unwrap();

        assert_eq!(
            add_iou_balance(&before, &delta, false)
                .unwrap()
                .to_decimal_string(),
            "-2178.567102560521"
        );
        assert_eq!(
            add_iou_balance(&before, &delta, true)
                .unwrap()
                .to_decimal_string(),
            "-2178.567102560522"
        );
    }

    #[test]
    fn ledger_105501905_direct_clob_source_debits_once() {
        let before = IOUAmount::from_decimal_string("1535.346215477393").unwrap();
        let debits = ["243.6968029113126", "779.8129600886874"]
            .map(|value| IOUAmount::from_decimal_string(value).unwrap());
        let sequential = debits.iter().fold(before, |balance, debit| {
            IOUAmount::add_round(&balance, &debit.negate()).unwrap()
        });
        assert_eq!(sequential.to_decimal_string(), "511.8364524773926");

        let spent = sum_smallest_to_largest(&debits.map(|debit| Number::from_iou(&debit)));
        assert_eq!(spent.to_iou().to_decimal_string(), "1023.509763");
        assert_eq!(
            aggregate_flow_source_balance_number(&before, &spent, false).to_decimal_string(),
            "511.836452477393"
        );
    }

    #[test]
    fn ledger_105500435_amm_pool_debit_rounds_downward() {
        let before = IOUAmount::from_decimal_string("-0.01387908469263376").unwrap();
        let delta = IOUAmount::from_decimal_string("0.00005820841462248592").unwrap();

        assert_eq!(
            add_iou_balance_downward(&before, &delta).to_decimal_string(),
            "-0.01382087627801128"
        );
        assert_eq!(
            add_iou_balance(&before, &delta, true)
                .unwrap()
                .to_decimal_string(),
            "-0.01382087627801127"
        );
    }

    #[test]
    fn ledger_105500789_amm_pool_and_recipient_use_distinct_rounding() {
        let pool_before = IOUAmount::from_decimal_string("-341.3073048793806").unwrap();
        let delivery = IOUAmount::from_decimal_string("6.64285325496125").unwrap();

        assert_eq!(
            add_iou_balance(&pool_before, &delivery, true)
                .unwrap()
                .to_decimal_string(),
            "-334.6644516244194"
        );
        assert_eq!(
            add_iou_balance_downward(&pool_before, &delivery).to_decimal_string(),
            "-334.6644516244194"
        );
        assert_eq!(
            add_iou_balance_half_upward(&pool_before, &delivery)
                .unwrap()
                .to_decimal_string(),
            "-334.6644516244193"
        );
    }

    #[test]
    fn ledger_105507029_offer_amm_recipient_halfway_credit() {
        let before = IOUAmount::from_decimal_string("-97610.72207438135").unwrap();
        let received = IOUAmount::from_decimal_string("13008.0553478498").unwrap();
        let delta = received.negate();

        assert!(matches!(
            amm_recipient_rounding(true, true),
            BalanceRounding::Nearest
        ));
        assert!(matches!(
            amm_recipient_rounding(false, true),
            BalanceRounding::HalfUpward
        ));
        assert!(matches!(
            amm_recipient_rounding(true, false),
            BalanceRounding::HalfUpward
        ));
        assert_eq!(
            add_iou_balance(&before, &delta, true)
                .unwrap()
                .to_decimal_string(),
            "-110618.7774222312"
        );
        assert_eq!(
            add_iou_balance_half_upward(&before, &delta)
                .unwrap()
                .to_decimal_string(),
            "-110618.7774222311"
        );
    }

    #[test]
    fn ledger_105501026_bridge_output_and_pool_balance_round_nearest() {
        let output = iou("0.0007164812156765496");
        let remaining = leftover_leg(&iou("0.02966"), &output, true);
        assert_eq!(remaining.iou.to_decimal_string(), "0.02894351878432345");

        let pool_before = IOUAmount::from_decimal_string("-1.771537787940604").unwrap();
        assert_eq!(
            add_iou_balance(&pool_before, &output.iou, true)
                .unwrap()
                .to_decimal_string(),
            "-1.770821306724927"
        );
        assert_eq!(
            add_iou_balance_downward(&pool_before, &output.iou).to_decimal_string(),
            "-1.770821306724928"
        );
    }

    #[test]
    fn ledger_105500849_offer_cross_pool_debit_rounds_nearest() {
        let before = IOUAmount::from_decimal_string("58255413.05686753").unwrap();
        let delivery = IOUAmount::from_decimal_string("738866.732999333").unwrap();
        let debit = delivery.negate();

        assert!(matches!(
            amm_pool_debit_rounding(false),
            BalanceRounding::Nearest
        ));
        assert!(matches!(
            amm_pool_debit_rounding(true),
            BalanceRounding::HalfUpward
        ));

        assert_eq!(
            add_iou_balance(&before, &debit, true)
                .unwrap()
                .to_decimal_string(),
            "57516546.3238682"
        );
        assert_eq!(
            add_iou_balance_downward(&before, &debit).to_decimal_string(),
            "57516546.32386819"
        );
    }

    #[test]
    fn ledger_105500855_direct_amm_pool_debit_rounds_nearest() {
        let before = IOUAmount::from_decimal_string("-33186471.87677241").unwrap();
        let delivery = IOUAmount::from_decimal_string("342084.897489097").unwrap();

        assert!(matches!(
            amm_pool_debit_rounding(false),
            BalanceRounding::Nearest
        ));
        assert_eq!(
            add_iou_balance(&before, &delivery, true)
                .unwrap()
                .to_decimal_string(),
            "-32844386.97928331"
        );
        assert_eq!(
            add_iou_balance_downward(&before, &delivery).to_decimal_string(),
            "-32844386.97928332"
        );
    }

    #[test]
    fn ledger_105500863_payment_pool_debit_rounds_nearest() {
        let before = IOUAmount::from_decimal_string("3009996411.331705").unwrap();
        let delivery = IOUAmount::from_decimal_string("700350.712002475").unwrap();
        let debit = delivery.negate();

        assert!(matches!(
            amm_pool_debit_rounding(false),
            BalanceRounding::Nearest
        ));
        assert_eq!(
            add_iou_balance(&before, &debit, true)
                .unwrap()
                .to_decimal_string(),
            "3009296060.619703"
        );
        assert_eq!(
            add_iou_balance_downward(&before, &debit).to_decimal_string(),
            "3009296060.619702"
        );
    }

    #[test]
    fn ledger_105500249_terminal_path_pool_halfway_debit() {
        let before = IOUAmount::from_decimal_string("-27.66112374802621").unwrap();
        let debit = IOUAmount::from_decimal_string("0.180231474079375").unwrap();

        assert_eq!(
            add_iou_balance_half_upward(&before, &debit)
                .unwrap()
                .to_decimal_string(),
            "-27.48089227394683"
        );
        assert_eq!(
            add_iou_balance(&before, &debit, true)
                .unwrap()
                .to_decimal_string(),
            "-27.48089227394684"
        );
    }

    #[test]
    fn ledger_105500795_intermediate_amm_pool_debit_rounds_half_upward() {
        for (before, delivery, expected) in [
            (
                "-48.13676099974311",
                "0.2133713275912883",
                "-47.92338967215182",
            ),
            (
                "-27546.71827174122",
                "42.94068601073885",
                "-27503.77758573048",
            ),
        ] {
            let before = IOUAmount::from_decimal_string(before).unwrap();
            let delivery = IOUAmount::from_decimal_string(delivery).unwrap();

            assert_eq!(
                add_iou_balance_half_upward(&before, &delivery)
                    .unwrap()
                    .to_decimal_string(),
                expected
            );
            assert_ne!(
                add_iou_balance_downward(&before, &delivery).to_decimal_string(),
                expected
            );
        }
    }

    #[test]
    fn ledger_105500053_funds_limited_iou_uses_nearest_quality() {
        let offer_in = iou("52.3881452157");
        let take_out = iou("10.15254433053624");
        let rate = from_rate(0x5512_5515_6781_6800).unwrap();

        assert_eq!(
            qual_mul(&take_out, &rate, &offer_in)
                .iou
                .to_decimal_string(),
            "52.38814400000005"
        );
        assert_eq!(
            in_for_out_round(&take_out, &rate, &offer_in, false)
                .iou
                .to_decimal_string(),
            "52.38814400000005"
        );
        assert_eq!(
            in_for_out(&take_out, &rate, &offer_in)
                .iou
                .to_decimal_string(),
            "52.38814400000006"
        );
    }

    #[test]
    fn ledger_105500333_funded_xrp_limits_offer_input_downward() {
        let offer_in = iou("17.539466");
        let funded_xrp = xrp(15_328_768);
        let rate = from_rate(0x4F03_F4BD_1BA9_5CC8).unwrap();

        assert_eq!(
            in_for_out(&funded_xrp, &rate, &offer_in)
                .iou
                .to_decimal_string(),
            "17.06885878250484"
        );
        assert_eq!(
            in_for_out_round(&funded_xrp, &rate, &offer_in, false)
                .iou
                .to_decimal_string(),
            "17.06885878250483"
        );
    }

    #[test]
    fn ledger_105500113_full_input_consumes_quality_dust() {
        let offer_out = iou("300");
        let offer_in = xrp(271_787_432);
        let rate = from_rate(0x5A20_2FA4_174A_E2AB).unwrap();
        let take_out = out_for_in(&offer_in, &rate, &offer_out);

        assert_eq!(take_out.iou.to_decimal_string(), "299.9999999999999");
        assert!(consumes_full_offer(
            &take_out, &offer_out, &offer_in, &offer_in, &offer_out, &offer_out, false,
        ));
    }

    #[test]
    fn ledger_105500357_funds_limited_debit_drains_iou_dust() {
        let source_funds = iou("0.679235696862605");
        let priced_debit = iou("0.6792356968626046");
        let material_remainder = iou("0.6792356968625");

        assert!(debit_drains_source(&source_funds, &priced_debit));
        assert!(!debit_drains_source(&source_funds, &material_remainder));
    }

    #[test]
    fn ledger_105503179_funded_maker_fits_integral_xrp_budget() {
        // F7A30553, first book hop: the maker holds less DROP than its offer
        // names. Quality-floor pricing leaves a 0.000000259520157 DROP sliver,
        // but the entire funded output costs the same 6,550,073 XRP drops.
        let offer_in = xrp(6_759_601_435);
        let offer_out = iou("3809.89083842443");
        let available = iou("3.691795308929194");
        let remaining_in = xrp(6_550_073);
        let rate = from_rate(0x5B06_4DA5_D16E_696C).unwrap();

        let quality_quote = out_for_in(&remaining_in, &rate, &offer_out);
        assert_eq!(quality_quote.iou.to_decimal_string(), "3.691795049409037");
        assert!(leg_gt(&available, &quality_quote));
        assert_eq!(
            scale_funds_limited_input(&offer_in, &available, &offer_out).drops,
            6_550_073
        );
        assert!(super::funds_limited_offer_fits_budget(
            &offer_in,
            &offer_out,
            &available,
            &available,
            &remaining_in,
        ));
        assert!(!super::funds_limited_offer_fits_budget(
            &offer_in,
            &offer_out,
            &available,
            &available,
            &xrp(6_550_072),
        ));
    }

    #[test]
    fn ledger_105503215_output_fee_caps_maker_funds() {
        // CB282698: a 1.0001 SOLO transfer rate leaves less than the maker's
        // gross 647.151311454168 balance available to the taker. Flow consumes
        // the full line, pays 8,773,980 drops, and deletes the unfunded offer
        // with these nonzero residual fields.
        let offer_out = iou("647.151311454168");
        let offer_in = xrp(8_774_858);
        let net_available = net_book_payment_input(&offer_out, 1_000_100_000);
        assert_eq!(net_available.iou.to_decimal_string(), "647.0866027938886");
        assert_eq!(
            super::grossed(
                &net_available.iou,
                &IOUAmount::from_decimal_string("1.0001").unwrap(),
            ),
            offer_out.iou,
        );
        let quality = from_rate(0x5904_D133_DE36_2968).unwrap();
        let spent = in_for_out_round(&net_available, &quality, &offer_in, false);
        assert_eq!(spent.drops, 8_773_980);
        assert_eq!(
            leftover_leg(&offer_out, &net_available, true)
                .iou
                .to_decimal_string(),
            "0.0647086602794"
        );
        assert_eq!(leftover_leg(&offer_in, &spent, true).drops, 878);
    }

    #[test]
    fn ledger_105503215_fully_delivered_buy_has_no_dust_offer() {
        let requested = iou("2000");
        let raw_remainder = iou("0.0000000000004");
        let sold = xrp(40_000_000);
        let normalized = super::flow_buy_remaining_output(&raw_remainder, &requested, false, true);
        assert!(normalized.is_zero());
        assert_eq!(
            super::flow_buy_remaining_output(&iou("0.00000000004"), &requested, false, true)
                .iou
                .to_decimal_string(),
            "0.00000000004"
        );
        assert_eq!(
            super::flow_buy_remaining_output(&raw_remainder, &requested, true, true)
                .iou
                .to_decimal_string(),
            "0.0000000000004"
        );
        let quality = get_rate(&leg_as_quality_iou(&sold), &requested.iou).unwrap();
        let (pays, gets) = remaining_offer(
            false,
            quality,
            &normalized,
            &xrp(12_868_512),
            &requested,
            &sold,
            true,
        );
        assert_eq!(pays["value"], "0");
        assert_eq!(gets, serde_json::json!("0"));
    }

    #[test]
    fn ledger_105503439_tiny_ioc_fill_fails_realized_quality() {
        let limit = crossing_quality_threshold(&iou("0.079619"), &xrp(71_884), true, false);
        let realized =
            crossing_quality_threshold(&iou("0.00000664629441625"), &xrp(6), true, false);
        assert!(reject_quality(realized, limit));
    }

    #[test]
    fn ledger_105503449_fee_limited_offer_input_round_trip() {
        let offer_in = iou("746.836934443318");
        let quoted = iou("12.83858365582099");
        let remaining = leftover_leg(&offer_in, &quoted, true);
        assert_eq!(remaining.iou.to_decimal_string(), "733.998350787497");
        let credited = leftover_leg(&offer_in, &remaining, true);
        assert_eq!(credited.iou.to_decimal_string(), "12.838583655821");
    }

    #[test]
    fn ledger_105502136_terminal_amm_uses_number_fee_division() {
        let gross = IOUAmount::from_decimal_string("3104710.380568").unwrap();
        let rate = IOUAmount::from_decimal_string("1.005").unwrap();
        let _small = rxrpl_amount::number::MantissaScaleGuard::new(
            rxrpl_amount::number::MantissaScale::Small,
        );
        let down = flow_net_input(&Number::from_iou(&gross), Some(rate));
        let net = super::flow_net_input_terminal_forward(&Number::from_iou(&gross), rate);
        assert_eq!(down.to_iou().to_decimal_string(), "3089264.060266666");
        assert_eq!(net.to_iou().to_decimal_string(), "3089264.060266667");

        let pool_in =
            Number::from_iou(&IOUAmount::from_decimal_string("300399418.3320585").unwrap());
        let pool_out =
            Number::from_iou(&IOUAmount::from_decimal_string("823022.855571103").unwrap());
        let old_out = crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &down, 1000, false);
        let new_out = crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &net, 1000, false);
        assert_eq!(old_out.to_iou().to_decimal_string(), "8294.7601419893");
        assert_eq!(new_out.to_iou().to_decimal_string(), "8294.7601419896");
        // Both input ULPs settle to the same pool trust-line balance.
        assert_eq!(
            IOUAmount::add_round(&pool_in.to_iou(), &net.to_iou())
                .unwrap()
                .to_decimal_string(),
            "303488682.3923252"
        );
    }

    #[test]
    fn ledger_105502606_terminal_clob_keeps_forward_fee_digit() {
        let gross = Number::from_iou(&IOUAmount::from_decimal_string("0.6036034909578").unwrap());
        let rate = IOUAmount::from_decimal_string("1.002").unwrap();
        assert_eq!(
            flow_net_input(&gross, Some(rate))
                .to_iou()
                .to_decimal_string(),
            "0.6023986935706586"
        );
        assert_eq!(
            super::flow_net_input_terminal_forward(&gross, rate)
                .to_iou()
                .to_decimal_string(),
            "0.6023986935706587"
        );
    }

    #[test]
    fn ledger_105502166_source_capped_first_book_spends_full_funds() {
        let tx_send_max = iou("636.3656581626093");
        let source_funds = iou("423.8210353901292");
        let effective_send_max = leg_to_number(&source_funds);
        assert!(flow_source_funds_limit_binds(
            Some(&tx_send_max),
            &effective_send_max
        ));
        assert!(!flow_source_funds_limit_binds(
            Some(&source_funds),
            &effective_send_max
        ));
        let offer_pays = iou("835.1094273846246");
        let offer_gets = iou("46266.45027061632");
        let quality = from_rate(0x5306_69A3_4FD1_D000).unwrap();
        let output = out_for_in(&source_funds, &quality, &offer_gets);
        let repriced = in_for_out_offer(&offer_pays, &output, &offer_gets, &quality);
        assert_eq!(output.iou.to_decimal_string(), "23480.38977230632");
        assert_eq!(repriced.iou.to_decimal_string(), "423.8210353901291");

        let full_budget =
            flow_first_book_spends_source_funds(true, true, true, true, Some(parity_rate()), false);
        assert!(full_budget);
        assert!(!flow_first_book_spends_source_funds(
            false,
            true,
            true,
            true,
            Some(parity_rate()),
            false,
        ));
        assert!(!flow_first_book_spends_source_funds(
            true,
            false,
            true,
            true,
            Some(parity_rate()),
            false,
        ));
        assert!(!flow_first_book_spends_source_funds(
            true,
            true,
            false,
            true,
            Some(parity_rate()),
            false,
        ));
        assert!(!flow_first_book_spends_source_funds(
            true,
            true,
            true,
            false,
            Some(parity_rate()),
            false,
        ));
        assert!(!flow_first_book_spends_source_funds(
            true,
            true,
            true,
            true,
            Some(IOUAmount::from_decimal_string("1.005").unwrap()),
            false,
        ));
        assert!(!flow_first_book_spends_source_funds(
            true,
            true,
            true,
            true,
            Some(parity_rate()),
            true,
        ));
        let spent = if input_limited_uses_full_budget(false, false, true, &offer_gets, full_budget)
        {
            super::leg_min(&source_funds, &offer_pays)
        } else {
            super::leg_min(&repriced, &source_funds)
        };
        assert_eq!(spent.iou, source_funds.iou);
        assert_eq!(
            leftover_leg(&offer_pays, &spent, true)
                .iou
                .to_decimal_string(),
            "411.2883919944954"
        );
        assert_eq!(
            leftover_leg(&offer_gets, &output, true)
                .iou
                .to_decimal_string(),
            "22786.06049831"
        );
    }

    #[test]
    fn ledger_105502181_funded_bridge_xrp_exhausts_maker() {
        let offer_in = iou("22217.46261674315");
        let offer_out = xrp(360_744_345);
        let funded = xrp(713_587);
        let rate = from_rate(0x5015_E161_7D97_7263).unwrap();
        let proportional = bridge_funded_xrp_input(&offer_in, &funded, &offer_out, &rate, false);
        assert_eq!(proportional.iou.to_decimal_string(), "43.94827726625594");
        assert_eq!(out_for_in(&proportional, &rate, &offer_out).drops, 713_586);

        let quoted = bridge_funded_xrp_input(&offer_in, &funded, &offer_out, &rate, true);
        assert_eq!(quoted.iou.to_decimal_string(), "43.94827731577981");
        assert_eq!(out_for_in(&quoted, &rate, &offer_out).drops, 713_587);
        assert_eq!(
            leftover_leg(&offer_in, &quoted, true)
                .iou
                .to_decimal_string(),
            "22173.51433942737"
        );
        assert_eq!(offer_out.drops - funded.drops, 360_030_758);
        assert_eq!(3_113_587 - funded.drops, 2_400_000);
    }

    #[test]
    fn ledger_105500187_multi_offer_sell_settles_full_source_budget() {
        let source_balance = IOUAmount::from_decimal_string("1026.811695831073").unwrap();
        let budget = iou("1026.811695");
        let first = IOUAmount::from_decimal_string("1026.80024893322").unwrap();
        let second = IOUAmount::from_decimal_string("0.01144606677961959").unwrap();
        let spent = sum_smallest_to_largest(&[Number::from_iou(&first), Number::from_iou(&second)]);

        assert!(debit_drains_source(
            &budget,
            &super::number_to_leg(&spent, &budget)
        ));
        assert_eq!(
            aggregate_flow_source_balance_number(&source_balance, &spent, false)
                .to_decimal_string(),
            "0.00000083107338"
        );
        assert_eq!(
            aggregate_flow_source_balance_number(
                &source_balance,
                &Number::from_iou(&budget.iou),
                false,
            )
            .to_decimal_string(),
            "0.000000831073"
        );
    }

    #[test]
    fn ledger_105502451_funds_capped_ogre_sell_clears_one_ulp_overdraft() {
        let source = super::FlowSourceBalance {
            key: rxrpl_primitives::Hash256::new([0; 32]),
            balance: IOUAmount::from_decimal_string("2484.5").unwrap(),
            holder_is_high: false,
        };
        let budget = iou("2484.5");
        let final_balance = IOUAmount::from_decimal_string("-0.000000000001").unwrap();
        assert!(super::source_has_one_ulp_overdraft(
            &source,
            &budget,
            &final_balance
        ));
        assert!(!super::source_has_one_ulp_overdraft(
            &source,
            &budget,
            &IOUAmount::from_decimal_string("-0.000000000002").unwrap()
        ));
        assert!(!super::source_has_one_ulp_overdraft(
            &source,
            &budget,
            &IOUAmount::from_decimal_string("0.000000000001").unwrap()
        ));
        assert!(!super::source_has_one_ulp_overdraft(
            &source,
            &iou("2484.4"),
            &final_balance
        ));
        let high_source = super::FlowSourceBalance {
            key: source.key,
            balance: source.balance.negate(),
            holder_is_high: true,
        };
        assert!(super::source_has_one_ulp_overdraft(
            &high_source,
            &budget,
            &final_balance.negate()
        ));

        let mut ledger = rxrpl_ledger::Ledger::genesis();
        ledger
            .put_state(
                source.key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "RippleState",
                    "Balance": {"value": "-0.000000000001"},
                }))
                .unwrap(),
            )
            .unwrap();
        let fees = crate::fees::FeeSettings::default();
        let view = crate::view::ledger_view::LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = crate::view::sandbox::Sandbox::new(&view);
        let rules = rxrpl_amendment::Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = crate::transactor::ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        super::clear_one_ulp_source_overdraft(&mut ctx, &source, &budget);
        let line: serde_json::Value =
            serde_json::from_slice(&ctx.view.read(&source.key).unwrap()).unwrap();
        assert_eq!(line["Balance"]["value"], "0");
    }

    #[test]
    fn ledger_105502095_full_offer_take_preserves_source_remainder() {
        let source_funds = iou("518.9660669417391");
        let offer_pays = iou("518.9660669417388");
        let source_balance = source_funds.iou.negate();

        // The relative dust check alone would zero this high-account line.
        assert!(debit_drains_source(&source_funds, &offer_pays));
        assert!(!book_payment_dust_drain(true, true));
        assert!(book_payment_dust_drain(false, true));
        assert!(book_payment_dust_drain(true, false));
        assert_eq!(
            IOUAmount::add_round(&source_balance, &offer_pays.iou)
                .unwrap()
                .to_decimal_string(),
            "-0.0000000000003"
        );
    }

    #[test]
    fn ledger_105500212_parity_issue_does_not_round_funded_xrp_up() {
        let parity = parity_rate();
        let fee_rate = IOUAmount::from_decimal_string("1.002").unwrap();

        assert!(!funds_limited_xrp_rounds_up(
            true, true, 1_914_188, true, &parity
        ));
        assert!(!funds_limited_xrp_rounds_up(
            true, true, 1_914_188, false, &fee_rate
        ));
        assert!(funds_limited_xrp_rounds_up(
            true, true, 1_914_188, true, &fee_rate
        ));
    }

    #[test]
    fn ledger_105500212_funded_xrp_scales_before_dividing() {
        let offer_in = xrp(4_787_716);
        let available = iou("2.130004");
        let offer_out = iou("5.327509");

        assert_eq!(
            scale_funds_limited_input(&offer_in, &available, &offer_out).drops,
            1_914_188
        );
    }

    #[test]
    fn ledger_105500938_underfunded_xrp_uses_directory_quality() {
        let offer_in = xrp(192_198_956);
        let offer_out = iou("214.2469190929406");
        let available = iou("214.246918526333");
        let directory_rate = from_rate(0x5A1F_DEFE_770D_555E).unwrap();

        assert_eq!(
            in_for_out_round(&available, &directory_rate, &offer_in, false).drops,
            192_198_956
        );
        assert_eq!(
            scale_funds_limited_input(&offer_in, &available, &offer_out).drops,
            192_198_955
        );
    }

    #[test]
    fn ledger_105500212_allows_single_hop_iou_dust_below_one_drop() {
        let spent = xrp(0);
        let delivered = iou("0.0000009147578795");

        assert!(zero_cost_xrp_clob_fill(&spent, &delivered, true));
        assert!(!zero_cost_xrp_clob_fill(&spent, &delivered, false));
        assert!(!zero_cost_xrp_clob_fill(&spent, &iou("0"), true));
        assert!(!zero_cost_xrp_clob_fill(&xrp(1), &delivered, true));

        let offer_in = xrp(1);
        let offer_out = iou("1");
        assert!(zero_cost_funded_xrp_offer(
            &xrp(0),
            &xrp(0),
            &offer_in,
            &delivered,
            &offer_out,
        ));
        assert!(!zero_cost_funded_xrp_offer(
            &xrp(1),
            &xrp(0),
            &offer_in,
            &delivered,
            &offer_out,
        ));
        assert!(!zero_cost_funded_xrp_offer(
            &xrp(0),
            &xrp(1),
            &offer_in,
            &delivered,
            &offer_out,
        ));
    }

    #[test]
    fn ledger_105500592_stops_before_later_zero_cost_offer() {
        let offer_in = xrp(1_260_798_413);
        let offer_out = iou("134074406.52");
        let owner_funds = iou("0.0221");

        assert!(scale_funds_limited_input(&offer_in, &owner_funds, &offer_out).is_zero());
        assert!(!zero_cost_funded_xrp_offer(
            &xrp(711_124_873),
            &xrp(0),
            &offer_in,
            &owner_funds,
            &offer_out,
        ));
    }

    #[test]
    fn ledger_105500652_stops_before_reaping_later_unfunded_offer() {
        assert!(book_step_input_exhausted(
            &iou("1.16496250827378"),
            &iou("0")
        ));
        assert!(!book_step_input_exhausted(&iou("0"), &iou("0")));
        assert!(!book_step_input_exhausted(
            &iou("1.16496250827378"),
            &iou("0.00000000000001"),
        ));
    }

    #[test]
    fn ledgers_105500679_and_710_control_clob_after_amm() {
        assert!(!amm_step_allows_clob(false, &xrp(0)));
        assert!(amm_step_allows_clob(false, &xrp(1)));
        assert!(amm_step_allows_clob(true, &xrp(0)));

        assert!(!terminal_amm_reaches_deferred_clob(
            false,
            true,
            &iou("0"),
            &xrp(1),
        ));
        assert!(!terminal_amm_reaches_deferred_clob(
            true,
            false,
            &iou("0"),
            &xrp(1),
        ));
        assert!(!terminal_amm_reaches_deferred_clob(
            true,
            true,
            &iou("1"),
            &xrp(1),
        ));
        assert!(!terminal_amm_reaches_deferred_clob(
            true,
            true,
            &iou("0"),
            &xrp(0),
        ));
        assert!(terminal_amm_reaches_deferred_clob(
            true,
            true,
            &iou("0"),
            &xrp(1),
        ));
    }

    #[test]
    fn ledger_105500731_dry_clob_pass_stops_terminal_amm() {
        assert!(!clob_band_allows_terminal_amm(true, true, false));
        assert!(clob_band_allows_terminal_amm(true, true, true));
        assert!(clob_band_allows_terminal_amm(false, true, false));
    }

    #[test]
    fn ledger_105500743_rejects_rounded_clob_residual_after_amm() {
        let original_in = iou("46");
        let original_out = xrp(462_371);
        let residual_in = iou("30.57659758073063");
        let residual_out = xrp(307_342);

        let threshold = crossing_quality_threshold(&original_in, &original_out, true, false);
        let realized = crossing_quality_threshold(&residual_in, &residual_out, true, false);

        assert!(!reject_quality(threshold, threshold));
        assert!(reject_quality(realized, threshold));
    }

    #[test]
    fn ledger_105500749_source_funds_bind_before_xrp_rounding() {
        let source_funds = iou("0.00419404338858");
        let desired_out = xrp(28_077_251);
        let offer_out = xrp(134_435_250);
        let offer_in = iou("150");
        let rate = from_rate(0x4F03_F6CB_7E2B_609C).unwrap();

        let affordable_out = out_for_in(&source_funds, &rate, &offer_out);
        let repriced = in_for_out(&affordable_out, &rate, &offer_in);

        assert_eq!(affordable_out.drops, 3_758);
        assert!(terminal_budget_binds(
            &source_funds,
            &desired_out,
            &affordable_out,
            &offer_in,
            &offer_out,
            &rate,
            false,
        ));
        assert!(input_limited_uses_full_budget(
            true, false, false, &offer_out, false,
        ));
        assert_eq!(repriced.iou.to_decimal_string(), "0.004193096676652889");
        assert_eq!(
            leftover_leg(&source_funds, &repriced, true)
                .iou
                .to_decimal_string(),
            "0.000000946711927111"
        );
        assert!(leftover_leg(&source_funds, &source_funds, true).is_zero());
        assert_eq!(
            leftover_leg(&offer_in, &source_funds, true)
                .iou
                .to_decimal_string(),
            "149.9958059566114"
        );
        assert_eq!(offer_out.drops - affordable_out.drops, 134_431_492);
    }

    #[test]
    fn ledger_105500749_preserves_funded_self_offer_at_next_quality() {
        assert!(preserve_next_quality_self_offer(false, &xrp(0)));
        assert!(!preserve_next_quality_self_offer(false, &xrp(1)));
        assert!(!preserve_next_quality_self_offer(true, &xrp(0)));
    }

    #[test]
    fn ledger_105500074_sendmax_binds_below_requested_output() {
        let budget = xrp(10_000_000);
        let offer_in = xrp(8_899_212_829);
        let offer_out = iou("889921283.0178205");
        let demand = iou("1000000.000132394");
        let rate = from_rate(0x5603_8D7E_A4C6_8000).unwrap();
        let budget_out = out_for_in(&budget, &rate, &offer_out);

        assert_eq!(budget_out.iou.to_decimal_string(), "1000000");
        assert!(
            leg_ge(
                &budget,
                &in_for_out_offer(&offer_in, &demand, &offer_out, &rate)
            ),
            "proportional offer-field pricing reproduces the old non-binding decision"
        );
        assert!(terminal_budget_binds(
            &budget,
            &demand,
            &budget_out,
            &offer_in,
            &offer_out,
            &rate,
            false,
        ));
    }

    #[test]
    fn ledger_105500388_funded_output_beats_quality_floor() {
        let budget = xrp(900_890);
        let offer_in = xrp(738_865_700);
        let offer_out = iou("19317717.7763431");
        let funded = iou("23553.87758628668");
        let rate = from_rate(0x560d_96a4_9c6c_5b91).unwrap();
        let budget_out = out_for_in(&budget, &rate, &offer_out);

        assert_eq!(budget_out.iou.to_decimal_string(), "23553.85933807691");
        let funds_cost = scale_funds_limited_input(&offer_in, &funded, &offer_out);
        assert_eq!(funds_cost.drops, 900_890);
        assert!(leg_ge(&budget, &funds_cost));
        assert!(!terminal_budget_binds(
            &budget,
            &funded,
            &budget_out,
            &offer_in,
            &offer_out,
            &rate,
            true,
        ));
    }

    #[test]
    fn ledger_105500430_reduced_offer_preserves_quality() {
        let mut remaining = leg_to_number(&iou("10401.686462"));
        for fill in ["115.057164", "250.958543", "146.4945172319043"] {
            remaining = remaining.sub(&leg_to_number(&iou(fill)));
        }
        let consumed_out = number_to_leg(&remaining, &iou("0"));
        let remaining_out = leftover_leg(&iou("9994.4743452889"), &consumed_out, true);

        assert_eq!(consumed_out.iou.to_decimal_string(), "9889.176237768096");
        assert_eq!(remaining_out.iou.to_decimal_string(), "105.298107520804");
    }

    #[test]
    fn input_limited_iou_to_xrp_spends_full_budget_at_any_path_position() {
        assert!(input_limited_uses_full_budget(
            false,
            false,
            true,
            &xrp(1),
            false
        ));
        assert!(input_limited_uses_full_budget(
            false,
            true,
            true,
            &xrp(1),
            false
        ));
        assert!(input_limited_uses_full_budget(
            false,
            true,
            false,
            &xrp(1),
            false
        ));
        assert!(!input_limited_uses_full_budget(
            false,
            true,
            false,
            &iou("1"),
            false
        ));
        assert!(input_limited_uses_full_budget(
            false,
            true,
            false,
            &iou("1"),
            true
        ));
    }

    #[test]
    fn forward_xrp_carry_preserves_input_limited_iou_leftover() {
        assert!(preserve_flow_input_limited_iou_ulp(1, 0, true, false));
        assert!(preserve_flow_input_limited_iou_ulp(2, 0, true, false));
        assert!(!preserve_flow_input_limited_iou_ulp(0, 0, true, false));
        assert!(!preserve_flow_input_limited_iou_ulp(1, 1, true, false));
        assert!(!preserve_flow_input_limited_iou_ulp(1, 0, false, false));
        assert!(preserve_flow_input_limited_iou_ulp(1, 1, false, true));
    }

    #[test]
    fn xrp_rounding_adjustment_only_applies_to_interior_hops() {
        assert!(legacy_intermediate_xrp_rounding_applies(
            true,
            true,
            true,
            138_833_977,
            153_622_477,
            834_700_000,
        ));
        assert!(!legacy_intermediate_xrp_rounding_applies(
            false,
            true,
            true,
            138_833_977,
            153_622_477,
            834_700_000,
        ));
    }

    #[test]
    fn fulfilled_input_limited_hop_keeps_untrimmed_leftover() {
        let leftover = iou("321.5569861684434");
        let requested = iou("0.891729766029164");
        let partial = iou("0.8");

        assert!(!should_trim_input_limited_iou_leftover(
            true, false, false, &leftover, false, &requested, &requested
        ));
        assert!(!should_trim_input_limited_iou_leftover(
            true, false, true, &leftover, false, &requested, &requested
        ));
        assert!(should_trim_input_limited_iou_leftover(
            true, false, true, &leftover, false, &partial, &requested
        ));
        assert!(!should_trim_input_limited_iou_leftover(
            true, false, true, &leftover, true, &partial, &requested
        ));
    }

    #[test]
    fn ledger_105501907_interior_xah_offer_keeps_rounded_leftover() {
        let previous = iou("54617.53022130543");
        let filled = iou("7202.132638204216");
        let remaining = leftover_leg(&previous, &filled, true);
        assert_eq!(remaining.iou.to_decimal_string(), "47415.39758310121");
        assert!(!should_trim_input_limited_iou_leftover(
            true,
            true,
            true,
            &remaining,
            false,
            &filled,
            &iou("10000"),
        ));
    }

    #[test]
    fn ledger_105502845_mixed_amm_clob_sums_outputs_before_partial_take() {
        let requested = iou("10949.219057");
        let prior = [
            "0.098032",
            "99.946703",
            "0.172355",
            "0.229574",
            "0.100023",
            "320.881453",
            "218.6293587155448",
            "539.676994197",
        ]
        .map(|value| Number::from_iou(&IOUAmount::from_decimal_string(value).unwrap()));
        let (last_fill, _) = aggregate_output_remainder(&requested, &prior);
        assert_eq!(last_fill.iou.to_decimal_string(), "9769.484564087455");
        assert_eq!(
            leftover_leg(&iou("9995.8657958886"), &last_fill, true)
                .iou
                .to_decimal_string(),
            "226.381231801145"
        );
        let taker_before = IOUAmount::from_decimal_string("2709.846304599723").unwrap();
        assert_eq!(
            IOUAmount::add_round(&taker_before, &requested.iou)
                .unwrap()
                .to_decimal_string(),
            "13659.06536159972"
        );
    }

    #[test]
    fn ledger_105501947_uni_gross_cap_already_nets_remaining_demand() {
        let prior = [
            Number::from_iou(&IOUAmount::from_decimal_string("0.6741286976833167").unwrap()),
            Number::from_iou(&IOUAmount::from_decimal_string("0.6741286976833167").unwrap()),
        ];
        let requested = Number::from_iou(&IOUAmount::from_decimal_string("1.62").unwrap());
        let remaining = requested.sub(&sum_smallest_to_largest(&prior)).to_iou();
        assert_eq!(remaining.to_decimal_string(), "0.2717426046333666");
        let rate = IOUAmount::from_parts(1_001_000_000, -9, false).unwrap();
        let gross = super::grossed(&remaining, &rate);
        assert_eq!(gross.to_decimal_string(), "0.272014347238");
        let net = gross
            .mul_ratio(1_000_000_000, 1_001_000_000, false)
            .unwrap();
        assert_eq!(net.to_decimal_string(), remaining.to_decimal_string());
        assert!(!super::gross_book_output_cap_nets_short(
            &gross,
            &remaining,
            1_001_000_000,
        ));

        let earlier_demand = IOUAmount::from_decimal_string("0.03666999476872078").unwrap();
        let earlier_rate = IOUAmount::from_parts(1_003_000_000, -9, false).unwrap();
        let earlier_gross = super::grossed(&earlier_demand, &earlier_rate);
        assert!(super::gross_book_output_cap_nets_short(
            &earlier_gross,
            &earlier_demand,
            1_003_000_000,
        ));
    }

    #[test]
    fn self_payment_does_not_bump_partial_path_credit() {
        let taker = AccountId::from([1u8; 20]);
        let other = AccountId::from([2u8; 20]);

        assert!(!should_bump_partial_path_credit(
            true, false, &taker, &taker
        ));
        assert!(should_bump_partial_path_credit(true, false, &taker, &other));
        assert!(!should_bump_partial_path_credit(true, true, &taker, &other));
        assert!(!should_bump_partial_path_credit(
            false, false, &taker, &other
        ));
    }

    #[test]
    fn ledger_105507013_dust_remainder_credits_realised_delivery() {
        // TX80 ED6A2D9A: Payment to self XRP->XRPN. The offer and maker line
        // lose the realised amount, and DeliveredAmount is one ULP below Amount.
        // The destination line must agree with that oracle delivery.
        let demand = iou("1754.385964912281");
        let delivered = iou("1754.38596491228");
        let remaining = iou("0.000000000001");
        assert!(remaining_is_filled(&remaining, &demand));
        assert_eq!(
            book_destination_credit(&demand, &remaining, &delivered, true, false),
            delivered.iou
        );

        // A truly consumed demand still uses Amount to replace the sum of
        // truncated per-fill credits (30000046 63B72EB4).
        assert_eq!(
            book_destination_credit(&demand, &iou("0"), &delivered, true, false),
            demand.iou
        );
        assert_eq!(
            book_destination_credit(&demand, &remaining, &demand, true, false),
            demand.iou
        );
    }

    #[test]
    fn ledger_105502605_reverse_quoted_partial_delivery_keeps_demand_ulp() {
        // TX65 6BE62E3E: a partial XRP->SOLO self-payment asks for
        // 1.31710991664361 SOLO, with DeliverMin 0.851731079429536.
        // Flow's reverse quote caps this BookStep at the canonical partial
        // output, one ULP above the sum of its forward fills.
        let demand = iou("0.8780609555281941");
        let delivered = iou("0.878060955528194");
        let remaining = iou("0.0000000000000001");
        assert!(remaining_is_filled(&remaining, &demand));
        let credit = book_destination_credit(&demand, &remaining, &delivered, false, false);
        assert_eq!(credit, demand.iou);
        assert_eq!(
            add_iou_balance(&IOUAmount::ZERO, &credit.negate(), true)
                .unwrap()
                .to_decimal_string(),
            "-0.8780609555281941"
        );
    }

    #[test]
    fn ledger_30000020_eur_leftover_is_canonical_zero() {
        // IOC 4F0BD7AA: taker wants 50.0000000012541 EUR, maker has
        // 50.00000000125411. Mainnet leftover TakerGets is 0 and the offer is
        // deleted. STAmount 16-digit leftover must be zero.
        let offer = IOUAmount::from_decimal_string("50.00000000125411").unwrap();
        let take = IOUAmount::from_decimal_string("50.0000000012541").unwrap();
        let leftover = IOUAmount::sub(&offer, &take).unwrap();
        let part = Leg {
            is_xrp: false,
            drops: 0,
            iou: leftover,
            currency: [0u8; 20],
            issuer: AccountId::from([0u8; 20]),
        };
        let whole = Leg {
            is_xrp: false,
            drops: 0,
            iou: offer,
            currency: [0u8; 20],
            issuer: AccountId::from([0u8; 20]),
        };
        assert!(
            leftover_is_dust(&part, &whole),
            "leftover {} must be dust vs {}",
            leftover.to_decimal_string(),
            offer.to_decimal_string()
        );
    }

    #[test]
    fn ledger_30000046_jpy_tail_is_filled_at_minus_21_not_at_hop_start() {
        let demand = iou("25450");
        assert!(
            !remaining_is_filled_at(&demand, &demand, -21),
            "remaining=Amount must keep walking (B19E first)"
        );
        let after_b19e = iou("25166.91826601741");
        assert!(
            !remaining_is_filled_at(&after_b19e, &demand, -21),
            "after the 283 JPY B19E take the book still has work"
        );
        let tail = iou("0.08");
        assert!(
            remaining_is_filled_at(&tail, &demand, -21),
            "0.08/25450 is the extra-offer tail"
        );
        assert!(
            !remaining_is_filled(&tail, &demand),
            "0.08/25450 is not 1e-15 STAmount dust"
        );
    }

    #[test]
    fn ledger_30000015_threshold_rejects_best_inverse_btc_xrp() {
        // OfferCreate 7C16EAE4: TakerGets 23048078497 XRP / TakerPays 7.6826 BTC.
        // Inverse book tip rhK6GEk 8688 is ~70000 XRP/BTC; we must not take it.
        let threshold = get_rate(
            &IOUAmount::from_decimal_string("23048078497").unwrap(),
            &IOUAmount::from_decimal_string("7.68261600540501").unwrap(),
        )
        .unwrap();
        let inverse_tip = 0x5F18_DE4D_4B80_5BD1u64;
        assert!(
            reject_quality(inverse_tip, threshold),
            "inverse tip {inverse_tip:#x} must miss threshold {threshold:#x}"
        );
        let our_book = 0x4B0B_D79E_626E_0800u64;
        assert_eq!(
            offer_quality(
                &IOUAmount::from_decimal_string("7.68261600540501").unwrap(),
                &IOUAmount::from_decimal_string("23048078497").unwrap(),
            )
            .unwrap(),
            our_book
        );
        let btc = {
            let mut c = [0u8; 20];
            c[12..15].copy_from_slice(b"BTC");
            c
        };
        let issuer =
            rxrpl_codec::address::classic::decode_account_id("rLEsXccBGNR3UPuPu2hUXPjziKC3qKSBun")
                .unwrap();
        let xrp = [0u8; 20];
        let zero = AccountId::from([0u8; 20]);
        let inverse = keylet::book_dir(&xrp, &zero, &btc, &issuer);
        let same = keylet::book_dir(&btc, &issuer, &xrp, &zero);
        assert!(
            inverse.to_string().to_uppercase().starts_with("16CAFBAC"),
            "inverse {}",
            inverse
        );
        assert!(
            same.to_string().to_uppercase().starts_with("101FE75F"),
            "same {}",
            same
        );
    }

    #[test]
    fn ledger_30000004_taker_threshold_vs_maker_qualities() {
        let taker_q = get_rate(
            &IOUAmount::from_decimal_string("30658943169").unwrap(),
            &IOUAmount::from_decimal_string("9063").unwrap(),
        )
        .unwrap();
        let m1 = get_rate(
            &IOUAmount::from_decimal_string("409126752").unwrap(),
            &IOUAmount::from_decimal_string("120.9419652590728").unwrap(),
        )
        .unwrap();
        let m2 = get_rate(
            &IOUAmount::from_decimal_string("166222442").unwrap(),
            &IOUAmount::from_decimal_string("49.13701852804732").unwrap(),
        )
        .unwrap();
        let m3 = get_rate(
            &IOUAmount::from_decimal_string("1008259326").unwrap(),
            &IOUAmount::from_decimal_string("298.0515540012705").unwrap(),
        )
        .unwrap();
        assert_eq!(m3, 0x5b0c_04ab_b9f8_8eed);
        assert!(
            !reject_quality(m1, taker_q),
            "m1 {m1:#x} should pass threshold {taker_q:#x}"
        );
        assert!(
            !reject_quality(m2, taker_q),
            "m2 {m2:#x} should pass threshold {taker_q:#x}"
        );
        assert!(
            !reject_quality(m3, taker_q),
            "m3 {m3:#x} should pass threshold {taker_q:#x} (mainnet crossed F02D74BB)"
        );
    }

    #[test]
    fn passive_threshold_rejects_equal_quality() {
        let quality = 0x5503_8dad_1140_38e4;
        let passive = crossing_threshold(quality, true);

        assert!(reject_quality(quality, passive));
        assert!(!reject_quality(quality - 2, passive));
        assert_eq!(crossing_threshold(quality, false), quality);
    }

    #[test]
    fn ledger_105500369_rejects_out_of_quality_own_tip() {
        let threshold = 0x5107_73a8_7855_8652;
        let own_tip = 0x5811_2e96_d787_0186;

        assert!(!own_tip_can_be_reaped(own_tip, threshold, true));
        assert!(own_tip_can_be_reaped(threshold, threshold, true));
        assert!(own_tip_can_be_reaped(own_tip, threshold, false));
    }

    #[test]
    fn ledger_105500432_transfer_fee_admits_passive_own_tip() {
        let gets = IOUAmount::from_decimal_string("0.886").unwrap();
        let pays = IOUAmount::from_decimal_string("1593.30477").unwrap();
        let rate = IOUAmount::from_parts(1_001_500_000, -9, false).unwrap();
        let send_max = IOUAmount::mul_round(&gets, &rate, true).unwrap();
        let quality = get_rate_round_even(&send_max, &pays).unwrap();
        let threshold = crossing_threshold(quality, true);
        let own_tip = 0x5113_c4d3_a3b2_e906;

        assert!(
            !reject_quality(own_tip, threshold),
            "fee-adjusted passive threshold {threshold:#x} should admit own tip {own_tip:#x}"
        );
    }

    #[test]
    fn ledger_105500436_transfer_fee_rejects_external_clob_tip() {
        let gets = IOUAmount::from_decimal_string("7.247").unwrap();
        let pays = IOUAmount::from_decimal_string("13017.771606").unwrap();
        let rate = IOUAmount::from_parts(1_001_500_000, -9, false).unwrap();
        let send_max = IOUAmount::mul_round(&gets, &rate, true).unwrap();
        let traversal = crossing_threshold(get_rate_round_even(&send_max, &pays).unwrap(), true);
        let clob = crossing_threshold(get_rate_round_even(&gets, &pays).unwrap(), true);
        let external_tip = 0x5113_c9f6_5683_7af7;

        assert!(!reject_quality(external_tip, traversal));
        assert!(reject_quality(external_tip, clob));
    }

    #[test]
    fn bridge_uses_aggregate_quality() {
        let threshold = 0x5503_8dad_1140_38e4;
        let first_in = IOUAmount::from_decimal_string("1.643241424565478").unwrap();
        let first_out = IOUAmount::from_decimal_string("1.64375665128").unwrap();
        let total_in = IOUAmount::from_decimal_string("68.182047").unwrap();
        let total_out = IOUAmount::from_decimal_string("68.1046293541").unwrap();

        let first_quality = get_rate_round_even(&first_in, &first_out).unwrap();
        let aggregate_quality = get_rate_round_even(&total_in, &total_out).unwrap();

        assert!(rxrpl_amount::is_better_quality(first_quality, threshold));
        assert!(!rxrpl_amount::is_better_quality(
            aggregate_quality,
            threshold
        ));
    }

    #[test]
    fn maker_funds_gate_full_offer_take() {
        let offer_out = iou("2.325403");
        let funded_out = iou("0.696834");
        let offer_in = iou("2.088622");
        let remaining_in = iou("2.694554");
        let remaining_out = offer_out.clone();

        assert!(!full_offer_take(
            &funded_out,
            &funded_out,
            &remaining_in,
            &remaining_out,
            &offer_in,
            &offer_out,
        ));
        assert!(full_offer_take(
            &offer_out,
            &offer_out,
            &remaining_in,
            &remaining_out,
            &offer_in,
            &offer_out,
        ));
    }

    #[test]
    fn book_payment_source_budget_is_gross_of_transfer_fee() {
        let gross = iou("19.90518287280272");
        let net = net_book_payment_input(&gross, 1_000_100_000);
        assert_eq!(net.iou.to_decimal_string(), "19.90319255354736");

        let charged = gross_book_payment_input(&net, &gross, 1_000_100_000, true);
        assert_eq!(charged.iou, gross.iou);
    }

    #[test]
    fn ledger_105501174_sell_budget_caps_direct_xrp_take() {
        let source_funds = iou("0.3311164273572643");
        let rate = from_rate(0x501A98304FF13A42).unwrap();
        let net = net_offer_input_budget(&source_funds, &source_funds, 1_000_100_000);

        assert_eq!(out_for_in(&net, &rate, &xrp(10_000_000)).drops, 4_422);
        assert_eq!(
            out_for_in(&source_funds, &rate, &xrp(10_000_000)).drops,
            4_423
        );
        assert_eq!(
            gross_book_payment_input(&net, &source_funds, 1_000_100_000, true).iou,
            source_funds.iou
        );
    }

    #[test]
    fn path_payment_source_budget_is_net_of_transfer_fee() {
        let gross = iou("0.1987120275960489");
        let net = net_book_payment_input(&gross, 1_003_000_000);
        assert_eq!(net.iou.to_decimal_string(), "0.1981176745723319");

        let charged = gross_book_payment_input(&net, &gross, 1_003_000_000, true);
        assert_eq!(charged.iou, gross.iou);
    }

    #[test]
    fn amm_balance_settlement_rounds_halfway_sum_upward() {
        let tie = add_iou_balance_half_upward(
            &IOUAmount::from_decimal_string("-27.66112374802621").unwrap(),
            &IOUAmount::from_decimal_string("0.180231474079375").unwrap(),
        )
        .unwrap();
        assert_eq!(tie.to_decimal_string(), "-27.48089227394683");

        let coarse = add_iou_balance_half_upward(
            &IOUAmount::from_decimal_string("2849.407740863407").unwrap(),
            &IOUAmount::from_decimal_string("-0.9938176775876362").unwrap(),
        )
        .unwrap();
        assert_eq!(coarse.to_decimal_string(), "2848.413923185819");

        let positive_tie = add_iou_balance_half_upward(
            &IOUAmount::from_decimal_string("0.0373881425673948").unwrap(),
            &IOUAmount::from_decimal_string("-0.001542493738460875").unwrap(),
        )
        .unwrap();
        assert_eq!(positive_tie.to_decimal_string(), "0.03584564882893392");
    }

    #[test]
    fn amm_output_fee_preserves_sgb_precision() {
        let gross = rxrpl_amount::number::Number::from_iou(
            &IOUAmount::from_decimal_string("131.84642063").unwrap(),
        );
        let rate = IOUAmount::from_decimal_string("1.003").unwrap();
        assert_eq!(
            flow_net_input(&gross, Some(rate))
                .to_iou()
                .to_decimal_string(),
            "131.4520644366899"
        );
        assert_eq!(
            gross
                .to_iou()
                .mul_ratio(1_000_000_000, 1_003_000_000, false)
                .unwrap()
                .to_decimal_string(),
            "131.4520644366899"
        );
    }

    // select_path picks the better (smaller packed rate) of the direct tip and the
    // composed bridge quality; rippled uses direct when `bridge_q > direct_q`.
    #[test]
    fn select_path_prefers_better_quality() {
        assert_eq!(select_path(true, 100, false, 0), (true, 100)); // no bridge -> direct
        assert_eq!(select_path(false, 0, true, 200), (false, 200)); // no direct -> bridge
        assert_eq!(select_path(true, 100, true, 200), (true, 100)); // direct better (smaller)
        assert_eq!(select_path(true, 200, true, 100), (false, 100)); // bridge better
        assert_eq!(select_path(true, 100, true, 100), (false, 100)); // tie -> bridge
    }

    // composed_quality(q1, q2) = pack(multiply(rate1, rate2)): a 2.0 leg chained
    // with a 3.0 leg gives a 6.0 bridge.
    #[test]
    fn composed_quality_multiplies_rates() {
        let q2 = get_rate(
            &IOUAmount::from_decimal_string("2").unwrap(),
            &IOUAmount::from_decimal_string("1").unwrap(),
        )
        .unwrap();
        let q3 = get_rate(
            &IOUAmount::from_decimal_string("3").unwrap(),
            &IOUAmount::from_decimal_string("1").unwrap(),
        )
        .unwrap();
        let q6 = get_rate(
            &IOUAmount::from_decimal_string("6").unwrap(),
            &IOUAmount::from_decimal_string("1").unwrap(),
        )
        .unwrap();
        assert_eq!(composed_quality(q2, q3), q6);
    }

    #[test]
    fn limit_quality_rejects_composed_path_worse_than_sendmax_amount() {
        // 30000012 AE4D4DEA: SendMax 600.06 CNY / Amount 1482.4567 XLM.
        // After 30000010 took the 10 CNY band, leftover hop quality composes
        // strictly worse than that limit.
        let limit = get_rate(
            &IOUAmount::from_decimal_string("600.06").unwrap(),
            &IOUAmount::from_decimal_string("1482.456705882353").unwrap(),
        )
        .unwrap();
        let first_band = get_rate(
            &IOUAmount::from_decimal_string("10").unwrap(),
            &IOUAmount::from_decimal_string("4.200294").unwrap(),
        )
        .unwrap();
        let xlm_band = get_rate(
            &IOUAmount::from_decimal_string("4.200294").unwrap(),
            &IOUAmount::from_decimal_string("24.707884").unwrap(),
        )
        .unwrap();
        let ok = composed_quality(first_band, xlm_band);
        assert!(
            ok <= limit,
            "30000010 fill {ok:#x} must pass limit {limit:#x}"
        );
        let worse_cny = get_rate(
            &IOUAmount::from_decimal_string("20").unwrap(),
            &IOUAmount::from_decimal_string("4.200294").unwrap(),
        )
        .unwrap();
        let dry = composed_quality(worse_cny, xlm_band);
        assert!(
            dry > limit,
            "leftover band {dry:#x} must miss limit {limit:#x}"
        );
    }

    #[test]
    fn pack_rate_roundtrips_from_rate() {
        let q = get_rate(
            &IOUAmount::from_decimal_string("2.5").unwrap(),
            &IOUAmount::from_decimal_string("1").unwrap(),
        )
        .unwrap();
        assert_eq!(pack_rate(&from_rate(q).unwrap()), q);
    }

    // qual_mul computes input from output at the rate and clamps to the cap;
    // qual_div computes output from input and clamps.
    #[test]
    fn qual_mul_div_clamp_to_cap() {
        let r = parity_rate(); // 1.0
        // multiply(3, 1.0) = 3, clamped to cap 5 -> 3.
        assert_eq!(
            qual_mul(&iou("3"), &r, &iou("5")).iou.to_decimal_string(),
            "3"
        );
        // multiply(3, 1.0) = 3, clamped to cap 2 -> 2.
        assert_eq!(
            qual_mul(&iou("3"), &r, &iou("2")).iou.to_decimal_string(),
            "2"
        );
        // divide(6, 1.0) = 6, clamped to cap 10 -> 6.
        assert_eq!(
            qual_div(&iou("6"), &r, &iou("10")).iou.to_decimal_string(),
            "6"
        );
    }

    // flow_iou_to_iou owner-funds clamp: the owner can only give 40 of a 100/100
    // offer, so the fill scales to (in=40, out=40).
    #[test]
    fn flow_iou_to_iou_owner_funds_bound() {
        let f = flow_iou_to_iou(
            &iou("100"),         // offer_in
            &iou("100"),         // offer_out
            &rate("100", "100"), // rate 1.0
            &iou("40"),          // owner_funds (out asset)
            &iou("1000000"),     // taker_funds (in asset)
            &parity_rate(),      // in_rate
            &parity_rate(),      // out_rate
            false,               // buy
            &iou("1000000"),     // remaining_out
            &iou("1000000"),     // remaining_in
        );
        assert_eq!(f.order_out.iou.to_decimal_string(), "40");
        assert_eq!(f.order_in.iou.to_decimal_string(), "40");
    }

    // flow_iou_to_iou taker-input clamp: the taker has only 30 input left.
    #[test]
    fn flow_iou_to_iou_taker_input_bound() {
        let f = flow_iou_to_iou(
            &iou("100"),
            &iou("100"),
            &rate("100", "100"),
            &iou("1000000"),
            &iou("1000000"),
            &parity_rate(),
            &parity_rate(),
            false,
            &iou("1000000"),
            &iou("30"), // remaining_in
        );
        assert_eq!(f.order_in.iou.to_decimal_string(), "30");
        assert_eq!(f.order_out.iou.to_decimal_string(), "30");
    }

    // flow_iou_to_iou taker-output (demand) clamp at rate 2.0: a 50-unit demand
    // costs 100 input.
    #[test]
    fn flow_iou_to_iou_demand_bound_rate_two() {
        let f = flow_iou_to_iou(
            &iou("200"), // offer_in
            &iou("100"), // offer_out (rate 2.0)
            &rate("200", "100"),
            &iou("100"), // owner_funds (full)
            &iou("1000000"),
            &parity_rate(),
            &parity_rate(),
            false,
            &iou("50"), // remaining_out demand
            &iou("1000000"),
        );
        assert_eq!(f.order_out.iou.to_decimal_string(), "50");
        assert_eq!(f.order_in.iou.to_decimal_string(), "100");
    }

    // flow_xrp_to_iou taker-input clamp fires for a direct XrpToIou cross (taker's
    // remaining XRP input bounds the fill).
    #[test]
    fn flow_xrp_to_iou_input_bound() {
        let f = flow_xrp_to_iou(
            &xrp(200),   // offer_in (XRP the offer wants)
            &iou("100"), // offer_out (IOU, rate 2.0 in XRP/IOU)
            &rate("200", "100"),
            &iou("1000000"), // owner_funds (IOU out)
            &xrp(1_000_000), // taker_funds (XRP)
            &parity_rate(),  // out_rate
            false,
            &iou("1000000"), // remaining_out
            &xrp(50),        // remaining_in (XRP)
            TakerCrossType::XrpToIou,
        );
        assert_eq!(f.order_in.drops, 50);
        assert_eq!(f.order_out.iou.to_decimal_string(), "25"); // 50 / 2.0
    }

    // flow_iou_to_xrp owner (XRP) funds clamp: the owner can only pay out 30 XRP.
    #[test]
    fn flow_iou_to_xrp_owner_funds_bound() {
        let f = flow_iou_to_xrp(
            &iou("100"), // offer_in (IOU the offer wants)
            &xrp(100),   // offer_out (XRP, rate 1.0)
            &rate("100", "100"),
            &xrp(30),        // owner_funds (XRP)
            &iou("1000000"), // taker_funds (IOU)
            &parity_rate(),  // in_rate
            false,
            &xrp(1_000_000),
            &iou("1000000"),
            TakerCrossType::IouToXrp,
        );
        assert_eq!(f.order_out.drops, 30);
        assert_eq!(f.order_in.iou.to_decimal_string(), "30");
    }

    // remaining_offer rescales the leftover to the ORIGINAL quality. Original
    // offer TakerPays=100, TakerGets=200 (rate gets/pays = 2.0).
    #[test]
    fn remaining_offer_sell_rescales_to_original_quality() {
        let orig_q = get_rate(
            &IOUAmount::from_decimal_string("200").unwrap(), // gets
            &IOUAmount::from_decimal_string("100").unwrap(), // pays
        )
        .unwrap();
        let (pays, gets) = remaining_offer(
            true, // sell
            orig_q,
            &iou("30"), // remaining_out (pays) — ignored for sell
            &iou("50"), // remaining_in (gets)
            &iou("100"),
            &iou("200"),
            true,
        );
        // sell: gets = remaining_in = 50; pays = divRound(50, gets/pays=2.0) = 25.
        assert_eq!(gets["value"], "50");
        assert_eq!(pays["value"], "25");
    }

    #[test]
    fn ledger_105500434_sell_remainder_rounds_output_down() {
        let pays = iou("0.078815052");
        let gets = xrp(126_900_000);
        let orig_q =
            get_rate_round_even(&leg_as_quality_iou(&gets), &leg_as_quality_iou(&pays)).unwrap();

        let (remaining_pays, remaining_gets) = remaining_offer(
            true,
            orig_q,
            &iou("0.04880669731935999"),
            &xrp(78_583_592),
            &pays,
            &gets,
            true,
        );

        assert_eq!(remaining_gets.as_str(), Some("78583592"));
        assert_eq!(remaining_pays["value"], "0.04880669731935998");
    }

    #[test]
    fn remaining_offer_buy_rescales_to_original_quality() {
        let orig_q = get_rate(
            &IOUAmount::from_decimal_string("200").unwrap(),
            &IOUAmount::from_decimal_string("100").unwrap(),
        )
        .unwrap();
        let (pays, gets) = remaining_offer(
            false, // buy
            orig_q,
            &iou("30"), // remaining_out (pays)
            &iou("50"), // remaining_in (gets) — ignored for buy
            &iou("100"),
            &iou("200"),
            true,
        );
        // buy: pays = remaining_out = 30; gets = mulRound(30, gets/pays=2.0) = 60.
        assert_eq!(pays["value"], "30");
        assert_eq!(gets["value"], "60");
    }

    #[test]
    fn remaining_offer_buy_preserves_tick_snapped_testnet_quality() {
        // Testnet 20934193 / 9B345F1A: TickSize=6 snaps TakerGets to
        // 6.03172688340672 WAR. Two crossed offers leave 1,000,018 drops XRP.
        // Flow must derive the WAR remainder at the snapped original quality,
        // rather than subtracting the two makers' nominal WAR amounts.
        let gets = iou("6.03172688340672");
        let pays = xrp(12_000_000);
        let orig_q = get_rate(
            &gets.iou,
            &IOUAmount::from_parts(12_000_000, 0, false).unwrap(),
        )
        .unwrap();
        let (remaining_pays, remaining_gets) = remaining_offer(
            false,
            orig_q,
            &xrp(1_000_018),
            // `cross_offers`' independent subtraction produces this positive
            // value; the rescaling path intentionally ignores it for a buy.
            &iou("0.50768279238949"),
            &pays,
            &gets,
            true,
        );
        assert_eq!(remaining_pays, serde_json::json!("1000018"));
        assert_eq!(remaining_gets["value"], "0.5026529545408852");
    }

    #[test]
    fn ledger_30000008_payment_leftover_taker_pays() {
        // Payment D091EB7D (30000008): demand-limited take of 18985524 XRP
        // from offer 67FAFA4B. Mainnet leftover TakerPays is 2.512242529714367.
        let offer_in = iou("2.51453731");
        let take_out = xrp(18_985_524);
        let packed: u64 = 0x4B04_4B4E_6621_DCEF;
        let rate = from_rate(packed).unwrap();
        let priced = in_for_out(&take_out, &rate, &offer_in);
        let leftover_trunc = leftover_leg(&offer_in, &priced, false);
        let leftover_round = leftover_leg(&offer_in, &priced, true);
        let want = IOUAmount::from_decimal_string("2.512242529714367").unwrap();
        assert_eq!(
            leftover_trunc.iou.to_decimal_string(),
            want.to_decimal_string()
        );
        assert_eq!(leftover_round.iou.to_decimal_string(), "2.512242529714366");
    }

    #[test]
    fn remaining_offer_exhausted_and_untouched() {
        let orig_q = get_rate(
            &IOUAmount::from_decimal_string("200").unwrap(),
            &IOUAmount::from_decimal_string("100").unwrap(),
        )
        .unwrap();
        // Exhausted (a side is zero) -> zero residual.
        let (pays, gets) = remaining_offer(
            false,
            orig_q,
            &iou("0"),
            &iou("50"),
            &iou("100"),
            &iou("200"),
            true,
        );
        assert_eq!(pays["value"], "0");
        assert_eq!(gets["value"], "0");
        // Untouched (nothing crossed) -> original amounts.
        let (pays, gets) = remaining_offer(
            false,
            orig_q,
            &iou("100"),
            &iou("200"),
            &iou("100"),
            &iou("200"),
            false,
        );
        assert_eq!(pays["value"], "100");
        assert_eq!(gets["value"], "200");
    }

    #[test]
    fn sell_clamp_and_leg_helpers() {
        // sell overshoot clamps the remaining demand at zero.
        assert!(sell_clamp_sub(&iou("40"), &iou("60"), true).is_zero());
        assert_eq!(
            sell_clamp_sub(&iou("40"), &iou("10"), true)
                .iou
                .to_decimal_string(),
            "30"
        );
        assert!(leg_gt(&iou("5"), &iou("3")));
        assert!(!leg_gt(&iou("3"), &iou("3")));
        assert_eq!(leg_max(&xrp(7), &xrp(4)).drops, 7);
    }
}

#[cfg(test)]
mod tick_round_tests {
    use super::{rebuild_iou, round_drops_half_even};
    use rxrpl_amount::IOUAmount;

    // The tick re-derivation of an XRP side yields a fractional drops magnitude;
    // rebuild_iou must round it onto an integer drop the way rippled's
    // `STAmount(XRP)` does (round half to even), not truncate. Mainnet tx
    // DFF0E4CB snaps TakerGets 3654519 -> 3654430 (re-derived 3654430.39499415,
    // rounds down); tx 3CB55737 snaps 8543180 -> 8543055 (re-derived
    // 8543054.951085025, rounds up — truncation produced 8543054 and diverged).
    #[test]
    fn rebuild_iou_rounds_xrp_drops_half_even() {
        let original = serde_json::json!("3654519");
        let redrived = IOUAmount::from_decimal_string("3654430.39499415").unwrap();
        assert_eq!(
            rebuild_iou(&original, &redrived),
            Some(serde_json::json!("3654430"))
        );

        let original = serde_json::json!("8543180");
        let redrived = IOUAmount::from_decimal_string("8543054.951085025").unwrap();
        assert_eq!(
            rebuild_iou(&original, &redrived),
            Some(serde_json::json!("8543055"))
        );
    }

    #[test]
    fn round_drops_ties_to_even() {
        assert_eq!(round_drops_half_even("10.5"), Some(10));
        assert_eq!(round_drops_half_even("11.5"), Some(12));
        assert_eq!(round_drops_half_even("10.5000001"), Some(11));
        assert_eq!(round_drops_half_even("10.4999999"), Some(10));
        assert_eq!(round_drops_half_even("42"), Some(42));
    }
}

#[cfg(test)]
mod book_directory_quality_tests {
    use super::offer_book_quality;

    // Known-answer vectors for the helper (mainnet DFF0E4CB / 105333100).
    // apply() keys BookDirectory on the ORIGINAL tick-snapped amounts (rippled
    // uRate); leftover vs original still hash-differ when TakerPays is not
    // scaled with TakerGets.
    #[test]
    fn book_quality_known_answers_original_and_leftover() {
        let zrp = serde_json::json!({
            "currency": "ZRP",
            "issuer": "rZapJ1PZ297QAEXRGu3SZkAiwXbA7BNoe",
            "value": "140.55304742187"
        });
        let remaining_gets = serde_json::json!("3654430");
        let original_gets = serde_json::json!("3654519");
        assert_eq!(
            offer_book_quality(&zrp, &remaining_gets, true),
            0x500DAA02090C875D
        );
        assert_eq!(
            offer_book_quality(&zrp, &original_gets, true),
            0x500DA9EC3A23F7E9
        );
    }

    // Mainnet 30000004 / 681FEF4C: pre-fixUniversalNumber getRate of the
    // original TakerPays/TakerGets. Placement must use this, not the leftover.
    #[test]
    fn pre_number_original_rate_pins_30000004() {
        let usd = serde_json::json!({
            "currency": "USD",
            "issuer": "rvYAfWj5gh67oV6fW32ZzP3Aw4Eubs59B",
            "value": "9063"
        });
        let xrp = serde_json::json!("30658943169");
        assert_eq!(
            offer_book_quality(&usd, &xrp, false),
            5_623_448_405_541_126_236
        );
    }
}

#[cfg(test)]
mod amm_quality_gate_tests {
    use super::{adjusted_amm_quality_allows, amm_offer_allows_partial_output, num_quality_iou};
    use rxrpl_amount::number::Number;

    // Mainnet tx 062FDE11 (ledger 105333100): offer sells 1061920 USD for
    // 1e12 drops XRP => limit quality (in/out rate) = get_rate(1061920, 1e12).
    // The USD/XRP AMM (4014336154 drops XRP + 4363.056921446038 USD) has a spot
    // quality of only 920074 drops/USD, worse than the offer's 941690, so the
    // gate must refuse the cross (amm_hop returns None) and the offer rests.
    #[test]
    fn amm_worse_than_offer_quality_is_gated_out() {
        // AMM spot rate: in = pool USD (received), out = pool XRP (paid).
        let pool_in = Number::from_iou(
            &rxrpl_amount::IOUAmount::from_decimal_string("4363.056921446038").unwrap(),
        );
        let pool_out = Number::from_int(4_014_336_154); // XRP drops
        let spq = rxrpl_amount::get_rate(
            &num_quality_iou(&pool_in, false),
            &num_quality_iou(&pool_out, true),
        )
        .unwrap();
        // Offer limit: in = TakerGets 1061920 USD, out = TakerPays 1e12 drops.
        let offer_in =
            Number::from_iou(&rxrpl_amount::IOUAmount::from_decimal_string("1061920").unwrap());
        let offer_out = Number::from_int(1_000_000_000_000);
        let cq = rxrpl_amount::get_rate(
            &num_quality_iou(&offer_in, false),
            &num_quality_iou(&offer_out, true),
        )
        .unwrap();
        // Gate refuses when the AMM does not strictly beat the offer quality.
        assert!(
            !rxrpl_amount::is_better_quality(spq, cq)
                || rxrpl_amount::within_relative_distance(spq, cq),
            "AMM spot must not beat the offer limit -> no cross"
        );
    }

    #[test]
    fn fok_amm_step_can_combine_partial_liquidity() {
        assert!(
            amm_offer_allows_partial_output(true, true),
            "tfSell + FOK must spend the exact TakerGets input through the AMM"
        );
        assert!(
            amm_offer_allows_partial_output(true, false),
            "a regular FOK may combine a partial AMM step with other liquidity"
        );
    }

    // Testnet ledger 105500478: the average-quality cap produces a synthetic
    // XRP/WETH quote whose integral XRP input rounds one drop above the exact
    // boundary. That worsens the realised quality by more than 1e-7, so
    // StrandFlow rejects the whole path and leaves the offer untouched.
    #[test]
    fn rounded_xrp_input_below_limit_quality_is_rejected() {
        let pool_in = Number::from_int(96_846_030_630);
        let pool_out = Number::from_iou(
            &rxrpl_amount::IOUAmount::from_decimal_string("60.71653104558634").unwrap(),
        );
        let target = 0x5E05_B95D_F5C6_CFE6;
        let (input, output) = crate::amm_helpers::limit_input_quality(
            &pool_in, &pool_out, target, 1_000, true, false,
        )
        .unwrap();

        assert_eq!(input.to_decimal_string(), "1478330");
        assert_eq!(output.to_decimal_string(), "0.0009175397117970489");
        assert!(!adjusted_amm_quality_allows(
            &input, true, &output, false, target, true,
        ));
    }

    // Testnet ledger 105500477: the corresponding ARMY/XRP tail remains
    // within the quality tolerance after XRP output rounding and must still be
    // accepted. This pins the byte-exact predecessor checkpoint.
    #[test]
    fn rounded_xrp_output_within_limit_quality_is_allowed() {
        let pool_in = Number::from_iou(
            &rxrpl_amount::IOUAmount::from_decimal_string("44014238.41050589").unwrap(),
        );
        let pool_out = Number::from_int(230_161_408_450);
        let target = 0x5106_CB90_95F0_CBEB;
        let (input, output) =
            crate::amm_helpers::limit_input_quality(&pool_in, &pool_out, target, 0, false, true)
                .unwrap();

        assert_eq!(input.to_decimal_string(), "8083.09352975");
        assert_eq!(output.to_decimal_string(), "42260747");
        assert!(adjusted_amm_quality_allows(
            &input, false, &output, true, target, true,
        ));
    }

    // Testnet ledger 105500536, 6FB245AE: the USDC issuer charges 0.3%.
    // limit_input_quality sizes the pool's net input at the already-grossed
    // sendMax quality, but StrandFlow evaluates the source's gross debit. The
    // extra transfer fee pushes the realised strand below the offer limit, so
    // the AMM path is dry and the complete offer must rest on the book.
    #[test]
    fn transfer_fee_is_included_in_tail_amm_quality() {
        let pool_in = Number::from_iou(
            &rxrpl_amount::IOUAmount::from_decimal_string("50269.83163890537").unwrap(),
        );
        let pool_out = Number::from_int(45_266_478_323);
        let offer_in = rxrpl_amount::IOUAmount::from_decimal_string("9974.282158").unwrap();
        let offer_out = rxrpl_amount::IOUAmount::from_decimal_string("8989149974").unwrap();
        let transfer_rate = rxrpl_amount::IOUAmount::from_decimal_string("1.003").unwrap();
        let gross_limit =
            rxrpl_amount::IOUAmount::mul_round(&offer_in, &transfer_rate, true).unwrap();
        let target = rxrpl_amount::get_rate_round_even(&gross_limit, &offer_out).unwrap();
        let (pool_input, output) =
            crate::amm_helpers::limit_input_quality(&pool_in, &pool_out, target, 54, false, true)
                .unwrap();

        assert_eq!(pool_input.to_decimal_string(), "80.97812541029156");
        assert_eq!(output.to_xrp_drops_mode(), 72_761_855);
        assert!(adjusted_amm_quality_allows(
            &pool_input,
            false,
            &output,
            true,
            target,
            true,
        ));

        let source_input = Number::from_iou(
            &rxrpl_amount::IOUAmount::mul_round(&pool_input.to_iou(), &transfer_rate, true)
                .unwrap(),
        );
        assert_eq!(source_input.to_decimal_string(), "81.22105978652244");
        assert!(!adjusted_amm_quality_allows(
            &source_input,
            false,
            &output,
            true,
            target,
            true,
        ));
    }
}

#[cfg(test)]
mod flow_source_balance_tests {
    use super::{
        Leg, aggregate_flow_destination_balance_passes, aggregate_flow_source_balance_number,
        aggregate_flow_source_balance_passes, can_fund_new_trust_line, flow_gross_input,
        flow_holder_funds, flow_input_limit, flow_net_input, flow_source_pass_budget, leftover_leg,
        next_flow_strands, preserve_flow_output_ulp, sum_smallest_to_largest,
    };
    use rxrpl_amount::IOUAmount;
    use rxrpl_amount::number::{MantissaScale, MantissaScaleGuard, Number};

    #[test]
    fn aggregates_multi_pass_source_debit_before_rounding() {
        let balance = IOUAmount::from_decimal_string("0.28961595565").unwrap();
        let spent = IOUAmount::from_decimal_string("0.2872378014634366").unwrap();

        assert_eq!(
            aggregate_flow_source_balance_number(&balance, &Number::from_iou(&spent), false)
                .to_decimal_string(),
            "0.0023781541865634"
        );
    }

    #[test]
    fn ledger_105500929_single_pass_source_uses_full_gross_input() {
        let balance = IOUAmount::from_decimal_string("-0.11365554335").unwrap();
        let spent = IOUAmount::from_decimal_string("0.1136555433").unwrap();
        let saved_ins = [Number::from_iou(&spent)];

        assert_eq!(
            aggregate_flow_source_balance_passes(&balance, &saved_ins, true)
                .unwrap()
                .to_decimal_string(),
            "-0.00000000005"
        );
    }

    #[test]
    fn self_payment_multi_pass_rounding_matches_flow() {
        let balance = IOUAmount::from_decimal_string("178717.8318043616").unwrap();
        let saved_ins = [
            "0.009895616380142929",
            "0.009900648259554282",
            "0.01982124843769783",
            "0.02977699115129875",
            "0.04975383267345824",
            "0.07992907373162355",
        ]
        .map(|value| Number::from_iou(&IOUAmount::from_decimal_string(value).unwrap()));
        let saved_outs = [
            "0.00000562869231097",
            "0.00000562869231097",
            "0.00001125738462194",
            "0.00001688607693291",
            "0.00002814346155485",
            "0.00004502953848776",
        ]
        .map(|value| Number::from_iou(&IOUAmount::from_decimal_string(value).unwrap()));
        let _scale = MantissaScaleGuard::new(MantissaScale::Small);
        let input = sum_smallest_to_largest(&saved_ins);
        let output = sum_smallest_to_largest(&saved_outs);
        assert_eq!(input.to_iou().to_decimal_string(), "0.1990774106337756");
        assert_eq!(output.to_iou().to_decimal_string(), "0.0001125738462194");
        assert_eq!(
            aggregate_flow_source_balance_number(&balance, &input, false).to_decimal_string(),
            "178717.632726951"
        );
        assert_eq!(
            aggregate_flow_source_balance_passes(&balance, &saved_ins, false)
                .unwrap()
                .to_decimal_string(),
            "178717.6327269509"
        );
        assert!(preserve_flow_output_ulp(
            &output,
            &Number::from_int(1),
            true
        ));
    }

    #[test]
    fn ledger_105500595_replays_destination_credits_in_pass_order() {
        let balance = IOUAmount::from_decimal_string("-200000000000").unwrap();
        let saved_outs = [
            "975339.266587",
            "17552924.87249",
            "1950678.533174",
            "2926017.799761",
            "87764624.36245",
            "7802714.132696",
            "12679410.465631",
            "368611422.32229",
            "33161535.063958",
            "53643659.662285",
            "1562210313.65161",
            "140448854.388528",
            "227254049.114771",
            "3740091895.31832",
        ]
        .map(|value| Number::from_iou(&IOUAmount::from_decimal_string(value).unwrap()));
        let _scale = MantissaScaleGuard::new(MantissaScale::Small);

        let aggregate = sum_smallest_to_largest(&saved_outs);
        assert_eq!(aggregate.to_iou().to_decimal_string(), "6257073438.954551");
        assert_eq!(
            IOUAmount::add_round(&balance, &aggregate.to_iou().negate())
                .unwrap()
                .to_decimal_string(),
            "-206257073438.9546"
        );
        assert_eq!(
            aggregate_flow_destination_balance_passes(&balance, &saved_outs, true)
                .unwrap()
                .to_decimal_string(),
            "-206257073438.9547"
        );
    }

    #[test]
    fn preserves_sub_iou_ulp_source_remainder_across_flow_passes() {
        let balance = IOUAmount::from_decimal_string("14.2391130014").unwrap();
        let spent = ["5.153878281024355", "6.340204940719714", "2.74502977965593"]
            .map(|value| Number::from_iou(&IOUAmount::from_decimal_string(value).unwrap()));
        let _scale = MantissaScaleGuard::new(MantissaScale::Small);
        let input_limit = Number::from_iou(&balance);
        let aggregate = sum_smallest_to_largest(&spent);

        assert!(aggregate.sub(&input_limit).is_zero());
        assert_eq!(
            aggregate_flow_source_balance_passes(&balance, &spent, false)
                .unwrap()
                .to_decimal_string(),
            "0.000000000000001"
        );
    }

    #[test]
    fn ledger_105503522_caps_flow_pass_at_live_source_funds() {
        let _scale = MantissaScaleGuard::new(MantissaScale::Small);
        let accounting =
            Number::from_iou(&IOUAmount::from_decimal_string("130.903458197998").unwrap());
        let live = Number::from_iou(&IOUAmount::from_decimal_string("130.9034581979979").unwrap());
        assert_eq!(
            flow_source_pass_budget(accounting, Some(live))
                .to_iou()
                .to_decimal_string(),
            "130.9034581979979"
        );
        assert_eq!(flow_source_pass_budget(accounting, None), accounting);
        assert_eq!(flow_source_pass_budget(live, Some(accounting)), live);
    }

    #[test]
    fn source_funds_follow_trust_line_orientation() {
        let positive = IOUAmount::from_decimal_string("1031126.522400319").unwrap();
        let negative = positive.negate();

        assert_eq!(
            flow_holder_funds(&positive, false)
                .to_iou()
                .to_decimal_string(),
            "1031126.522400319"
        );
        assert_eq!(
            flow_holder_funds(&negative, true)
                .to_iou()
                .to_decimal_string(),
            "1031126.522400319"
        );
        assert!(flow_holder_funds(&negative, false).is_zero());
    }

    #[test]
    fn transfer_rates_match_ledger_105500237_flow_amounts() {
        let rate = IOUAmount::from_parts(1_001_000_000, -9, false).unwrap();
        let source_net =
            Number::from_iou(&IOUAmount::from_decimal_string("8.438055169911182").unwrap());
        assert_eq!(
            flow_gross_input(&source_net, Some(rate))
                .to_iou()
                .to_decimal_string(),
            "8.446493225081094"
        );

        let terminal_gross =
            Number::from_iou(&IOUAmount::from_decimal_string("0.1531269004849731").unwrap());
        let book_net = flow_net_input(&terminal_gross, Some(rate));
        assert_eq!(book_net.to_iou().to_decimal_string(), "0.1529739265584146");
    }

    #[test]
    fn ledger_105500360_transfer_fee_source_is_funds_capped() {
        let send_max =
            Number::from_iou(&IOUAmount::from_decimal_string("0.000002190135839932516").unwrap());
        let source = super::FlowSourceBalance {
            key: rxrpl_primitives::Hash256::new([0; 32]),
            balance: IOUAmount::from_decimal_string("0.00000146009055995501").unwrap(),
            holder_is_high: false,
        };
        let limit = flow_input_limit(&send_max, Some(&source));
        let rate = IOUAmount::from_parts(1_001_500_000, -9, false).unwrap();

        assert_eq!(limit.to_iou().to_decimal_string(), "0.00000146009055995501");
        assert_eq!(
            flow_net_input(&limit, Some(rate))
                .to_iou()
                .to_decimal_string(),
            "0.000001457903704398412"
        );
    }

    #[test]
    fn active_strands_drop_failed_prefix_after_winner() {
        assert_eq!(next_flow_strands(&[1, 5, 2], 1), vec![5, 2]);
        assert_eq!(next_flow_strands(&[2, 5], 0), vec![2, 5]);
    }

    #[test]
    fn sell_remainder_rounds_before_crossing_the_next_offer() {
        let leg = |value: &str| {
            Leg::parse(&serde_json::json!({
                "currency": "BBRL",
                "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
                "value": value,
            }))
            .unwrap()
        };
        let remaining = leftover_leg(&leg("1026.811695"), &leg("0.01144606677961959"), true);

        assert_eq!(remaining.iou.to_decimal_string(), "1026.80024893322");
    }

    #[test]
    fn preserves_exact_requested_delivery() {
        let delivered =
            Number::from_iou(&IOUAmount::from_decimal_string("6762.885132533594").unwrap());
        assert!(preserve_flow_output_ulp(&delivered, &delivered, false));
    }

    #[test]
    fn rejects_unfunded_implicit_trust_line_after_bootstrap() {
        let account = serde_json::json!({"Balance": "2635820", "OwnerCount": 8});
        assert!(!can_fund_new_trust_line(&account, 2_800_000));
    }

    #[test]
    fn permits_bootstrap_or_funded_implicit_trust_line() {
        let bootstrap = serde_json::json!({"Balance": "1000000", "OwnerCount": 1});
        assert!(can_fund_new_trust_line(&bootstrap, 1_400_000));

        let funded = serde_json::json!({"Balance": "2800000", "OwnerCount": 8});
        assert!(can_fund_new_trust_line(&funded, 2_800_000));
    }
}

#[cfg(test)]
mod amm_transfer_fee_tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ISSUER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const POOL: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    const RECIPIENT: &str = "rGWrZyQqhTp9Xu7G5Pkayo7bXjH4k4QYpf";

    #[test]
    fn btc_amm_input_ratio_preserves_source_balance_ulp() {
        // Ledger 105503160/TX21: the pool receives this exact IOU input for
        // 62,602,418 XRP drops. BookStep's input fee has a ratio remainder
        // after its 64-bit expansion and IOU normalization.
        let source_before = IOUAmount::from_decimal_string("-0.001660967674287171").unwrap();
        let pool_before = IOUAmount::from_decimal_string("1.76720087586459").unwrap();
        let (pool_input, output) = crate::amm_helpers::limit_input_quality(
            &Number::from_iou(&pool_before),
            &Number::from_int(102_536_221_174),
            0x4A06_20C2_97A2_3C00,
            19,
            false,
            true,
        )
        .unwrap();
        assert_eq!(output.to_xrp_drops(), 62_602_418);
        let pool_net = pool_input.to_iou();
        assert_eq!(pool_net.to_decimal_string(), "0.001079810327359199");
        let gross = gross_amm_book_input(&pool_net, 1_001_500_000);
        assert_eq!(gross.to_decimal_string(), "0.001081430042850239");
        assert_eq!(
            IOUAmount::add_round(&source_before, &gross)
                .unwrap()
                .to_decimal_string(),
            "-0.000579537631436932"
        );
        assert_eq!(
            IOUAmount::add_round(&pool_before, &pool_net)
                .unwrap()
                .to_decimal_string(),
            "1.768280686191949"
        );
        assert_eq!(
            gross_amm_book_input(&IOUAmount::from_decimal_string("1").unwrap(), 1_001_500_000)
                .to_decimal_string(),
            "1.0015"
        );
    }

    #[test]
    fn amm_input_reports_the_source_gross_spend() {
        let pool_net = IOUAmount::from_decimal_string("1.066554203744208").unwrap();
        let rate = IOUAmount::from_parts(1_002_000_000, -9, false).unwrap();

        assert_eq!(
            grossed(&pool_net, &rate).to_decimal_string(),
            "1.068687312151697"
        );
    }

    fn put_line(ledger: &mut Ledger, holder: &str, value: &str) {
        let holder_id = decode_account_id(holder).unwrap();
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
        let amount = IOUAmount::from_decimal_string(value).unwrap();
        let stored = if holder_is_low {
            amount
        } else {
            amount.negate()
        };
        let (low, high) = if holder_is_low {
            (holder, ISSUER)
        } else {
            (ISSUER, holder)
        };
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored.to_decimal_string()},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&holder_id, &issuer_id, &currency),
                serde_json::to_vec(&line).unwrap(),
            )
            .unwrap();
    }

    fn holding(view: &dyn ReadView, holder: &str) -> IOUAmount {
        let holder_id = decode_account_id(holder).unwrap();
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let key = keylet::trust_line(&holder_id, &issuer_id, &helpers::currency_to_bytes("USD"));
        let line: Value = serde_json::from_slice(&view.read(&key).unwrap()).unwrap();
        let stored =
            IOUAmount::from_decimal_string(line["Balance"]["value"].as_str().unwrap()).unwrap();
        if holder_id.as_bytes() < issuer_id.as_bytes() {
            stored
        } else {
            stored.negate()
        }
    }

    #[test]
    fn offer_cross_amm_waives_output_fee_while_payment_hop_retains_it() {
        let demand = "0.0004810006089389414";
        let mut offer_pool_after = None;
        for offer_crossing in [true, false] {
            let mut ledger = Ledger::genesis();
            for (address, rate) in [
                (ISSUER, Some(1_001_500_000u64)),
                (POOL, None),
                (RECIPIENT, None),
            ] {
                let id = decode_account_id(address).unwrap();
                let mut account = serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": address,
                    "Balance": "100000000", "Sequence": 1, "OwnerCount": 0, "Flags": 0,
                });
                if let Some(rate) = rate {
                    account["TransferRate"] = Value::from(rate);
                }
                ledger
                    .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                    .unwrap();
            }
            put_line(&mut ledger, POOL, "10");
            put_line(&mut ledger, RECIPIENT, "0");
            let input = Leg::parse(&serde_json::json!("1000000")).unwrap();
            let output = Leg::parse(&serde_json::json!({
                "currency": "USD", "issuer": ISSUER, "value": demand
            }))
            .unwrap();
            let taker = decode_account_id(RECIPIENT).unwrap();
            ledger
                .put_state(
                    keylet::amm(
                        &input.currency,
                        &input.issuer,
                        &output.currency,
                        &output.issuer,
                    ),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AMM", "Account": POOL, "TradingFee": 0
                    }))
                    .unwrap(),
                )
                .unwrap();

            let fees = FeeSettings::default();
            let view = LedgerView::with_fees(&ledger, fees.clone());
            let mut sandbox = Sandbox::new(&view);
            let rules = Rules::new();
            let tx = serde_json::json!({});
            let mut taker_account = serde_json::json!({"Balance": "100000000"});
            let mut ctx = ApplyContext {
                tx: &tx,
                view: &mut sandbox,
                rules: &rules,
                fees: &fees,
            };
            let delivered = if offer_crossing {
                let mut remaining_out = output.clone();
                let mut remaining_in = input.clone();
                let mut source_remaining = input.clone();
                let mut crossed = false;
                let quality_input = Leg::parse(&serde_json::json!("20000000")).unwrap();
                let quality_output = Leg::parse(&serde_json::json!({
                    "currency": "USD", "issuer": ISSUER, "value": "1"
                }))
                .unwrap();
                let cq = crossing_quality_threshold(&quality_input, &quality_output, false, false);
                try_amm_step(
                    &mut ctx,
                    &taker,
                    &mut taker_account,
                    &mut remaining_out,
                    &mut remaining_in,
                    &mut source_remaining,
                    &mut crossed,
                    cq,
                    false,
                    false,
                    false,
                )
                .unwrap();
                assert!(crossed);
                assert!(remaining_out.is_zero());
                output.clone()
            } else {
                let AmmHopResult::Filled(delivered, _) = amm_hop(
                    &mut ctx,
                    &taker,
                    &mut taker_account,
                    &taker,
                    &output,
                    &input,
                    false,
                    false,
                    false,
                    true,
                    false,
                    None,
                    None,
                    false,
                    false,
                )
                .unwrap() else {
                    panic!("AMM quote should fill");
                };
                delivered
            };
            let pool_after = holding(&sandbox, POOL);
            assert!(pool_after < IOUAmount::from_decimal_string("10").unwrap());
            assert_eq!(holding(&sandbox, RECIPIENT), delivered.iou);
            if offer_crossing {
                assert_eq!(delivered.iou.to_decimal_string(), demand);
                offer_pool_after = Some(pool_after);
            } else {
                assert_eq!(Some(pool_after), offer_pool_after);
                assert_eq!(delivered.iou.to_decimal_string(), "0.0004802801886559575");
            }
        }
    }

    #[test]
    fn ledger_105502369_terminal_book_output_keeps_ratio_guard_digits() {
        assert!(matches!(
            super::book_output_fee_rounding(true, false),
            TransferFeeRounding::BookStepRatio
        ));
        assert!(matches!(
            super::book_output_fee_rounding(false, false),
            TransferFeeRounding::Down
        ));
        assert!(matches!(
            super::book_output_fee_rounding(true, true),
            TransferFeeRounding::Down
        ));
        let gross = IOUAmount::from_decimal_string("1067.201303501945").unwrap();
        let truncated = gross
            .mul_ratio(1_000_000_000, 1_002_000_000, false)
            .unwrap();
        assert_eq!(truncated.to_decimal_string(), "1065.071161179585");
        assert_eq!(
            super::book_output_net_ratio(&gross, 1_002_000_000).to_decimal_string(),
            "1065.071161179586"
        );
        let max_gross = IOUAmount::from_parts(9_999_999_999_999_999, 0, false).unwrap();
        for rate in [1_000_000_001, u32::MAX] {
            let net = super::book_output_net_ratio(&max_gross, rate);
            assert!(net > IOUAmount::ZERO && net <= max_gross);
        }
    }

    #[test]
    fn amm_output_charges_transfer_fee_from_gross_delivery() {
        let mut ledger = Ledger::genesis();
        for (address, transfer_rate) in [
            (ISSUER, Some(1_002_000_000u64)),
            (POOL, None),
            (RECIPIENT, None),
        ] {
            let id = decode_account_id(address).unwrap();
            let mut account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": "100000000", "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            if let Some(rate) = transfer_rate {
                account["TransferRate"] = Value::from(rate);
            }
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_line(&mut ledger, POOL, "10");
        put_line(&mut ledger, RECIPIENT, "0");

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut taker_account = serde_json::json!({"Balance": "100000000"});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let gross = Leg::parse(&serde_json::json!({
            "currency": "USD", "issuer": ISSUER, "value": "1.070824686776"
        }))
        .unwrap();
        let net = pay_out_gross(
            &mut ctx,
            &decode_account_id(RECIPIENT).unwrap(),
            &mut taker_account,
            &decode_account_id(POOL).unwrap(),
            &decode_account_id(RECIPIENT).unwrap(),
            &gross,
            false,
            BalanceRounding::Legacy,
            BalanceRounding::Legacy,
            false,
            TransferFeeRounding::Nearest,
            false,
        )
        .unwrap();

        assert_eq!(net.iou.to_decimal_string(), "1.068687312151697");
        assert_eq!(
            holding(&sandbox, POOL).to_decimal_string(),
            "8.929175313224"
        );
        assert_eq!(
            holding(&sandbox, RECIPIENT).to_decimal_string(),
            "1.068687312151697"
        );
    }

    #[test]
    fn flow_redeems_iou_to_issuer_without_fee_or_self_line() {
        let issuer = decode_account_id(ISSUER).unwrap();
        let taker = decode_account_id(RECIPIENT).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let stored = if taker.as_bytes() < issuer.as_bytes() {
            "100"
        } else {
            "-100"
        };
        let (low, high) = if taker.as_bytes() < issuer.as_bytes() {
            (RECIPIENT, ISSUER)
        } else {
            (ISSUER, RECIPIENT)
        };
        let mut ledger = Ledger::genesis();
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": ISSUER, "value": stored},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "1000"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&taker, &issuer, &currency),
                serde_json::to_vec(&line).unwrap(),
            )
            .unwrap();
        let issuer_root = serde_json::json!({
            "LedgerEntryType": "AccountRoot", "Account": ISSUER,
            "Balance": "100000000", "Sequence": 1, "OwnerCount": 0,
            "Flags": 0, "TransferRate": 1100000000u64,
        });
        ledger
            .put_state(
                keylet::account(&issuer),
                serde_json::to_vec(&issuer_root).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let amount = Leg::parse(&serde_json::json!({
            "currency": "USD", "issuer": ISSUER, "value": "25",
        }))
        .unwrap();
        let mut taker_acct = serde_json::json!({});
        pay_in(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &issuer,
            &amount,
            true,
            PayInOptions {
                skip_taker_debit: false,
                allow_dust_drain: true,
                gross_override: None,
            },
        )
        .unwrap();

        assert_eq!(
            crate::amm_helpers::iou_holding_number(ctx.view, &taker, &issuer, &currency,)
                .to_iou()
                .to_decimal_string(),
            "75"
        );
    }
}

#[cfg(test)]
mod owner_funds_tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const MAKER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const ISSUER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    // Build a ledger where MAKER holds 100 USD from ISSUER with the given
    // trust-line and issuer-account flags, then return the maker's offer funds
    // for an offer that SELLS that USD (`owner_funds_leg` on the USD leg).
    fn maker_usd_funds(line_flags: u64, issuer_flags: u64) -> Leg {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        let mut cur = [0u8; 20];
        cur[12..15].copy_from_slice(b"USD");
        let issuer_is_low = issuer.as_bytes() < maker.as_bytes();
        // Balance is the low account's view; the maker (holder) is positive iff
        // value < 0 when the issuer is low, else value > 0.
        let value = if issuer_is_low { "-100" } else { "100" };
        let (low, high) = if issuer_is_low {
            (ISSUER, MAKER)
        } else {
            (MAKER, ISSUER)
        };
        let mut ledger = Ledger::genesis();
        for (addr, id, flags) in [(MAKER, &maker, 0u64), (ISSUER, &issuer, issuer_flags)] {
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": "100000000", "Sequence": 1, "OwnerCount": 0, "Flags": flags,
            });
            ledger
                .put_state(keylet::account(id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": value},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000000"},
            "Flags": line_flags,
        });
        ledger
            .put_state(
                keylet::trust_line(&maker, &issuer, &cur),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let gets = Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::ZERO,
            currency: cur,
            issuer,
        };
        owner_funds_leg(&mut ctx, &maker, &gets)
    }

    #[test]
    fn deleted_offer_does_not_release_xrp_reserve_during_crossing() {
        let maker = decode_account_id(MAKER).unwrap();
        let gets = Leg::parse(&Value::String("6045815".into())).unwrap();
        let fees = FeeSettings {
            base_fee: 10,
            reserve_base: 1_000_000,
            reserve_increment: 200_000,
        };
        let account = serde_json::to_vec(&serde_json::json!({
            "Balance": "4600000", "OwnerCount": 17,
        }))
        .unwrap();

        let live = owner_funds_leg_with(&fees, &maker, &gets, 0, |_| Some(account.clone()));
        assert_eq!(live.drops, 200_000);

        let with_parent_count =
            owner_funds_leg_with(&fees, &maker, &gets, 18, |_| Some(account.clone()));
        assert_eq!(with_parent_count.drops, 0);
    }

    fn freeze_flag() -> u64 {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        if issuer.as_bytes() < maker.as_bytes() {
            0x0040_0000 // lsfLowFreeze
        } else {
            0x0080_0000 // lsfHighFreeze
        }
    }

    fn auth_flag() -> u64 {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        if issuer.as_bytes() < maker.as_bytes() {
            0x0004_0000 // lsfLowAuth
        } else {
            0x0008_0000 // lsfHighAuth
        }
    }

    #[test]
    fn clean_line_is_funded() {
        assert!(!maker_usd_funds(0, 0).is_zero());
    }

    #[test]
    fn issuer_frozen_line_is_zero() {
        assert!(maker_usd_funds(freeze_flag(), 0).is_zero());
    }

    #[test]
    fn issuer_global_freeze_is_zero() {
        assert!(maker_usd_funds(0, 0x0040_0000).is_zero());
    }

    #[test]
    fn require_auth_unauthorized_is_zero() {
        assert!(maker_usd_funds(0, 0x0004_0000).is_zero());
    }

    #[test]
    fn require_auth_authorized_is_funded() {
        assert!(!maker_usd_funds(auth_flag(), 0x0004_0000).is_zero());
    }

    // Does MAKER's USD line report deep-frozen for the given line flags?
    fn maker_usd_deep_frozen(line_flags: u64) -> bool {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        let mut cur = [0u8; 20];
        cur[12..15].copy_from_slice(b"USD");
        let issuer_is_low = issuer.as_bytes() < maker.as_bytes();
        let (low, high) = if issuer_is_low {
            (ISSUER, MAKER)
        } else {
            (MAKER, ISSUER)
        };
        let mut ledger = Ledger::genesis();
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": "0"},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000000"},
            "Flags": line_flags,
        });
        ledger
            .put_state(
                keylet::trust_line(&maker, &issuer, &cur),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let asset = Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::ZERO,
            currency: cur,
            issuer,
        };
        is_deep_frozen(&mut ctx, &maker, &asset)
    }

    #[test]
    fn deep_freeze_detected_either_side() {
        assert!(maker_usd_deep_frozen(0x0200_0000)); // lsfLowDeepFreeze
        assert!(maker_usd_deep_frozen(0x0400_0000)); // lsfHighDeepFreeze
    }

    #[test]
    fn clean_or_regular_freeze_is_not_deep_frozen() {
        assert!(!maker_usd_deep_frozen(0));
        assert!(!maker_usd_deep_frozen(0x0040_0000)); // regular freeze, not deep
    }
}

// End-to-end synthetic exercise of the legacy Taker XRP AUTOBRIDGE. With an empty
// `Rules` (FlowCross disabled) an IOU/IOU OfferCreate routes through `taker_cross`
// -> `bridged_cross`. There is no direct AAA/BBB book, so the taker can only fill
// by bridging AAA -> XRP (leg1) then XRP -> BBB (leg2). This deterministically
// covers `do_cross_bridged` (leg equalisation), `fill_bridged` (the discrete
// owner1 -> owner2 XRP transfer plus the two IOU settlements) and `composed_quality`
// / `select_path` — the bridge-specific code that the direct-cross oracles do not
// reach. Real pre-FlowCross autobridge transactions are vanishingly rare on chain,
// so this synthetic ledger is the practical validation of the bridge legs.
#[cfg(test)]
mod taker_bridge_integration {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::apply_view::ApplyView;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const TAKER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const O1: &str = "rf1BiGeXwwQoi8Z2ueFYTEXSwuJYfV2Jpn";
    const O2: &str = "rpP2JgiMyTF5jR5hLG3xHCPi1knBb1v9cM";
    const O3: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";
    const ISS: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn ccy(code: &[u8; 3]) -> [u8; 20] {
        let mut c = [0u8; 20];
        c[12..15].copy_from_slice(code);
        c
    }

    fn put_acct(ledger: &mut Ledger, addr: &str, drops: u64, owner_count: u64) {
        let id = decode_account_id(addr).unwrap();
        let acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot", "Account": addr,
            "Balance": drops.to_string(), "Sequence": 1,
            "OwnerCount": owner_count, "Flags": 0,
        });
        ledger
            .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
            .unwrap();
    }

    // Seed a RippleState where `holder` holds `bal` of `code` issued by ISS.
    fn put_line(ledger: &mut Ledger, holder: &str, code: &[u8; 3], bal: i64) {
        let h = decode_account_id(holder).unwrap();
        let i = decode_account_id(ISS).unwrap();
        let cur = ccy(code);
        let issuer_is_low = i.as_bytes() < h.as_bytes();
        // Balance is stored from the low account's view: a positive holder shows a
        // negative low-view value when the issuer is the low account.
        let value = if bal == 0 {
            "0".to_string()
        } else if issuer_is_low {
            format!("-{bal}")
        } else {
            bal.to_string()
        };
        let cur_str = currency_code_str(&cur);
        let (low, high) = if issuer_is_low {
            (ISS, holder)
        } else {
            (holder, ISS)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": cur_str, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": value},
            "LowLimit": {"currency": cur_str, "issuer": low, "value": "0"},
            "HighLimit": {"currency": cur_str, "issuer": high, "value": "1000000000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&h, &i, &cur),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();
    }

    // The holder's balance magnitude in `code` (holders are positive here).
    fn line_bal(sandbox: &Sandbox<'_>, holder: &str, code: &[u8; 3]) -> String {
        let h = decode_account_id(holder).unwrap();
        let i = decode_account_id(ISS).unwrap();
        let b = sandbox
            .read(&keylet::trust_line(&h, &i, &ccy(code)))
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        v["Balance"]["value"]
            .as_str()
            .unwrap()
            .trim_start_matches('-')
            .to_string()
    }

    fn xrp_bal(sandbox: &Sandbox<'_>, addr: &str) -> u64 {
        let id = decode_account_id(addr).unwrap();
        let b = sandbox.read(&keylet::account(&id)).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        v["Balance"].as_str().unwrap().parse().unwrap()
    }

    fn apply_offer(
        sandbox: &mut Sandbox<'_>,
        fees: &FeeSettings,
        tx: &serde_json::Value,
    ) -> TransactionResult {
        let rules = Rules::new(); // empty -> FlowCross disabled -> legacy taker_cross
        let mut ctx = ApplyContext {
            tx,
            view: sandbox,
            rules: &rules,
            fees,
        };
        OfferCreateTransactor.apply(&mut ctx).unwrap()
    }

    #[test]
    fn ledger_105502305_clob_upper_bound_ignores_evr_output_fee() {
        const ISSUER: &str = "ra9g3LAJm9xJu8Awe7oWzR6VXFB1mpFtSe";
        const SELLER: &str = "rGBYufoUiwpT2DYvbPXh68HTRswmyDzKVh";
        const MAKER: &str = "rsHHWwNemQXNxieW8nRoGGTP8kvzc6vrG4";
        let mut ledger = Ledger::genesis();
        let issuer = decode_account_id(ISSUER).unwrap();
        ledger
            .put_state(
                keylet::account(&issuer),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": ISSUER,
                    "Balance": "1000000000", "Sequence": 1, "OwnerCount": 0,
                    "Flags": 0, "TransferRate": 1_002_000_000u64,
                }))
                .unwrap(),
            )
            .unwrap();
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": SELLER, "Flags": TF_SELL,
            "TakerGets": "81000000",
            "TakerPays": {"currency": "EVR", "issuer": ISSUER, "value": "788.535"},
        });
        let input = Leg::parse(&tx["TakerGets"]).unwrap();
        let output = Leg::parse(&tx["TakerPays"]).unwrap();
        let book = keylet::book_dir(
            &input.currency,
            &input.issuer,
            &output.currency,
            &output.issuer,
        );
        let maker_quality = 0x5A03_A620_F369_8C3B;
        let directory = book_dir_with_quality(&book, maker_quality);
        let maker_key = keylet::offer(&decode_account_id(MAKER).unwrap(), 102_225_795);
        ledger
            .put_state(
                directory,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode", "Indexes": [maker_key.to_string()],
                }))
                .unwrap(),
            )
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_1"),
        ]);
        let ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let seller = decode_account_id(SELLER).unwrap();
        let limit = flow_crossing_threshold(&ctx, &seller, &input, &output, true, false);
        let raw = from_rate(maker_quality).unwrap();
        let fee = IOUAmount::from_decimal_string("1.002").unwrap();
        let with_output_fee = IOUAmount::mul_round(&raw, &fee, true).unwrap();
        assert_eq!(raw.to_decimal_string(), "102708.5383076923");
        assert_eq!(
            from_rate(limit).unwrap().to_decimal_string(),
            "102722.1366204417"
        );
        assert!(maker_quality < limit);
        assert!(with_output_fee > from_rate(limit).unwrap());
        assert!(offer_crossing_strand_reaches_limit(
            &ctx,
            &seller,
            &tx["TakerGets"],
            &tx["TakerPays"],
            limit,
        ));
    }

    fn assert_partial_sell_followed_by_self_offer(ioc: bool) {
        let mut ledger = Ledger::genesis();
        for account in [ISS, TAKER, O1] {
            put_acct(&mut ledger, account, 1_000_000_000, 0);
        }
        for (holder, code, balance) in [
            (TAKER, b"AAA", 1000),
            (TAKER, b"BBB", 100),
            (O1, b"AAA", 0),
            (O1, b"BBB", 100),
        ] {
            put_line(&mut ledger, holder, code, balance);
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let aaa =
            |value: &str| serde_json::json!({"currency": "AAA", "issuer": ISS, "value": value});
        let bbb =
            |value: &str| serde_json::json!({"currency": "BBB", "issuer": ISS, "value": value});
        for (owner, gets, pays) in [
            (O1, "49.5134019592343", "255.548570192"),
            (TAKER, "57.3606413970318", "296.0554784425"),
        ] {
            assert_eq!(
                apply_offer(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": owner,
                        "Fee": "10", "Sequence": 1, "Flags": TF_PASSIVE | TF_SELL,
                        "TakerGets": bbb(gets), "TakerPays": aaa(pays),
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let taker = decode_account_id(TAKER).unwrap();
        let maker = decode_account_id(O1).unwrap();
        let own_key = keylet::offer(&taker, 1);
        let own_before = sandbox.read(&own_key).unwrap();
        let own_offer: Value = serde_json::from_slice(&own_before).unwrap();
        let own_dir: Hash256 = own_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let maker_before: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&maker, 1)).unwrap()).unwrap();
        let maker_dir: Hash256 = maker_before["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let maker_quality = u64::from_be_bytes(maker_dir.as_bytes()[24..32].try_into().unwrap());
        let own_quality = u64::from_be_bytes(own_dir.as_bytes()[24..32].try_into().unwrap());
        assert!(
            maker_quality < own_quality,
            "self offer follows at a worse quality"
        );
        let dir_before = sandbox.read(&own_dir).unwrap();
        let owner_before: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker)).unwrap()).unwrap();
        assert_eq!(owner_before["OwnerCount"], 1);
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        let flags = if ioc {
            TF_SELL | TF_IMMEDIATE_OR_CANCEL
        } else {
            TF_SELL
        };
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "10", "Sequence": 2,
            "Flags": flags,
            "TakerGets": aaa("255.545538"),
            "TakerPays": bbb("49.423691"),
        });
        let threshold = crossing_quality_threshold(
            &Leg::parse(&tx["TakerGets"]).unwrap(),
            &Leg::parse(&tx["TakerPays"]).unwrap(),
            true,
            false,
        );
        assert!(
            own_quality < threshold,
            "self offer is within the taker's limit"
        );
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let maker_after: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&maker, 1)).unwrap()).unwrap();
        assert_eq!(maker_after["TakerPays"]["value"], "0.003032192");
        // Both synthetic assets use one issuer; its canonical Gets remainder
        // differs from TX8 by one ULP while the partial-fill boundary matches.
        assert_eq!(maker_after["TakerGets"]["value"], "0.00058749748121");
        assert_eq!(line_bal(&sandbox, TAKER, b"AAA"), "744.454462");
        let owner_after: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker)).unwrap()).unwrap();
        if ioc {
            assert_eq!(sandbox.read(&own_key).unwrap(), own_before);
            assert_eq!(sandbox.read(&own_dir).unwrap(), dir_before);
            assert_eq!(owner_after["OwnerCount"], owner_before["OwnerCount"]);
        } else {
            assert!(sandbox.read(&own_key).is_none());
            assert!(sandbox.read(&own_dir).is_none());
            assert_eq!(owner_after["OwnerCount"], 0);
        }
    }

    #[test]
    fn ledger_105502352_partial_sell_ioc_keeps_next_quality_self_offer() {
        assert_partial_sell_followed_by_self_offer(true);
    }

    #[test]
    fn ledger_105500733_partial_sell_non_ioc_reaps_next_quality_self_offer() {
        assert_partial_sell_followed_by_self_offer(false);
    }

    #[test]
    fn iou_to_iou_offer_autobridges_through_xrp() {
        let mut ledger = Ledger::genesis();
        // Accounts (generous XRP so the reserve never blocks placement).
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 2);
        put_acct(&mut ledger, O1, 1_000_000_000, 1);
        put_acct(&mut ledger, O2, 1_000_000_000, 1);
        // Trust lines: taker holds 100 AAA (to spend) and 0 BBB (to receive);
        // O1 holds 0 AAA (to receive); O2 holds 100 BBB (to give).
        put_line(&mut ledger, TAKER, b"AAA", 100);
        put_line(&mut ledger, TAKER, b"BBB", 0);
        put_line(&mut ledger, O1, b"AAA", 0);
        put_line(&mut ledger, O2, b"BBB", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        // leg1: O1 gives 100 XRP for 100 AAA  (book AAA->XRP).
        let r1 = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": O1, "Fee": "12", "Sequence": 1,
                "TakerGets": "100000000",
                "TakerPays": {"currency": "AAA", "issuer": ISS, "value": "100"},
            }),
        );
        assert_eq!(r1, TransactionResult::TesSuccess);
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(O1).unwrap(), 1))
                .is_some()
        );

        // leg2: O2 gives 100 BBB for 100 XRP  (book XRP->BBB).
        let r2 = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": O2, "Fee": "12", "Sequence": 1,
                "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
                "TakerPays": "100000000",
            }),
        );
        assert_eq!(r2, TransactionResult::TesSuccess);
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(O2).unwrap(), 1))
                .is_some()
        );

        // Taker: gives 100 AAA, wants 100 BBB. No direct AAA/BBB book -> autobridge.
        let rt = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "12", "Sequence": 1,
                "TakerGets": {"currency": "AAA", "issuer": ISS, "value": "100"},
                "TakerPays": {"currency": "BBB", "issuer": ISS, "value": "100"},
            }),
        );
        assert_eq!(rt, TransactionResult::TesSuccess);

        // The bridge crossed in full at 1:1:1.
        assert_eq!(
            line_bal(&sandbox, TAKER, b"AAA"),
            "0",
            "taker spent all AAA"
        );
        assert_eq!(
            line_bal(&sandbox, TAKER, b"BBB"),
            "100",
            "taker received BBB"
        );
        assert_eq!(
            line_bal(&sandbox, O1, b"AAA"),
            "100",
            "leg1 owner received AAA"
        );
        assert_eq!(line_bal(&sandbox, O2, b"BBB"), "0", "leg2 owner gave BBB");
        // XRP flowed leg1 owner -> leg2 owner (100 XRP), taker XRP untouched.
        assert_eq!(
            xrp_bal(&sandbox, O1),
            900_000_000,
            "leg1 owner paid 100 XRP"
        );
        assert_eq!(
            xrp_bal(&sandbox, O2),
            1_100_000_000,
            "leg2 owner got 100 XRP"
        );
        assert_eq!(
            xrp_bal(&sandbox, TAKER),
            1_000_000_000,
            "taker XRP untouched"
        );
        // Both legs fully consumed; the taker rests no residual.
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(O1).unwrap(), 1))
                .is_none(),
            "leg1 consumed"
        );
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(O2).unwrap(), 1))
                .is_none(),
            "leg2 consumed"
        );
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(TAKER).unwrap(), 1))
                .is_none(),
            "no taker residual"
        );
    }

    #[test]
    fn clob_only_bridge_hop_does_not_reinject_amm() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 1);
        put_acct(&mut ledger, O2, 1_000_000_000, 1);
        put_acct(&mut ledger, O3, 50_000_000, 1);
        put_line(&mut ledger, TAKER, b"BBB", 0);
        put_line(&mut ledger, O2, b"BBB", 100);
        put_line(&mut ledger, O3, b"BBB", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        // Resting CLOB price: 1 BBB costs 1 XRP.
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O2, "Fee": "12", "Sequence": 1,
                    "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TesSuccess
        );

        // The pool's 0.5 XRP/BBB spot is better than the CLOB. This helper
        // invocation is the CLOB portion of the bridge pass; AMM selection is
        // handled by the enclosing Flow strand.
        let zero = AccountId::from([0u8; 20]);
        let issuer = decode_account_id(ISS).unwrap();
        let amm_key = keylet::amm(&[0u8; 20], &zero, &ccy(b"BBB"), &issuer);
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Account": O3,
            "TradingFee": 0,
            "Flags": 0,
        });
        sandbox
            .insert(amm_key, serde_json::to_vec(&amm).unwrap())
            .unwrap();

        let taker = decode_account_id(TAKER).unwrap();
        let account_bytes = sandbox.read(&keylet::account(&taker)).unwrap();
        let mut taker_acct: Value = serde_json::from_slice(&account_bytes).unwrap();
        let demand = Leg::parse(&serde_json::json!({
            "currency": "BBB", "issuer": ISS, "value": "10"
        }))
        .unwrap();
        let budget = Leg::parse(&serde_json::json!("10000000")).unwrap();
        let rules = Rules::from_enabled([feature_id("FlowCross")]);
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let (delivered, spent) = cross_book_hop(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &demand,
            &budget,
            false,
            false,
            true,
            false,
            None,
            false,
            false,
            1_000_000_000,
            false,
            false,
        )
        .unwrap();

        assert_eq!(delivered.iou.to_decimal_string(), "10");
        assert_eq!(spent.drops, 10_000_000);
        assert_eq!(xrp_bal(&sandbox, O3), 50_000_000);
        assert_eq!(line_bal(&sandbox, O3, b"BBB"), "100");
    }

    #[test]
    fn reverse_clob_pricing_ignores_multihop_source_balance() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 5_000_000, 1);
        put_acct(&mut ledger, O2, 1_000_000_000, 1);
        put_line(&mut ledger, TAKER, b"BBB", 0);
        put_line(&mut ledger, O2, b"BBB", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O2, "Fee": "12", "Sequence": 1,
                    "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TesSuccess
        );

        let taker = decode_account_id(TAKER).unwrap();
        let offer_key = keylet::offer(&decode_account_id(O2).unwrap(), 1);
        let offer_before = sandbox.read(&offer_key).unwrap();
        let account_bytes = sandbox.read(&keylet::account(&taker)).unwrap();
        let mut taker_acct: Value = serde_json::from_slice(&account_bytes).unwrap();
        let taker_before = taker_acct.clone();
        let rules = Rules::from_enabled([feature_id("FlowCross")]);
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let strand = build_flow_strand(
            &ctx,
            &[
                serde_json::json!("10000000"),
                serde_json::json!({
                    "currency": "BBB", "issuer": ISS, "value": "10"
                }),
            ],
        )
        .unwrap();

        let priced = price_clob_band_reverse(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &strand.hops[0],
            &Number::from_int(10),
            None,
            true,
            false,
            &mut Vec::new(),
            false,
            false,
            false,
        )
        .unwrap();

        assert_eq!(priced.0.to_xrp_drops(), 10_000_000);
        assert_eq!(priced.1.to_iou().to_decimal_string(), "10");
        assert_eq!(taker_acct, taker_before);
        assert_eq!(ctx.view.read(&offer_key).unwrap(), offer_before);
    }

    #[test]
    fn first_xrp_book_caps_drifted_offer_at_source_budget() {
        let mut ledger = Ledger::genesis();
        for account in [ISS, TAKER, O2, O3] {
            put_acct(&mut ledger, account, 1_000_000_000, 0);
        }
        put_line(&mut ledger, O2, b"BBB", 2_000);
        put_line(&mut ledger, O3, b"BBB", 0);
        put_line(&mut ledger, O3, b"CCC", 360);
        put_line(&mut ledger, TAKER, b"CCC", 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        for (owner, gets, pays) in [
            (
                O2,
                serde_json::json!({"currency": "BBB", "issuer": ISS, "value": "1134.832047866812"}),
                serde_json::json!("195145211"),
            ),
            (
                O3,
                serde_json::json!({"currency": "CCC", "issuer": ISS, "value": "359.99705"}),
                serde_json::json!({"currency": "BBB", "issuer": ISS, "value": "277.99"}),
            ),
        ] {
            assert_eq!(
                apply_offer(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": owner,
                        "Fee": "12", "Sequence": 1,
                        "TakerGets": gets, "TakerPays": pays,
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }

        // The offer's live fields can drift from its fixed directory quality.
        // Its remaining Pays/ Gets ratio is slightly better than the fixed
        // directory rate. Buying 277.99 BBB at that rate costs 47803036 drops,
        // one above SendMax.
        let first_key = keylet::offer(&decode_account_id(O2).unwrap(), 1);
        let mut first: Value = serde_json::from_slice(&sandbox.read(&first_key).unwrap()).unwrap();
        first["TakerPays"] = Value::String("195145206".into());
        first["TakerGets"]["value"] = Value::String("1134.832046187368".into());
        sandbox
            .update(first_key, serde_json::to_vec(&first).unwrap())
            .unwrap();

        let taker = decode_account_id(TAKER).unwrap();
        let mut taker_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker)).unwrap()).unwrap();
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        let tx = serde_json::json!({"TransactionType": "Payment"});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let strand = build_flow_strand(
            &ctx,
            &[
                serde_json::json!("47803035"),
                serde_json::json!({"currency": "BBB", "issuer": ISS, "value": "277.99"}),
                serde_json::json!({"currency": "CCC", "issuer": ISS, "value": "359.99705"}),
            ],
        )
        .unwrap();
        assert_eq!(
            book_tip_quality(&ctx, &strand.hops[0].inverse_book),
            Some(0x5A06_1BF6_885D_7CDF)
        );
        let requested = leg_to_number(
            &Leg::parse(&serde_json::json!({
                "currency": "CCC", "issuer": ISS, "value": "359.99705"
            }))
            .unwrap(),
        );
        let pass = execute_strand_pass(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &strand,
            &Number::from_int(47_803_035),
            &requested,
            &mut AmmContext::new(false),
            false,
            false,
            None,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(pass.in_amt.to_xrp_drops(), 47_803_035);
        assert_eq!(
            pass.out_amt.to_iou().to_decimal_string(),
            "359.9970426831127"
        );
        let second: Value = serde_json::from_slice(
            &ctx.view
                .read(&keylet::offer(&decode_account_id(O3).unwrap(), 1))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(second["TakerPays"]["value"], "0.000005650106");
        assert_eq!(second["TakerGets"]["value"], "0.0000073168873");
    }

    #[test]
    fn ledger_105502321_bridge_xrp_quote_crosses_one_drop_tip_and_next_quality() {
        let mut ledger = Ledger::genesis();
        for account in [ISS, TAKER, O1, O2] {
            put_acct(&mut ledger, account, 1_000_000_000, 0);
        }
        for account in [TAKER, O1, O2] {
            put_line(
                &mut ledger,
                account,
                b"AAA",
                if account == TAKER { 1000 } else { 0 },
            );
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        for (owner, gets, pays) in [
            (O1, "1", "0.0000613515"),
            (O2, "265593709", "16371.85378035912"),
        ] {
            assert_eq!(
                apply_offer(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": owner,
                        "Fee": "12", "Sequence": 1,
                        "TakerGets": gets,
                        "TakerPays": {"currency": "AAA", "issuer": ISS, "value": pays},
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        // TX7's leading page retained its directory quality after earlier
        // partial fills, but its live offer fields had only one drop left.
        let tiny_key = keylet::offer(&decode_account_id(O1).unwrap(), 1);
        let mut tiny: Value = serde_json::from_slice(&sandbox.read(&tiny_key).unwrap()).unwrap();
        tiny["TakerPays"]["value"] = Value::String("0.00000000000012".into());
        sandbox
            .update(tiny_key, serde_json::to_vec(&tiny).unwrap())
            .unwrap();

        let taker = decode_account_id(TAKER).unwrap();
        let maker = decode_account_id(O2).unwrap();
        let maker_before = xrp_bal(&sandbox, O2);
        let mut taker_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker)).unwrap()).unwrap();
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let strand = build_flow_strand(
            &ctx,
            &[
                serde_json::json!({"currency": "AAA", "issuer": ISS, "value": "1000"}),
                serde_json::json!("0"),
            ],
        )
        .unwrap();
        let demanded = Number::from_int(2_702_825);
        let ordinary_quote = price_clob_band_reverse(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &strand.hops[0],
            &demanded,
            None,
            true,
            false,
            &mut Vec::new(),
            false,
            true,
            false,
        )
        .unwrap();
        assert_eq!(ordinary_quote.1.to_xrp_drops(), 1);
        assert!(bridge_xrp_one_drop_tip_needs_more_pages(
            true,
            &demanded,
            &ordinary_quote.1,
        ));
        assert!(!bridge_xrp_one_drop_tip_needs_more_pages(
            false,
            &demanded,
            &ordinary_quote.1,
        ));
        let quoted = price_clob_band_reverse(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &strand.hops[0],
            &demanded,
            None,
            true,
            false,
            &mut Vec::new(),
            false,
            true,
            true,
        )
        .unwrap();
        assert_eq!(quoted.1.to_xrp_drops(), 2_702_825);
        assert!(
            ctx.view.read(&tiny_key).is_some(),
            "reverse quote rolls back"
        );

        let (delivered, spent) = cross_book_hop(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &number_to_leg(&quoted.1, &strand.hops[0].out_leg),
            &number_to_leg(&quoted.0, &strand.hops[0].in_leg),
            false,
            true,
            false,
            false,
            None,
            false,
            true,
            1_000_000_000,
            false,
            false,
        )
        .unwrap();
        assert_eq!(delivered.drops, 2_702_825);
        assert_eq!(spent.iou, quoted.0.to_iou());
        assert!(ctx.view.read(&tiny_key).is_none());
        let next: Value =
            serde_json::from_slice(&ctx.view.read(&keylet::offer(&maker, 1)).unwrap()).unwrap();
        assert_eq!(next["TakerGets"], "262890885");
        let maker_after: Value =
            serde_json::from_slice(&ctx.view.read(&keylet::account(&maker)).unwrap()).unwrap();
        assert_eq!(
            maker_after["Balance"],
            (maker_before - 2_702_824).to_string()
        );
    }

    #[test]
    fn reverse_quote_preserves_permanent_reaps_after_forward_input_stop() {
        let mut ledger = Ledger::genesis();
        for account in [ISS, TAKER, O2, O3] {
            put_acct(&mut ledger, account, 1_000_000_000, 0);
        }
        for account in [TAKER, O2, O3] {
            put_line(
                &mut ledger,
                account,
                b"BBB",
                if account == TAKER { 0 } else { 100 },
            );
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut placement = Sandbox::new(&view);
        for (account, sequence) in [(O2, 1), (O2, 2)]
            .into_iter()
            .chain((1..=4).map(|seq| (O3, seq)))
        {
            assert_eq!(
                apply_offer(
                    &mut placement,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": account,
                        "Fee": "12", "Sequence": sequence,
                        "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
                        "TakerPays": "100000000",
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let changes = placement.into_changes();
        for (key, bytes) in changes.inserts.into_iter().chain(changes.updates) {
            ledger.put_state(key, bytes).unwrap();
        }
        for key in changes.deletes.keys() {
            ledger.delete_state(key).unwrap();
        }
        // The four offers existed before this Payment but now have no funds.
        put_line(&mut ledger, O3, b"BBB", 0);

        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let taker = decode_account_id(TAKER).unwrap();
        let funded_key = keylet::offer(&decode_account_id(O2).unwrap(), 1);
        let became_unfunded_key = keylet::offer(&decode_account_id(O2).unwrap(), 2);
        let invalid_owner = decode_account_id(O3).unwrap();
        let invalid_keys: Vec<_> = (1..=4)
            .map(|sequence| keylet::offer(&invalid_owner, sequence))
            .collect();
        let account_bytes = sandbox.read(&keylet::account(&taker)).unwrap();
        let mut taker_acct: Value = serde_json::from_slice(&account_bytes).unwrap();
        let rules = Rules::from_enabled([feature_id("FlowCross")]);
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let strand = build_flow_strand(
            &ctx,
            &[
                serde_json::json!("10000000"),
                serde_json::json!({"currency": "BBB", "issuer": ISS, "value": "1000"}),
            ],
        )
        .unwrap();
        let mut reaps = Vec::new();
        let quoted = price_clob_band_reverse(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &strand.hops[0],
            &Number::from_int(1000),
            Some(&Number::from_int(10_000_000)),
            true,
            true,
            &mut reaps,
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(quoted.0.to_xrp_drops(), 10_000_000);
        assert_eq!(quoted.1.to_iou().to_decimal_string(), "10");
        assert_eq!(reaps.len(), 4);
        assert!(reaps.iter().all(|(_, key, _)| *key != became_unfunded_key));
        for key in &invalid_keys {
            assert!(
                ctx.view.read(key).is_some(),
                "reverse quote was rolled back"
            );
        }

        let (delivered, spent) = cross_book_hop(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &number_to_leg(&quoted.1, &strand.hops[0].out_leg),
            &number_to_leg(&quoted.0, &strand.hops[0].in_leg),
            false,
            false,
            true,
            false,
            None,
            false,
            false,
            1_000_000_000,
            false,
            false,
        )
        .unwrap();
        assert_eq!(spent.drops, 10_000_000);
        assert_eq!(delivered.iou.to_decimal_string(), "10");
        for key in &invalid_keys {
            assert!(
                ctx.view.read(key).is_some(),
                "forward stopped at the partial tip"
            );
        }
        apply_permanent_reaps(&mut ctx, &reaps);
        for key in &invalid_keys {
            assert!(ctx.view.read(key).is_none());
        }
        let funded: Value = serde_json::from_slice(&ctx.view.read(&funded_key).unwrap()).unwrap();
        assert_eq!(funded["TakerGets"]["value"], "90");
        assert!(ctx.view.read(&became_unfunded_key).is_some());
        let owner: Value =
            serde_json::from_slice(&ctx.view.read(&keylet::account(&invalid_owner)).unwrap())
                .unwrap();
        assert_eq!(owner["OwnerCount"], 0);
    }

    #[test]
    fn funded_tip_probe_reaps_unfunded_offer_and_follows_directory_pages() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, TAKER, 1_000_000_000, 0);
        put_acct(&mut ledger, O2, 1_000_000_000, 1);
        put_acct(&mut ledger, O3, 1_000_000_000, 1);
        put_line(&mut ledger, O3, b"BBB", 100);

        let taker = decode_account_id(TAKER).unwrap();
        let owner = decode_account_id(O2).unwrap();
        let funded_owner = decode_account_id(O3).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let zero = AccountId::from([0u8; 20]);
        let currency = ccy(b"BBB");
        let book = keylet::book_dir(&[0u8; 20], &zero, &currency, &issuer);
        let quality = get_rate(
            &IOUAmount::from_decimal_string("1000000").unwrap(),
            &IOUAmount::from_decimal_string("100").unwrap(),
        )
        .unwrap();
        let directory = book_dir_with_quality(&book, quality);
        let offer_key = keylet::offer(&owner, 1);
        let funded_offer_key = keylet::offer(&funded_owner, 1);
        let offer = serde_json::json!({
            "LedgerEntryType": "Offer",
            "Account": O2,
            "Sequence": 1,
            "TakerPays": "1000000",
            "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
            "BookDirectory": directory.to_string(),
            "OwnerNode": "0000000000000000",
            "BookNode": "0000000000000000",
            "Flags": 0,
        });
        ledger
            .put_state(offer_key, serde_json::to_vec(&offer).unwrap())
            .unwrap();
        let funded_offer = serde_json::json!({
            "LedgerEntryType": "Offer",
            "Account": O3,
            "Sequence": 1,
            "TakerPays": "1000000",
            "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
            "BookDirectory": directory.to_string(),
            "OwnerNode": "0000000000000000",
            "BookNode": "0000000000000001",
            "Flags": 0,
        });
        ledger
            .put_state(funded_offer_key, serde_json::to_vec(&funded_offer).unwrap())
            .unwrap();
        let dir = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "RootIndex": directory.to_string(),
            "Indexes": [offer_key.to_string()],
            "IndexNext": 1,
            "IndexPrevious": 0,
            "Flags": 0,
        });
        ledger
            .put_state(directory, serde_json::to_vec(&dir).unwrap())
            .unwrap();
        let page_key = keylet::dir_node(&directory, 1);
        let page = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "RootIndex": directory.to_string(),
            "Indexes": [funded_offer_key.to_string()],
            "IndexNext": 0,
            "IndexPrevious": 0,
            "Flags": 0,
        });
        ledger
            .put_state(page_key, serde_json::to_vec(&page).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        assert_eq!(
            funded_book_tip_quality(&mut ctx, &taker, &book),
            Some(quality)
        );
        assert!(ctx.view.read(&offer_key).is_none());
        assert!(ctx.view.read(&directory).is_some());
        assert!(ctx.view.read(&funded_offer_key).is_some());

        let account_bytes = ctx.view.read(&keylet::account(&taker)).unwrap();
        let mut taker_acct: Value = serde_json::from_slice(&account_bytes).unwrap();
        let demand = Leg::parse(&serde_json::json!({
            "currency": "BBB", "issuer": ISS, "value": "10"
        }))
        .unwrap();
        let budget = Leg::parse(&serde_json::json!("1000000")).unwrap();
        let (delivered, spent) = cross_book_hop(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker,
            &demand,
            &budget,
            true,
            true,
            true,
            false,
            None,
            false,
            false,
            1_000_000_000,
            false,
            false,
        )
        .unwrap();
        assert_eq!(delivered.iou.to_decimal_string(), "10");
        assert_eq!(spent.drops, 100_000);
    }

    #[test]
    fn bridged_cross_accumulates_leg1_leftover_across_leg2_fills() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 2);
        put_acct(&mut ledger, O1, 1_000_000_000, 1);
        put_acct(&mut ledger, O2, 1_000_000_000, 1);
        put_acct(&mut ledger, O3, 1_000_000_000, 1);
        put_line(&mut ledger, TAKER, b"AAA", 200);
        put_line(&mut ledger, TAKER, b"BBB", 0);
        put_line(&mut ledger, O1, b"AAA", 0);
        put_line(&mut ledger, O2, b"BBB", 100);
        put_line(&mut ledger, O3, b"BBB", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O1, "Fee": "12", "Sequence": 1,
                    "TakerGets": "300000000",
                    "TakerPays": {"currency": "AAA", "issuer": ISS, "value": "300"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O2, "Fee": "12", "Sequence": 1,
                    "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O3, "Fee": "12", "Sequence": 1,
                    "TakerGets": {"currency": "BBB", "issuer": ISS, "value": "100"},
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "12", "Sequence": 1,
                    "TakerGets": {"currency": "AAA", "issuer": ISS, "value": "200"},
                    "TakerPays": {"currency": "BBB", "issuer": ISS, "value": "200"},
                }),
            ),
            TransactionResult::TesSuccess
        );

        assert_eq!(xrp_bal(&sandbox, O1), 800_000_000, "leg1 paid 200 XRP");
        let leftover = sandbox
            .read(&keylet::offer(&decode_account_id(O1).unwrap(), 1))
            .expect("leg1 still resting");
        let leftover: serde_json::Value = serde_json::from_slice(&leftover).unwrap();
        assert_eq!(
            leftover["TakerGets"].as_str(),
            Some("100000000"),
            "leg1 leftover is original minus every fill, not minus the last fill"
        );
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(O2).unwrap(), 1))
                .is_none(),
            "leg2a consumed"
        );
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(O3).unwrap(), 1))
                .is_none(),
            "leg2b consumed"
        );
    }

    fn has_line(sandbox: &Sandbox<'_>, holder: &str, code: &[u8; 3]) -> bool {
        let h = decode_account_id(holder).unwrap();
        let i = decode_account_id(ISS).unwrap();
        sandbox
            .read(&keylet::trust_line(&h, &i, &ccy(code)))
            .is_some()
    }

    fn owner_count(sandbox: &Sandbox<'_>, addr: &str) -> u64 {
        let id = decode_account_id(addr).unwrap();
        let b = sandbox.read(&keylet::account(&id)).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        v["OwnerCount"].as_u64().unwrap()
    }

    // Seed a default (limit-0) RippleState so drain-to-zero is deletable.
    fn put_default_line(ledger: &mut Ledger, holder: &str, code: &[u8; 3], bal: i64) {
        let h = decode_account_id(holder).unwrap();
        let i = decode_account_id(ISS).unwrap();
        let cur = ccy(code);
        let issuer_is_low = i.as_bytes() < h.as_bytes();
        let value = if bal == 0 {
            "0".to_string()
        } else if issuer_is_low {
            format!("-{bal}")
        } else {
            bal.to_string()
        };
        let cur_str = currency_code_str(&cur);
        let (low, high) = if issuer_is_low {
            (ISS, holder)
        } else {
            (holder, ISS)
        };
        // Reserve on the holder side only; no NoRipple so DefaultRipple-off
        // still treats the drained line as deletable (View.cpp:994).
        const LSF_LOW_RESERVE: u64 = 0x0001_0000;
        const LSF_HIGH_RESERVE: u64 = 0x0002_0000;
        let flags = if issuer_is_low {
            LSF_HIGH_RESERVE
        } else {
            LSF_LOW_RESERVE
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": cur_str, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": value},
            "LowLimit": {"currency": cur_str, "issuer": low, "value": "0"},
            "HighLimit": {"currency": cur_str, "issuer": high, "value": "0"},
            "Flags": flags,
        });
        ledger
            .put_state(
                keylet::trust_line(&h, &i, &cur),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn taker_without_output_line_creates_trust_line() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 0);
        put_acct(&mut ledger, O1, 1_000_000_000, 0);
        put_line(&mut ledger, O1, b"AAA", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        let r1 = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": O1, "Fee": "12", "Sequence": 1,
                "TakerGets": {"currency": "AAA", "issuer": ISS, "value": "100"},
                "TakerPays": "100000000",
            }),
        );
        assert_eq!(r1, TransactionResult::TesSuccess);
        assert!(!has_line(&sandbox, TAKER, b"AAA"));

        let rt = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "12", "Sequence": 1,
                "TakerGets": "100000000",
                "TakerPays": {"currency": "AAA", "issuer": ISS, "value": "100"},
            }),
        );
        assert_eq!(rt, TransactionResult::TesSuccess);
        assert!(
            has_line(&sandbox, TAKER, b"AAA"),
            "rippleCredit createOnDemand must write the taker's AAA line"
        );
        assert_eq!(line_bal(&sandbox, TAKER, b"AAA"), "100");
        assert_eq!(line_bal(&sandbox, O1, b"AAA"), "0");
        assert!(
            owner_count(&sandbox, TAKER) >= 1,
            "taker bears the new line's reserve"
        );
    }

    #[test]
    fn maker_without_input_line_creates_trust_line() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 0);
        put_acct(&mut ledger, O1, 1_000_000_000, 0);
        put_line(&mut ledger, TAKER, b"AAA", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        let r1 = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": O1, "Fee": "12", "Sequence": 1,
                "TakerGets": "100000000",
                "TakerPays": {"currency": "AAA", "issuer": ISS, "value": "100"},
            }),
        );
        assert_eq!(r1, TransactionResult::TesSuccess);
        assert!(!has_line(&sandbox, O1, b"AAA"));

        let rt = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "12", "Sequence": 1,
                "TakerGets": {"currency": "AAA", "issuer": ISS, "value": "100"},
                "TakerPays": "100000000",
            }),
        );
        assert_eq!(rt, TransactionResult::TesSuccess);
        assert!(
            has_line(&sandbox, O1, b"AAA"),
            "rippleCredit createOnDemand must write the maker's AAA line"
        );
        assert_eq!(line_bal(&sandbox, O1, b"AAA"), "100");
        assert_eq!(line_bal(&sandbox, TAKER, b"AAA"), "0");
        assert!(
            owner_count(&sandbox, O1) >= 1,
            "maker bears the new line's reserve"
        );
    }

    #[test]
    fn drained_default_trust_line_is_deleted() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 1);
        put_acct(&mut ledger, O1, 1_000_000_000, 0);
        put_default_line(&mut ledger, TAKER, b"AAA", 100);
        put_line(&mut ledger, O1, b"AAA", 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        let r1 = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": O1, "Fee": "12", "Sequence": 1,
                "TakerGets": "100000000",
                "TakerPays": {"currency": "AAA", "issuer": ISS, "value": "100"},
            }),
        );
        assert_eq!(r1, TransactionResult::TesSuccess);

        let rt = apply_offer(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "12", "Sequence": 1,
                "TakerGets": {"currency": "AAA", "issuer": ISS, "value": "100"},
                "TakerPays": "100000000",
            }),
        );
        assert_eq!(rt, TransactionResult::TesSuccess);
        assert!(
            !has_line(&sandbox, TAKER, b"AAA"),
            "drained default line must be trustDeleted"
        );
        assert_eq!(
            owner_count(&sandbox, TAKER),
            0,
            "sender OwnerCount drops with the deleted line"
        );
    }

    #[test]
    fn payment_book_output_deletes_drained_maker_line() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 1);
        put_acct(&mut ledger, O1, 1_000_000_000, 1);
        put_line(&mut ledger, TAKER, b"AAA", 0);
        put_default_line(&mut ledger, O1, b"AAA", 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let taker = decode_account_id(TAKER).unwrap();
        let owner = decode_account_id(O1).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let taker_key = keylet::account(&taker);
        let mut taker_acct: Value =
            serde_json::from_slice(&sandbox.read(&taker_key).unwrap()).unwrap();
        let amount = Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::from_decimal_string("100").unwrap(),
            currency: ccy(b"AAA"),
            issuer,
        };
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let delivered = pay_out_gross(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &owner,
            &taker,
            &amount,
            false,
            BalanceRounding::Legacy,
            BalanceRounding::Legacy,
            false,
            TransferFeeRounding::Down,
            false,
        )
        .unwrap();

        assert_eq!(delivered.iou, amount.iou);
        assert!(!has_line(&sandbox, O1, b"AAA"));
        assert_eq!(owner_count(&sandbox, O1), 0);
    }

    fn oc(sandbox: &Sandbox<'_>, addr: &str) -> u64 {
        let id = decode_account_id(addr).unwrap();
        let b = sandbox.read(&keylet::account(&id)).unwrap();
        let v: Value = serde_json::from_slice(&b).unwrap();
        v["OwnerCount"].as_u64().unwrap()
    }

    /// 30000060 D995194A: after filling the better lsfSell, OfferStream must
    /// still step and reap the maker's worse-page lsfSell that can no longer
    /// deliver its whole TakerGets (OwnerCount 6→4, not 6→5).
    #[test]
    fn ledger_30000060_reaps_maker_short_lsfsell_after_fill() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 10_000_000_000, 0);
        put_acct(&mut ledger, O1, 1_000_000_000, 0);
        put_line(&mut ledger, TAKER, b"BTC", 0);
        put_line(&mut ledger, O1, b"BTC", 1);
        let o1 = decode_account_id(O1).unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O1, "Fee": "10", "Sequence": 1,
                    "Flags": TF_SELL,
                    "TakerGets": {"currency": "BTC", "issuer": ISS, "value": "1"},
                    "TakerPays": "840000000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O1, "Fee": "10", "Sequence": 2,
                    "Flags": TF_SELL,
                    "TakerGets": {"currency": "BTC", "issuer": ISS, "value": "1"},
                    "TakerPays": "845000000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(oc(&sandbox, O1), 2);
        assert!(sandbox.read(&keylet::offer(&o1, 1)).is_some());
        assert!(sandbox.read(&keylet::offer(&o1, 2)).is_some());

        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "12", "Sequence": 1,
                    "Flags": TF_IMMEDIATE_OR_CANCEL,
                    "TakerGets": "841000000",
                    "TakerPays": {"currency": "BTC", "issuer": ISS, "value": "1"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(
            sandbox.read(&keylet::offer(&o1, 1)).is_none(),
            "better lsfSell consumed"
        );
        assert!(
            sandbox.read(&keylet::offer(&o1, 2)).is_none(),
            "worse lsfSell unfunded after the take must be reaped"
        );
        assert_eq!(oc(&sandbox, O1), 0, "both maker offers deleted");
    }

    /// 30000073 3D88107D: placing a buy of XID for XRP must reap the taker's
    /// own inverse Flags=0 offers that would self-cross (OwnerCount 1672→1670).
    #[test]
    fn ledger_30000073_reaps_own_inverse_offers_that_self_cross() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, O1, 10_000_000_000, 0);
        put_line(&mut ledger, O1, b"XID", 400);
        let o1 = decode_account_id(O1).unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O1, "Fee": "10", "Sequence": 1,
                    "TakerGets": {"currency": "XID", "issuer": ISS, "value": "199"},
                    "TakerPays": "1000301600",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O1, "Fee": "10", "Sequence": 2,
                    "TakerGets": {"currency": "XID", "issuer": ISS, "value": "199"},
                    "TakerPays": "997863470",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(oc(&sandbox, O1), 2);

        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": O1, "Fee": "10", "Sequence": 3,
                    "TakerGets": "1002105230",
                    "TakerPays": {"currency": "XID", "issuer": ISS, "value": "199"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(
            sandbox.read(&keylet::offer(&o1, 1)).is_none(),
            "own inverse seq 1 must be reaped"
        );
        assert!(
            sandbox.read(&keylet::offer(&o1, 2)).is_none(),
            "own inverse seq 2 must be reaped"
        );
        assert!(
            sandbox.read(&keylet::offer(&o1, 3)).is_some(),
            "new buy leftover is placed"
        );
        assert_eq!(oc(&sandbox, O1), 1, "two inverse offers reaped, one placed");
    }

    #[test]
    fn ledger_105501188_does_not_retry_amm_after_reaping_self_tip() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 0);
        put_acct(&mut ledger, O3, 1_000_000_000, 0);
        put_line(&mut ledger, TAKER, b"SHR", 100);
        put_line(&mut ledger, O3, b"SHR", 100_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        // A funded inverse offer sits at the AMM's spot quality. It is the
        // taker's own offer, so BookStep will remove it without trading.
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "12", "Sequence": 1,
                    "TakerGets": {"currency": "SHR", "issuer": ISS, "value": "10"},
                    "TakerPays": "100000",
                }),
            ),
            TransactionResult::TesSuccess
        );

        let zero = AccountId::from([0u8; 20]);
        let issuer = decode_account_id(ISS).unwrap();
        let amm_key = keylet::amm(&[0u8; 20], &zero, &ccy(b"SHR"), &issuer);
        sandbox
            .insert(
                amm_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": O3,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let taker = decode_account_id(TAKER).unwrap();
        let self_offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&taker, 1)).unwrap()).unwrap();
        let directory: Hash256 = self_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let self_quality = u64::from_be_bytes(directory.as_bytes()[24..32].try_into().unwrap());
        let pool_spot = rxrpl_amount::get_rate(
            &IOUAmount::from_decimal_string("1000000000").unwrap(),
            &IOUAmount::from_decimal_string("100000").unwrap(),
        )
        .unwrap();
        assert_eq!(self_quality, pool_spot);
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "12", "Sequence": 2,
            "TakerGets": "101000",
            "TakerPays": {"currency": "SHR", "issuer": ISS, "value": "10"},
        });
        let threshold = crossing_quality_threshold(
            &Leg::parse(&tx["TakerGets"]).unwrap(),
            &Leg::parse(&tx["TakerPays"]).unwrap(),
            false,
            false,
        );
        assert!(rxrpl_amount::is_better_quality(self_quality, threshold));
        let rules = Rules::from_enabled([feature_id("FlowCross")]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        assert!(sandbox.read(&keylet::offer(&taker, 1)).is_none());
        assert!(
            sandbox.read(&keylet::offer(&taker, 2)).is_some(),
            "pool XRP={}, pool SHR={}, taker XRP={}, taker SHR={}",
            xrp_bal(&sandbox, O3),
            line_bal(&sandbox, O3, b"SHR"),
            xrp_bal(&sandbox, TAKER),
            line_bal(&sandbox, TAKER, b"SHR"),
        );
        assert_eq!(xrp_bal(&sandbox, O3), 1_000_000_000);
        assert_eq!(line_bal(&sandbox, O3, b"SHR"), "100000");
        assert_eq!(xrp_bal(&sandbox, TAKER), 1_000_000_000);
        assert_eq!(line_bal(&sandbox, TAKER, b"SHR"), "100");
    }

    #[test]
    fn zero_drop_selected_amm_keeps_funded_inverse_self_offer() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 0);
        put_acct(&mut ledger, O3, 1_000_000_000_000, 0);
        put_line(&mut ledger, TAKER, b"SHR", 0);
        put_line(&mut ledger, O3, b"SHR", 80_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let taker = decode_account_id(TAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();

        // Place a funded inverse offer before the AMM exists. It sells XRP,
        // so the taker's tiny SHR holding does not make this old offer dry.
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "12", "Sequence": 1,
                    "TakerGets": "465532",
                    "TakerPays": {"currency": "SHR", "issuer": ISS, "value": "46"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        let old_key = keylet::offer(&taker, 1);
        let old_before = sandbox.read(&old_key).unwrap();
        let old_offer: Value = serde_json::from_slice(&old_before).unwrap();
        let old_dir: Hash256 = old_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let old_quality = u64::from_be_bytes(old_dir.as_bytes()[24..32].try_into().unwrap());

        let line_key = keylet::trust_line(&taker, &issuer, &ccy(b"SHR"));
        let mut line: Value = serde_json::from_slice(&sandbox.read(&line_key).unwrap()).unwrap();
        let tiny_balance = "0.00000000212625";
        line["Balance"]["value"] = Value::String(if taker.as_bytes() < issuer.as_bytes() {
            tiny_balance.to_string()
        } else {
            format!("-{tiny_balance}")
        });
        sandbox
            .update(line_key, serde_json::to_vec(&line).unwrap())
            .unwrap();

        let zero = AccountId::from([0u8; 20]);
        sandbox
            .insert(
                keylet::amm(&[0u8; 20], &zero, &ccy(b"SHR"), &issuer),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": O3,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let pool_spot = rxrpl_amount::get_rate(
            &IOUAmount::from_decimal_string("80000000").unwrap(),
            &IOUAmount::from_decimal_string("1000000000000").unwrap(),
        )
        .unwrap();
        assert!(rxrpl_amount::is_better_quality(pool_spot, old_quality));
        let out = crate::amm_helpers::swap_asset_in(
            &Number::from_iou(&IOUAmount::from_decimal_string("80000000").unwrap()),
            &Number::from_int(1_000_000_000_000),
            &Number::from_iou(&IOUAmount::from_decimal_string(tiny_balance).unwrap()),
            0,
            true,
        );
        assert_eq!(out.to_xrp_drops(), 0);

        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "12", "Sequence": 2,
            "TakerGets": {"currency": "SHR", "issuer": ISS, "value": "46"},
            "TakerPays": "465237",
        });
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        assert_eq!(sandbox.read(&old_key), Some(old_before));
        assert!(sandbox.read(&keylet::offer(&taker, 2)).is_some());
        assert_eq!(oc(&sandbox, TAKER), 2);
        assert_eq!(xrp_bal(&sandbox, O3), 1_000_000_000_000);
        assert_eq!(line_bal(&sandbox, O3, b"SHR"), "80000000");
    }

    #[test]
    fn ledger_105500679_speculative_probe_keeps_funded_self_offer() {
        let mut ledger = Ledger::genesis();
        put_acct(&mut ledger, ISS, 1_000_000_000, 0);
        put_acct(&mut ledger, TAKER, 1_000_000_000, 0);
        put_line(&mut ledger, TAKER, b"SHR", 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        assert_eq!(
            apply_offer(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "12", "Sequence": 1,
                    "TakerGets": "460655",
                    "TakerPays": {"currency": "SHR", "issuer": ISS, "value": "46"},
                }),
            ),
            TransactionResult::TesSuccess
        );

        let taker = decode_account_id(TAKER).unwrap();
        let offer_key = keylet::offer(&taker, 1);
        let offer_before = sandbox.read(&offer_key).unwrap();
        let offer: Value = serde_json::from_slice(&offer_before).unwrap();
        let directory = offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse::<Hash256>()
            .unwrap();
        let owner_dir_before = crate::owner_dir::collect_owner_dir_entries(&sandbox, &taker);
        let account_bytes = sandbox.read(&keylet::account(&taker)).unwrap();
        let mut taker_acct: Value = serde_json::from_slice(&account_bytes).unwrap();
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        assert!(
            advance_past_unfunded_quality(
                &mut ctx,
                &taker,
                &mut taker_acct,
                &directory,
                QualityAdvanceMode::Speculative,
            )
            .unwrap()
            .funded_tip_remains
        );
        assert_eq!(ctx.view.read(&offer_key).unwrap(), offer_before);
        assert!(ctx.view.read(&directory).is_some());
        assert_eq!(
            crate::owner_dir::collect_owner_dir_entries(ctx.view, &taker),
            owner_dir_before
        );
        assert_eq!(taker_acct["OwnerCount"].as_u64(), Some(1));

        assert!(
            !advance_past_unfunded_quality(
                &mut ctx,
                &taker,
                &mut taker_acct,
                &directory,
                QualityAdvanceMode::Executing,
            )
            .unwrap()
            .funded_tip_remains
        );
        assert!(ctx.view.read(&offer_key).is_none());
        assert!(ctx.view.read(&directory).is_none());
        assert!(
            !crate::owner_dir::collect_owner_dir_entries(ctx.view, &taker)
                .contains(&offer_key.to_string())
        );
        assert_eq!(taker_acct["OwnerCount"].as_u64(), Some(0));
    }
}

#[cfg(test)]
mod flow_cross_own_unfunded {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::apply_view::ApplyView;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const TAKER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const MAKER: &str = "rpP2JgiMyTF5jR5hLG3xHCPi1knBb1v9cM";
    const MAKER2: &str = "rf1BiGeXwwQoi8Z2ueFYTEXSwuJYfV2Jpn";
    const ISS: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    #[test]
    fn ledger_105501848_one_drop_offer_skips_unchanged_trust_lines() {
        let holder = decode_account_id(TAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let key = keylet::trust_line(&holder, &issuer, &currency);
        let mut ledger = Ledger::genesis();
        let (low, high) = if holder.as_bytes() < issuer.as_bytes() {
            (TAKER, ISS)
        } else {
            (ISS, TAKER)
        };
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {
                "currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji",
                "value": "1000.123456789012"
            },
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "0"},
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&line).unwrap())
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({});
        let rules = Rules::from_enabled([feature_id("fixUniversalNumber")]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        credit_line(
            &mut ctx,
            &holder,
            &issuer,
            &currency,
            &IOUAmount::from_decimal_string("0.00000000000001").unwrap(),
            true,
        )
        .unwrap();
        assert!(sandbox.into_changes().updates.is_empty());
    }

    fn apply_flow(
        sandbox: &mut Sandbox<'_>,
        fees: &FeeSettings,
        tx: &serde_json::Value,
    ) -> TransactionResult {
        let rules = Rules::from_enabled([feature_id("FlowCross")]);
        let mut child = sandbox.child();
        let result = {
            let mut ctx = ApplyContext {
                tx,
                view: &mut child,
                rules: &rules,
                fees,
            };
            OfferCreateTransactor.apply(&mut ctx).unwrap()
        };
        if result.is_success() {
            sandbox.merge_child_changes(child.into_changes());
        }
        result
    }

    fn owner_count(sandbox: &Sandbox<'_>, addr: &str) -> u64 {
        let id = decode_account_id(addr).unwrap();
        let b = sandbox.read(&keylet::account(&id)).unwrap();
        let v: Value = serde_json::from_slice(&b).unwrap();
        v["OwnerCount"].as_u64().unwrap()
    }

    fn xrp_balance(sandbox: &Sandbox<'_>, addr: &str) -> u64 {
        let id = decode_account_id(addr).unwrap();
        let b = sandbox.read(&keylet::account(&id)).unwrap();
        let v: Value = serde_json::from_slice(&b).unwrap();
        v["Balance"].as_str().unwrap().parse().unwrap()
    }

    fn put_usd_line(ledger: &mut Ledger, holder: &str, balance: i64) {
        let holder_id = decode_account_id(holder).unwrap();
        let issuer_id = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
        let stored = if balance == 0 {
            "0".to_string()
        } else if holder_is_low {
            balance.to_string()
        } else {
            format!("-{}", balance)
        };
        let (low, high) = if holder_is_low {
            (holder, ISS)
        } else {
            (ISS, holder)
        };
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {
                "currency": "USD",
                "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji",
                "value": stored,
            },
            "LowLimit": {"currency": "USD", "issuer": low, "value": "1000"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&holder_id, &issuer_id, &currency),
                serde_json::to_vec(&line).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn ledger_105501906_direct_amm_fib_quote_does_not_activate_second_strand() {
        const TAKER: &str = "rTeLeproT3BVgjWoYrDYpKbBLXPaVMkge";
        const XAH_ISSUER: &str = "rswh1fvyLqHizBS2awu1vs6QcmwTBd9qiv";
        const RLUSD_ISSUER: &str = "rMxCKbEDwqr76QuheSUMdEGf4B9xJ8m5De";
        const POOL: &str = "rK9WLBTBxvgMtCsPo5aq7da6NhXoSKZor7";
        const RLUSD: &str = "524C555344000000000000000000000000000000";
        let pool = decode_account_id(POOL).unwrap();
        let xah_issuer = decode_account_id(XAH_ISSUER).unwrap();
        let rlusd_issuer = decode_account_id(RLUSD_ISSUER).unwrap();
        let xah = helpers::currency_to_bytes("XAH");
        let rlusd = helpers::currency_to_bytes(RLUSD);
        let mut ledger = Ledger::genesis();
        for (issuer, currency, holding) in [
            (XAH_ISSUER, "XAH", "241898.5753545085"),
            (RLUSD_ISSUER, RLUSD, "4371.101523406265"),
        ] {
            let issuer_id = decode_account_id(issuer).unwrap();
            let (low, high, stored) = if pool.as_bytes() < issuer_id.as_bytes() {
                (POOL, issuer, holding.to_string())
            } else {
                (issuer, POOL, format!("-{holding}"))
            };
            let line = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": currency, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                "LowLimit": {"currency": currency, "issuer": low, "value": "0"},
                "HighLimit": {"currency": currency, "issuer": high, "value": "0"},
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::trust_line(&pool, &issuer_id, &helpers::currency_to_bytes(currency)),
                    serde_json::to_vec(&line).unwrap(),
                )
                .unwrap();
        }
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM", "Account": POOL, "TradingFee": 953,
        });
        ledger
            .put_state(
                keylet::amm(&xah, &xah_issuer, &rlusd, &rlusd_issuer),
                serde_json::to_vec(&amm).unwrap(),
            )
            .unwrap();
        // The best direct offer is far outside this taker's limit.
        let book = keylet::book_dir(&xah, &xah_issuer, &rlusd, &rlusd_issuer);
        let poor_quality = rxrpl_amount::get_rate_round_even(
            &IOUAmount::from_decimal_string("200000").unwrap(),
            &IOUAmount::from_decimal_string("11.399999772").unwrap(),
        )
        .unwrap();
        ledger
            .put_state(
                book_dir_with_quality(&book, poor_quality),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [Hash256::new([3u8; 32]).to_string()],
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        let tx = serde_json::json!({
            "TakerGets": {"currency": "XAH", "issuer": XAH_ISSUER, "value": "100000"},
            "TakerPays": {"currency": RLUSD, "issuer": RLUSD_ISSUER, "value": "1805"},
            "Flags": TF_SELL,
        });
        let ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let taker = decode_account_id(TAKER).unwrap();
        let input = Leg::parse(&tx["TakerGets"]).unwrap();
        let output = Leg::parse(&tx["TakerPays"]).unwrap();
        let limit = flow_crossing_threshold(&ctx, &taker, &input, &output, true, false);
        assert!(offer_crossing_strand_reaches_limit(
            &ctx,
            &taker,
            &tx["TakerGets"],
            &tx["TakerPays"],
            limit,
        ));
        assert!(!offer_crossing_strand_reaches_limit_with_amm_mode(
            &ctx,
            &taker,
            &tx["TakerGets"],
            &tx["TakerPays"],
            limit,
            true,
        ));
    }

    #[test]
    fn ledger_105501906_bridge_forward_uses_reverse_amm_threshold() {
        // TX20: the XRP/RLUSD AMM is selected in the reverse pass after the
        // direct strand drops out. Its spot quote is worse than the resting
        // CLOB tip, so repricing the forward pass against that tip dries the
        // bridge after the XAH/XRP CLOB has already produced 672659411 drops.
        let pool_in = Number::from_int(1_859_904_487_328);
        let pool_out =
            Number::from_iou(&IOUAmount::from_decimal_string("2066118.962732664").unwrap());
        let amm = AmmLiquidity {
            in_is_xrp: true,
            out_is_xrp: false,
            round_even_quality: true,
            fix_amm_v1_2: true,
            tfee: 208,
            initial_pool_in: pool_in,
            initial_pool_out: pool_out,
        };
        let clob = 6_494_189_227_102_275_047;
        let limit = clob - 1;
        let ctx = AmmContext::new(false);
        assert!(
            amm.get_offer(&pool_in, &pool_out, Some(clob), &ctx)
                .is_none()
        );
        let threshold = offer_crossing_amm_threshold(Some(clob), Some(limit), ctx.multi_path());
        assert_eq!(threshold, None);
        let offer = amm.get_offer(&pool_in, &pool_out, threshold, &ctx).unwrap();
        let (_, delivered) = offer.limit_in(&Number::from_int(672_659_411), false);
        assert!(!delivered.is_zero());
    }

    #[test]
    fn ledger_105501906_first_bridge_band_does_not_reach_quality_cap() {
        let requested = Number::from_int(1_000_000_000);
        let cap = Number::from_iou(&IOUAmount::from_decimal_string("10167.66431569803").unwrap());
        let first_band =
            Number::from_iou(&IOUAmount::from_decimal_string("745.416355142").unwrap());
        assert!(!strand_quality_cap_reached(&requested, &cap, &first_band));
        assert!(strand_quality_cap_reached(&requested, &cap, &cap));
        assert!(!strand_quality_cap_reached(&cap, &cap, &cap));
    }

    #[test]
    fn ledger_105501906_adjusted_amm_fragment_worse_than_offer_limit() {
        let limit = rxrpl_amount::get_rate_round_even(
            &IOUAmount::from_decimal_string("100000").unwrap(),
            &IOUAmount::from_decimal_string("1805").unwrap(),
        )
        .unwrap();
        let first = Number::from_iou(&IOUAmount::from_decimal_string("41108.90641193176").unwrap());
        let first_out = Number::from_iou(&IOUAmount::from_decimal_string("745.416355142").unwrap());
        assert!(bridge_adjusted_pass_meets_limit(
            &first, &first_out, limit, true
        ));
        let surplus =
            Number::from_iou(&IOUAmount::from_decimal_string("13482.9877506115").unwrap());
        let surplus_out =
            Number::from_iou(&IOUAmount::from_decimal_string("242.7407044606865").unwrap());
        assert!(!bridge_adjusted_pass_meets_limit(
            &surplus,
            &surplus_out,
            limit,
            true,
        ));
    }

    #[test]
    fn ledger_105501905_two_offer_cross_preserves_taker_bbrl_balance() {
        const BBRL_ISSUER: &str = "rH5CJsqvNqZGxrMyGaqLEoMWRYcVTAPZMt";
        const RLUSD_ISSUER: &str = "rMxCKbEDwqr76QuheSUMdEGf4B9xJ8m5De";
        const TAKER: &str = "rEariHxzoKiUAe5wZ5jVA8YkZnankoQGUo";
        const FIRST: &str = "rsevkvX1VDf6CiLNEaCJnChJewVt9rGeEc";
        const SECOND: &str = "rnhHK8HbgNQMT7Wj7giUC2ez3j5CcgDDgT";
        const BBRL: &str = "4242524C00000000000000000000000000000000";
        const RLUSD: &str = "524C555344000000000000000000000000000000";

        let mut ledger = Ledger::genesis();
        for address in [BBRL_ISSUER, RLUSD_ISSUER, TAKER, FIRST, SECOND] {
            let id = decode_account_id(address).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": "1000000000", "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        for (holder, issuer, currency, balance) in [
            (TAKER, BBRL_ISSUER, BBRL, "1535.346215477393"),
            (TAKER, RLUSD_ISSUER, RLUSD, "686.5101154185002"),
            (FIRST, BBRL_ISSUER, BBRL, "974.7302373571656"),
            (FIRST, RLUSD_ISSUER, RLUSD, "47.2390484049222"),
            (SECOND, BBRL_ISSUER, BBRL, "0.000000149825"),
            (SECOND, RLUSD_ISSUER, RLUSD, "248.0038304841648"),
        ] {
            let holder_id = decode_account_id(holder).unwrap();
            let issuer_id = decode_account_id(issuer).unwrap();
            let (low, high, stored) = if holder_id.as_bytes() < issuer_id.as_bytes() {
                (holder, issuer, balance.to_string())
            } else {
                (issuer, holder, format!("-{balance}"))
            };
            let line = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": currency, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                "LowLimit": {"currency": currency, "issuer": low, "value": "1000000000"},
                "HighLimit": {"currency": currency, "issuer": high, "value": "1000000000"},
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::trust_line(
                        &holder_id,
                        &issuer_id,
                        &helpers::currency_to_bytes(currency),
                    ),
                    serde_json::to_vec(&line).unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let bbrl = |value: &str| {
            serde_json::json!({
                "currency": BBRL, "issuer": BBRL_ISSUER, "value": value,
            })
        };
        let rlusd = |value: &str| {
            serde_json::json!({
                "currency": RLUSD, "issuer": RLUSD_ISSUER, "value": value,
            })
        };
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        for (owner, gets, pays) in [
            (FIRST, "47.2390485466388", "243.6968036424"),
            (SECOND, "248.003831", "1279.4021633628"),
        ] {
            let tx = serde_json::json!({
                "TransactionType": "OfferCreate", "Account": owner,
                "Fee": "10", "Sequence": 1, "Flags": TF_PASSIVE | TF_SELL,
                "TakerGets": rlusd(gets), "TakerPays": bbrl(pays),
            });
            let mut child = sandbox.child();
            let mut ctx = ApplyContext {
                tx: &tx,
                view: &mut child,
                rules: &rules,
                fees: &fees,
            };
            assert_eq!(
                OfferCreateTransactor.apply(&mut ctx),
                Ok(TransactionResult::TesSuccess)
            );
            sandbox.merge_child_changes(child.into_changes());
        }
        let first = decode_account_id(FIRST).unwrap();
        let second = decode_account_id(SECOND).unwrap();
        let first_key = keylet::offer(&first, 1);
        let second_key = keylet::offer(&second, 1);
        let mut first_offer: Value =
            serde_json::from_slice(&sandbox.read(&first_key).unwrap()).unwrap();
        let second_offer: Value =
            serde_json::from_slice(&sandbox.read(&second_key).unwrap()).unwrap();
        // The first offer had been partly filled before this ledger. Its
        // amounts drifted one quality ULP, while BookDirectory stayed at the
        // original quality shared with the second offer.
        let old_dir: Hash256 = first_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let oracle_dir: Hash256 = second_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            oracle_dir.to_string(),
            "623BC4019CE424B43F7088AF6BDE6CBFEAF4A41CB32D5EF7551253E6B977A000"
        );
        remove_from_book_dir(&mut sandbox, &old_dir, &first_key).unwrap();
        add_to_book_dir(&mut sandbox, &oracle_dir, &first_key, &[]).unwrap();
        first_offer["BookDirectory"] = Value::String(oracle_dir.to_string());
        sandbox
            .update(first_key, serde_json::to_vec(&first_offer).unwrap())
            .unwrap();
        let mut dir: Value = serde_json::from_slice(&sandbox.read(&oracle_dir).unwrap()).unwrap();
        dir["Indexes"] = serde_json::json!([first_key.to_string(), second_key.to_string()]);
        sandbox
            .update(oracle_dir, serde_json::to_vec(&dir).unwrap())
            .unwrap();

        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "10", "Sequence": 1, "Flags": TF_SELL | TF_IMMEDIATE_OR_CANCEL,
            "TakerGets": bbrl("1023.509763"), "TakerPays": rlusd("198.043623"),
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let taker = decode_account_id(TAKER).unwrap();
        let issuer = decode_account_id(BBRL_ISSUER).unwrap();
        let key = keylet::trust_line(&taker, &issuer, &helpers::currency_to_bytes(BBRL));
        let line: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
        assert_eq!(line["Balance"]["value"], "511.836452477393");
        let first = decode_account_id(FIRST).unwrap();
        let second = decode_account_id(SECOND).unwrap();
        assert!(sandbox.read(&keylet::offer(&first, 1)).is_none());
        let offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&second, 1)).unwrap()).unwrap();
        assert_ne!(offer["TakerPays"]["value"], "1279.4021633628");
    }

    #[test]
    fn ledger_105501442_rejects_fee_adjusted_amm_before_self_tip() {
        let mut ledger = Ledger::genesis();
        for (address, drops) in [
            (ISS, 1_000_000_000u64),
            (TAKER, 10_000_000_000),
            (MAKER2, 102_398_903_505),
        ] {
            let id = decode_account_id(address).unwrap();
            let mut account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": drops.to_string(), "Sequence": 1,
                "OwnerCount": 0, "Flags": 0,
            });
            if address == ISS {
                account["TransferRate"] = Value::from(1_001_500_000u64);
            }
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("BTC");
        for (holder, balance) in [(TAKER, "1"), (MAKER2, "1.769570217575273")] {
            let holder_id = decode_account_id(holder).unwrap();
            let holder_is_low = holder_id.as_bytes() < issuer.as_bytes();
            let stored = if holder_is_low {
                balance.to_string()
            } else {
                format!("-{balance}")
            };
            let (low, high) = if holder_is_low {
                (holder, ISS)
            } else {
                (ISS, holder)
            };
            let line = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": "BTC", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                "LowLimit": {"currency": "BTC", "issuer": low, "value": "1000"},
                "HighLimit": {"currency": "BTC", "issuer": high, "value": "1000"},
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::trust_line(&holder_id, &issuer, &currency),
                    serde_json::to_vec(&line).unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let btc =
            |value: &str| serde_json::json!({"currency": "BTC", "issuer": ISS, "value": value});
        let taker = decode_account_id(TAKER).unwrap();
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "12", "Sequence": 1,
                    "TakerGets": "1000000000", "TakerPays": btc("0.017288"),
                }),
            ),
            TransactionResult::TesSuccess
        );
        let self_key = keylet::offer(&taker, 1);
        let self_before = sandbox.read(&self_key).unwrap();
        let self_offer: Value = serde_json::from_slice(&self_before).unwrap();
        let directory: Hash256 = self_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            u64::from_be_bytes(directory.as_bytes()[24..32].try_into().unwrap()),
            0x4A06_2455_9DE3_4000
        );
        let pool = decode_account_id(MAKER2).unwrap();
        let amm_key = keylet::amm(&currency, &issuer, &[0u8; 20], &AccountId::from([0u8; 20]));
        let amm = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AMM", "Account": MAKER2, "TradingFee": 19,
        }))
        .unwrap();
        sandbox.insert(amm_key, amm.clone()).unwrap();
        let pool_line_key = keylet::trust_line(&pool, &issuer, &currency);
        let pool_line_before = sandbox.read(&pool_line_key).unwrap();
        let pool_xrp_before = xrp_balance(&sandbox, MAKER2);
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "12", "Sequence": 2,
            "TakerGets": btc("0.012675345816"), "TakerPays": "734160000",
        });
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let input = Leg::parse(&tx["TakerGets"]).unwrap();
        let output = Leg::parse(&tx["TakerPays"]).unwrap();
        let threshold = flow_crossing_threshold(&ctx, &taker, &input, &output, true, false);
        let clob_threshold = crossing_quality_threshold(&input, &output, true, false);
        assert!(clob_threshold < 0x4A06_2455_9DE3_4000);
        assert!(0x4A06_2455_9DE3_4000 < threshold);
        assert!(!offer_crossing_strand_reaches_limit(
            &ctx,
            &taker,
            &tx["TakerGets"],
            &tx["TakerPays"],
            threshold
        ));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        assert_eq!(sandbox.read(&self_key).unwrap(), self_before);
        assert_eq!(sandbox.read(&amm_key).unwrap(), amm);
        assert_eq!(sandbox.read(&pool_line_key).unwrap(), pool_line_before);
        assert_eq!(xrp_balance(&sandbox, MAKER2), pool_xrp_before);
        let placed: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&taker, 2)).unwrap()).unwrap();
        assert_eq!(placed["TakerGets"], tx["TakerGets"]);
        assert_eq!(placed["TakerPays"], tx["TakerPays"]);
    }

    #[test]
    fn ledger_105501940_btc_pool_quality_cap_uses_net_input() {
        let input = IOUAmount::from_decimal_string("0.012672409176").unwrap();
        let output = IOUAmount::from_decimal_string("734160000").unwrap();
        let fee = IOUAmount::from_parts(1_001_500_000, -9, false).unwrap();
        let gross = IOUAmount::mul_round(&input, &fee, true).unwrap();
        let gross_quality = rxrpl_amount::get_rate_round_even(&gross, &output).unwrap();
        let net_quality = rxrpl_amount::get_rate_round_even(&input, &output).unwrap();
        let pool_in =
            Number::from_iou(&IOUAmount::from_decimal_string("1.768361324152221").unwrap());
        let pool_out = Number::from_int(102_468_919_112);
        let (net_spend, drops) = crate::amm_helpers::limit_input_quality(
            &pool_in,
            &pool_out,
            net_quality,
            19,
            false,
            true,
        )
        .unwrap();
        assert!(gross_quality > net_quality);
        let (gross_cap, _) = crate::amm_helpers::limit_input_quality(
            &pool_in,
            &pool_out,
            gross_quality,
            19,
            false,
            true,
        )
        .unwrap();
        let funded =
            Number::from_iou(&IOUAmount::from_decimal_string("0.0016898940319902").unwrap());
        assert!(num_gt(&gross_cap, &funded));
        assert_eq!(
            net_spend.to_iou().to_decimal_string(),
            "0.00002888303315329913"
        );
        assert_eq!(drops.to_xrp_drops(), 1_673_302);
        assert_eq!(734_160_000 - drops.to_xrp_drops(), 732_486_698);
    }

    #[test]
    fn ledger_105501572_xrp_to_btc_amm_ignores_output_transfer_rate() {
        const BTC_ISSUER: &str = "rvYAfWj5gh67oV6fW32ZzP3Aw4Eubs59B";
        const SELLER: &str = "rLtCVnojydtQy7GYGTmDegkM15hLX8VuZk";
        const POOL: &str = "rQBeAghWHEwWvKShryBSa5yR3VRX9oyQ5T";
        let mut ledger = Ledger::genesis();
        for (address, balance) in [
            (BTC_ISSUER, "412602491952"),
            (SELLER, "249777468248"),
            (POOL, "102398903505"),
        ] {
            let id = decode_account_id(address).unwrap();
            let mut account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": balance, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            if address == BTC_ISSUER {
                account["TransferRate"] = Value::from(1_001_500_000u64);
            }
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        let issuer = decode_account_id(BTC_ISSUER).unwrap();
        let currency = helpers::currency_to_bytes("BTC");
        for (holder, balance) in [
            (SELLER, "0.0004810006089389414"),
            (POOL, "1.769570217575273"),
        ] {
            let holder_id = decode_account_id(holder).unwrap();
            let holder_is_low = holder_id.as_bytes() < issuer.as_bytes();
            let (low, high, stored) = if holder_is_low {
                (holder, BTC_ISSUER, balance.to_string())
            } else {
                (BTC_ISSUER, holder, format!("-{balance}"))
            };
            let line = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": "BTC", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                "LowLimit": {"currency": "BTC", "issuer": low, "value": "0"},
                "HighLimit": {"currency": "BTC", "issuer": high, "value": "999999999"},
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::trust_line(&holder_id, &issuer, &currency),
                    serde_json::to_vec(&line).unwrap(),
                )
                .unwrap();
        }
        let pool = decode_account_id(POOL).unwrap();
        let amm_key = keylet::amm(&[0u8; 20], &AccountId::from([0u8; 20]), &currency, &issuer);
        ledger
            .put_state(
                amm_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": POOL, "TradingFee": 19,
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": SELLER,
            "Fee": "12", "Sequence": 1, "Flags": TF_SELL,
            "TakerGets": "142840000",
            "TakerPays": {"currency": "BTC", "issuer": BTC_ISSUER, "value": "0.002466961072"},
        });
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
        ]);
        let seller = decode_account_id(SELLER).unwrap();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let input = Leg::parse(&tx["TakerGets"]).unwrap();
        let output = Leg::parse(&tx["TakerPays"]).unwrap();
        let threshold = flow_crossing_threshold(&ctx, &seller, &input, &output, true, false);
        assert!(offer_crossing_strand_reaches_limit(
            &ctx,
            &seller,
            &tx["TakerGets"],
            &tx["TakerPays"],
            threshold,
        ));
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&seller, 1)).unwrap()).unwrap();
        assert_eq!(offer["TakerGets"], "100968518");
        assert_eq!(offer["TakerPays"]["value"], "0.001743807080674399");
        let seller_line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&seller, &issuer, &currency))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(seller_line["Balance"]["value"], "-0.001204154587974575");
        let pool_account: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&pool)).unwrap()).unwrap();
        assert_eq!(pool_account["Balance"], "102440774987");
    }

    #[test]
    fn ledger_105501575_self_tip_limits_one_amm_fragment() {
        const ISSUER: &str = "r9DDsqsG73ETWeSWZW3P758AaoVeFWiX69";
        const TAKER: &str = "rsf76vEErkL8QJA4r6fQprYZRCrZexmLmM";
        const MAKER: &str = "rKkBNf2dZRGzKbfDPtqx5DAVG9yLn9FNaf";
        const POOL: &str = "rrqdNYyedJG9xbqXuwqtnH8c74qfvdkTE";
        const PEPE: &str = "5368657250657065000000000000000000000000";

        let mut ledger = Ledger::genesis();
        for (address, drops) in [
            (ISSUER, "1000000000"),
            (TAKER, "364243977"),
            (MAKER, "444714532"),
            (POOL, "1266998052"),
        ] {
            let id = decode_account_id(address).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        let issuer = decode_account_id(ISSUER).unwrap();
        let currency = helpers::currency_to_bytes(PEPE);
        for (holder, balance) in [
            (TAKER, "183.4028562192301"),
            (MAKER, "122.4080434475157"),
            (POOL, "125825.2490677176"),
        ] {
            let id = decode_account_id(holder).unwrap();
            let low_holder = id.as_bytes() < issuer.as_bytes();
            let (low, high, stored) = if low_holder {
                (holder, ISSUER, balance.to_string())
            } else {
                (ISSUER, holder, format!("-{balance}"))
            };
            let line = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": PEPE, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                "LowLimit": {"currency": PEPE, "issuer": low, "value": "1000000000"},
                "HighLimit": {"currency": PEPE, "issuer": high, "value": "1000000000"},
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::trust_line(&id, &issuer, &currency),
                    serde_json::to_vec(&line).unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let pepe =
            |value: &str| serde_json::json!({"currency": PEPE, "issuer": ISSUER, "value": value});
        for (owner, gets, pays) in [
            (TAKER, "6.621423468649978", "66694"),
            (MAKER, "46", "463498"),
        ] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": owner,
                        "Fee": "12", "Sequence": 1,
                        "TakerGets": pepe(gets), "TakerPays": pays,
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let taker = decode_account_id(TAKER).unwrap();
        let maker = decode_account_id(MAKER).unwrap();
        let self_offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&taker, 1)).unwrap()).unwrap();
        let maker_offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&maker, 1)).unwrap()).unwrap();
        assert_eq!(
            &self_offer["BookDirectory"].as_str().unwrap()[48..],
            "59039415A74CA459"
        );
        assert_eq!(
            &maker_offer["BookDirectory"].as_str().unwrap()[48..],
            "590394692B39EFA7"
        );
        let pool = decode_account_id(POOL).unwrap();
        let amm_key = keylet::amm(&[0u8; 20], &AccountId::from([0u8; 20]), &currency, &issuer);
        sandbox
            .insert(
                amm_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": POOL, "TradingFee": 2,
                }))
                .unwrap(),
            )
            .unwrap();

        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "12", "Sequence": 2, "Flags": 0,
            "TakerGets": "464225", "TakerPays": pepe("46"),
        });
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("fixReducedOffersV1"),
        ]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        assert!(sandbox.read(&keylet::offer(&taker, 1)).is_none());
        let remaining: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&maker, 1)).unwrap()).unwrap();
        assert_eq!(remaining["TakerGets"]["value"], "18.4324931545");
        assert_eq!(remaining["TakerPays"], "185726");
        let pool_account: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&pool)).unwrap()).unwrap();
        assert_eq!(pool_account["Balance"], "1267183689");
    }

    #[test]
    fn ledger_105501643_direct_amm_competes_with_xrp_bridge() {
        const OGRE_ISSUER: &str = "rnCHm7VHh5fK98JLXwuuwX1vkuqaiRA868";
        const RLUSD_ISSUER: &str = "rMxCKbEDwqr76QuheSUMdEGf4B9xJ8m5De";
        const TAKER: &str = "rrhXifoGRLKEfjkjBA9QM6x9MGfJVLDy5p";
        const MAKER: &str = "rU9Ubyq4aRiUqU9m7BgF8ja6yx8z3Q5ZTn";
        const DIRECT_POOL: &str = "rUmSLiSeYZnLTka4FC9fL5zUiPJiGjLduC";
        const BRIDGE_POOL: &str = "rpZQzSYW2snK5Gw2KZogCDU2rLSYLYNG4h";
        const OGRE: &str = "4F47524500000000000000000000000000000000";
        const RLUSD: &str = "524C555344000000000000000000000000000000";
        let mut ledger = Ledger::genesis();
        for (address, drops) in [
            (OGRE_ISSUER, "1019922"),
            (RLUSD_ISSUER, "99926365"),
            (TAKER, "1730349946"),
            (MAKER, "90612954"),
            (DIRECT_POOL, "0"),
            (BRIDGE_POOL, "8944317817"),
        ] {
            let id = decode_account_id(address).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        let ogre_issuer = decode_account_id(OGRE_ISSUER).unwrap();
        let rlusd_issuer = decode_account_id(RLUSD_ISSUER).unwrap();
        let ogre_currency = helpers::currency_to_bytes(OGRE);
        let rlusd_currency = helpers::currency_to_bytes(RLUSD);
        for (holder, issuer_name, currency_name, balance) in [
            (TAKER, OGRE_ISSUER, OGRE, "986.5"),
            (TAKER, RLUSD_ISSUER, RLUSD, "2.849695109552994"),
            (MAKER, RLUSD_ISSUER, RLUSD, "289.8663"),
            (DIRECT_POOL, OGRE_ISSUER, OGRE, "862.2487593536531"),
            (DIRECT_POOL, RLUSD_ISSUER, RLUSD, "8.264288407800006"),
            (BRIDGE_POOL, OGRE_ISSUER, OGRE, "1047552.50502734"),
        ] {
            let holder_id = decode_account_id(holder).unwrap();
            let issuer_id = decode_account_id(issuer_name).unwrap();
            let low_holder = holder_id.as_bytes() < issuer_id.as_bytes();
            let (low, high, stored) = if low_holder {
                (holder, issuer_name, balance.to_string())
            } else {
                (issuer_name, holder, format!("-{balance}"))
            };
            let line = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": currency_name, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                "LowLimit": {"currency": currency_name, "issuer": low, "value": "1000000000"},
                "HighLimit": {"currency": currency_name, "issuer": high, "value": "1000000000"},
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::trust_line(
                        &holder_id,
                        &issuer_id,
                        &helpers::currency_to_bytes(currency_name),
                    ),
                    serde_json::to_vec(&line).unwrap(),
                )
                .unwrap();
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rlusd = |value: &str| serde_json::json!({"currency": RLUSD, "issuer": RLUSD_ISSUER, "value": value});
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": MAKER,
                    "Fee": "12", "Sequence": 1,
                    "TakerGets": rlusd("289.8663"), "TakerPays": "261486848",
                }),
            ),
            TransactionResult::TesSuccess
        );
        for (asset1, issuer1, asset2, issuer2, pool, tfee) in [
            (
                ogre_currency,
                ogre_issuer,
                rlusd_currency,
                rlusd_issuer,
                DIRECT_POOL,
                500,
            ),
            (
                ogre_currency,
                ogre_issuer,
                [0u8; 20],
                AccountId::from([0u8; 20]),
                BRIDGE_POOL,
                343,
            ),
        ] {
            sandbox
                .insert(
                    keylet::amm(&asset1, &issuer1, &asset2, &issuer2),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AMM", "Account": pool, "TradingFee": tfee,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "12", "Sequence": 1,
            "Flags": TF_SELL | TF_IMMEDIATE_OR_CANCEL,
            "TakerGets": {"currency": OGRE, "issuer": OGRE_ISSUER, "value": "986.5"},
            "TakerPays": rlusd("7.284617041433707"),
        });
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
            feature_id("FlowSortStrands"),
        ]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert!(ctx.rules.enabled(&feature_id("FlowSortStrands")));
        assert!(!ctx.rules.enabled(&feature_id("fixFlowSortStrands")));
        let direct =
            build_flow_strand(&ctx, &[tx["TakerGets"].clone(), tx["TakerPays"].clone()]).unwrap();
        assert!(direct.terminal_has_amm());
        assert!(!direct.hops[0].has_clob);
        assert_eq!(transfer_rate_value(&ctx, &ogre_issuer), 1_000_000_000);
        assert_eq!(transfer_rate_value(&ctx, &rlusd_issuer), 1_000_000_000);
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let maker_id = decode_account_id(MAKER).unwrap();
        let maker_offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&maker_id, 1)).unwrap()).unwrap();
        assert_eq!(maker_offer["TakerGets"]["value"], "280.6770894253657");
        assert_eq!(maker_offer["TakerPays"], "253197310");
        let direct_id = decode_account_id(DIRECT_POOL).unwrap();
        let direct_ogre: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(
                    &direct_id,
                    &ogre_issuer,
                    &ogre_currency,
                ))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(direct_ogre["Balance"]["value"], "-869.4195010804415");
        let direct_rlusd: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(
                    &direct_id,
                    &rlusd_issuer,
                    &rlusd_currency,
                ))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(direct_rlusd["Balance"]["value"], "8.196465801205923");
        let taker_id = decode_account_id(TAKER).unwrap();
        let taker_ogre: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&taker_id, &ogre_issuer, &ogre_currency))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(taker_ogre["Balance"]["value"], "0");
        let taker_rlusd: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(
                    &taker_id,
                    &rlusd_issuer,
                    &rlusd_currency,
                ))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(taker_rlusd["Balance"]["value"], "12.10672829078141");
        let bridge_id = decode_account_id(BRIDGE_POOL).unwrap();
        let bridge_account: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&bridge_id)).unwrap()).unwrap();
        assert_eq!(bridge_account["Balance"], "8936028279");
        let bridge_ogre: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(
                    &bridge_id,
                    &ogre_issuer,
                    &ogre_currency,
                ))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(bridge_ogre["Balance"]["value"], "1048531.834285613");
    }

    #[test]
    fn fee_band_clob_tips_keep_the_initial_strand_viable() {
        for (gets, pays, quality) in [
            ("0.886", "1593.30477", 0x5113_c4d3_a3b2_e906u64),
            ("7.247", "13017.771606", 0x5113_c9f6_5683_d7b2),
        ] {
            let mut ledger = Ledger::genesis();
            let issuer = decode_account_id(ISS).unwrap();
            let issuer_account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": ISS,
                "TransferRate": 1_001_500_000u64,
            });
            ledger
                .put_state(
                    keylet::account(&issuer),
                    serde_json::to_vec(&issuer_account).unwrap(),
                )
                .unwrap();
            let taker = decode_account_id(TAKER).unwrap();
            let output_issuer = decode_account_id(MAKER).unwrap();
            let gets_json = serde_json::json!({
                "currency": "USD", "issuer": ISS, "value": gets
            });
            let pays_json = serde_json::json!({
                "currency": "EUR", "issuer": MAKER, "value": pays
            });
            let input = Leg::parse(&gets_json).unwrap();
            let output = Leg::parse(&pays_json).unwrap();
            let book = keylet::book_dir(&input.currency, &issuer, &output.currency, &output_issuer);
            let dir = book_dir_with_quality(&book, quality);
            ledger
                .put_state(
                    dir,
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "DirectoryNode",
                        "Indexes": [Hash256::new([1u8; 32]).to_string()],
                    }))
                    .unwrap(),
                )
                .unwrap();
            let fees = FeeSettings::default();
            let view = LedgerView::with_fees(&ledger, fees.clone());
            let mut sandbox = Sandbox::new(&view);
            let rules =
                Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
            let tx = serde_json::json!({"Flags": TF_PASSIVE});
            let ctx = ApplyContext {
                tx: &tx,
                view: &mut sandbox,
                rules: &rules,
                fees: &fees,
            };
            let threshold = flow_crossing_threshold(&ctx, &taker, &input, &output, true, true);
            let raw = crossing_quality_threshold(&input, &output, true, true);
            assert!(raw < quality && quality < threshold);
            assert!(offer_crossing_strand_reaches_limit(
                &ctx, &taker, &gets_json, &pays_json, threshold,
            ));
        }
    }

    #[test]
    fn flow_cross_creates_missing_taker_output_line() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER] {
            let id = decode_account_id(addr).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": "1000000000",
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, MAKER, 100);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate",
                    "Account": MAKER,
                    "Fee": "10",
                    "Sequence": 1,
                    "TakerGets": usd("100"),
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TesSuccess
        );

        let taker = decode_account_id(TAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        assert!(
            sandbox
                .read(&keylet::trust_line(&taker, &issuer, &currency))
                .is_none()
        );

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate",
                    "Account": TAKER,
                    "Fee": "10",
                    "Sequence": 1,
                    "Flags": TF_IMMEDIATE_OR_CANCEL,
                    "TakerGets": "100000000",
                    "TakerPays": usd("100"),
                }),
            ),
            TransactionResult::TesSuccess
        );

        let line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&taker, &issuer, &currency))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            line["Balance"]["value"]
                .as_str()
                .map(|value| value.trim_start_matches('-')),
            Some("100")
        );
        assert_eq!(owner_count(&sandbox, TAKER), 1);
    }

    /// 105500305 E05D37B2: removing the taker's own inverse offer releases one
    /// owner reserve, but Flow's source sendMax remains fixed to the XRP that was
    /// liquid before the book walk.
    #[test]
    fn source_budget_ignores_reserve_released_during_crossing() {
        let mut ledger = Ledger::genesis();
        for (addr, drops, owner_count) in [
            (ISS, 1_000_000_000_u64, 0_u64),
            (TAKER, 4_034_048, 10),
            (MAKER, 1_000_000_000, 0),
            (MAKER2, 1_000_000_000, 0),
        ] {
            let id = decode_account_id(addr).unwrap();
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops.to_string(), "Sequence": 1,
                "OwnerCount": owner_count, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 10);
        put_usd_line(&mut ledger, MAKER, 1);
        put_usd_line(&mut ledger, MAKER2, 1);

        let fees = FeeSettings {
            base_fee: 10,
            reserve_base: 1_000_000,
            reserve_increment: 200_000,
        };
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});

        // This crossable self-owned offer raises the initial OwnerCount to 11.
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "12", "Sequence": 1,
                    "TakerGets": usd("0.1"), "TakerPays": "80000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(owner_count(&sandbox, TAKER), 11);

        for (maker, sequence, gets, pays) in [
            (MAKER, 1_u64, "0.5", "400000"),
            (MAKER2, 1_u64, "1", "800000"),
        ] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": maker,
                        "Fee": "12", "Sequence": sequence,
                        "TakerGets": usd(gets), "TakerPays": pays,
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "12", "Sequence": 2, "Flags": TF_PASSIVE,
                    "TakerGets": "2000000", "TakerPays": usd("2"),
                }),
            ),
            TransactionResult::TesSuccess
        );

        let taker_id = decode_account_id(TAKER).unwrap();
        assert_eq!(xrp_balance(&sandbox, TAKER), 3_200_000);
        assert_eq!(owner_count(&sandbox, TAKER), 10);
        assert!(sandbox.read(&keylet::offer(&taker_id, 1)).is_none());
        assert!(
            sandbox.read(&keylet::offer(&taker_id, 2)).is_none(),
            "an exhausted source budget must not leave a new resting offer"
        );
    }

    /// 105500477 684D7816: a non-sell FillOrKill that exhausts its
    /// underfunded source after a partial cross must roll the cross back.
    #[test]
    fn fill_or_kill_rolls_back_when_source_is_exhausted() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER] {
            let id = decode_account_id(addr).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": "1000000000",
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 60);
        put_usd_line(&mut ledger, MAKER, 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate",
                    "Account": MAKER,
                    "Fee": "10",
                    "Sequence": 1,
                    "TakerGets": "100000000",
                    "TakerPays": usd("100"),
                }),
            ),
            TransactionResult::TesSuccess
        );

        let maker = decode_account_id(MAKER).unwrap();
        let maker_offer = keylet::offer(&maker, 1);
        let offer_before = sandbox.read(&maker_offer).unwrap();
        let taker_xrp_before = xrp_balance(&sandbox, TAKER);
        let maker_xrp_before = xrp_balance(&sandbox, MAKER);

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate",
                    "Account": TAKER,
                    "Fee": "10",
                    "Sequence": 1,
                    "Flags": TF_FILL_OR_KILL,
                    "TakerGets": usd("100"),
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TecKilled
        );

        assert_eq!(sandbox.read(&maker_offer).unwrap(), offer_before);
        assert_eq!(xrp_balance(&sandbox, TAKER), taker_xrp_before);
        assert_eq!(xrp_balance(&sandbox, MAKER), maker_xrp_before);
        assert_eq!(owner_count(&sandbox, MAKER), 1);
    }

    /// 105503179/F352BE04: tfSell spends the entire liquid XRP budget in an
    /// AMM even though TakerGets names a larger amount. The ticket was already
    /// consumed and the fee already charged when this handler sees the state.
    #[test]
    fn ledger_105503179_sell_fok_spends_funded_amm_budget() {
        const SOURCE: &str = "rHcnqECUfVe4xWzPFNZWnBC5s5RjAzu3fQ";
        const ISSUER: &str = "rGG3wQ4kUzd7Jnmk1n5NWPZjjut62kCBfC";
        const POOL: &str = "rnsRq5ahgbFeRiAgBVvFTafyAgiS9x9Ztn";
        const ARMY: &str = "41524D5900000000000000000000000000000000";
        let mut ledger = Ledger::genesis();
        for (address, balance, owner_count) in [
            (SOURCE, "1005654094", 54),
            (ISSUER, "1000000000", 0),
            (POOL, "230646272324", 1),
        ] {
            let id = decode_account_id(address).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": address,
                "Balance": balance, "Sequence": 1,
                "OwnerCount": owner_count, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        let source = decode_account_id(SOURCE).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        let pool = decode_account_id(POOL).unwrap();
        let currency = helpers::currency_to_bytes(ARMY);
        let (low, high, stored) = if pool.as_bytes() < issuer.as_bytes() {
            (POOL, ISSUER, "43921711.81633347")
        } else {
            (ISSUER, POOL, "-43921711.81633347")
        };
        let pool_line = serde_json::json!({
            "LedgerEntryType": "RippleState", "Flags": 16842752,
            "Balance": {"currency": ARMY, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
            "LowLimit": {"currency": ARMY, "issuer": low, "value": "0"},
            "HighLimit": {"currency": ARMY, "issuer": high, "value": "0"},
        });
        ledger
            .put_state(
                keylet::trust_line(&pool, &issuer, &currency),
                serde_json::to_vec(&pool_line).unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                keylet::amm(&[0u8; 20], &AccountId::from([0u8; 20]), &currency, &issuer),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": POOL, "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let fees = FeeSettings {
            base_fee: 10,
            reserve_base: 1_000_000,
            reserve_increment: 200_000,
        };
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": SOURCE,
            "Fee": "12", "Sequence": 0, "TicketSequence": 103313020,
            "Flags": TF_FILL_OR_KILL | TF_SELL,
            "TakerGets": "4050000000",
            "TakerPays": {"currency": ARMY, "issuer": ISSUER, "value": "764744.5"},
        });
        let rules = Rules::from_enabled([
            feature_id("FlowCross"),
            feature_id("fixUniversalNumber"),
            feature_id("fixAMMv1_2"),
        ]);
        let result = {
            let mut ctx = ApplyContext {
                tx: &tx,
                view: &mut sandbox,
                rules: &rules,
                fees: &fees,
            };
            OfferCreateTransactor.apply(&mut ctx).unwrap()
        };
        assert_eq!(result, TransactionResult::TesSuccess);
        assert_eq!(xrp_balance(&sandbox, SOURCE), 11_800_000);
        assert_eq!(xrp_balance(&sandbox, POOL), 231_640_126_418);
        let pool_line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&pool, &issuer, &currency))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(pool_line["Balance"]["value"], "43733265.30760822");
        let source_line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&source, &issuer, &currency))
                .expect("AMM trade creates source ARMY trust line"),
        )
        .unwrap();
        assert_eq!(source_line["Balance"]["value"], "-188446.50872525");
    }

    /// Ledger 105501294: a stale book tip must not cap an AMM fill after the
    /// tip has been removed for lack of maker funds.
    #[test]
    fn fill_or_kill_retries_amm_after_unfunded_book_tip() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER, MAKER2] {
            let id = decode_account_id(addr).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": "1000000000", "Sequence": 1,
                "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, MAKER, 100);
        put_usd_line(&mut ledger, MAKER2, 10_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": MAKER,
                    "Fee": "10", "Sequence": 1,
                    "TakerGets": usd("100"), "TakerPays": "10050000",
                }),
            ),
            TransactionResult::TesSuccess
        );

        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let maker_line = keylet::trust_line(&maker, &issuer, &currency);
        let mut line: Value = serde_json::from_slice(&sandbox.read(&maker_line).unwrap()).unwrap();
        line["Balance"]["value"] = Value::from("0");
        sandbox
            .update(maker_line, serde_json::to_vec(&line).unwrap())
            .unwrap();

        let zero = AccountId::from([0u8; 20]);
        let amm_key = keylet::amm(&[0u8; 20], &zero, &currency, &issuer);
        sandbox
            .insert(
                amm_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": MAKER2,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 1, "Flags": TF_FILL_OR_KILL,
                    "TakerGets": "11000000", "TakerPays": usd("100"),
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(sandbox.read(&keylet::offer(&maker, 1)).is_none());
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(TAKER).unwrap(), 1))
                .is_none()
        );
        assert_eq!(owner_count(&sandbox, MAKER), 0);
        assert!(xrp_balance(&sandbox, MAKER2) > 1_000_000_000);
        assert!(
            sandbox
                .read(&keylet::trust_line(
                    &decode_account_id(TAKER).unwrap(),
                    &issuer,
                    &currency
                ))
                .is_some()
        );
    }

    #[test]
    fn ledger_105502488_reaps_next_unfunded_quality_before_amm_finishes() {
        let mut ledger = Ledger::genesis();
        for (addr, drops) in [
            (ISS, 1_000_000_000u64),
            (TAKER, 1_000_000_000),
            (MAKER, 1_000_000_000),
            (MAKER2, 9_900_000_000),
        ] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot", "Account": addr,
                        "Balance": drops.to_string(), "Sequence": 1,
                        "OwnerCount": 0, "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }
        put_usd_line(&mut ledger, MAKER, 100);
        put_usd_line(&mut ledger, MAKER2, 1_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});
        for (seq, pays) in [(1, "437000"), (2, "460000"), (3, "464600")] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": MAKER,
                        "Fee": "10", "Sequence": seq,
                        "TakerGets": usd("46"), "TakerPays": pays,
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        assert_eq!(owner_count(&sandbox, MAKER), 3);
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let maker_line = keylet::trust_line(&maker, &issuer, &currency);
        let mut line: Value = serde_json::from_slice(&sandbox.read(&maker_line).unwrap()).unwrap();
        line["Balance"]["value"] = Value::from(if maker.as_bytes() < issuer.as_bytes() {
            "14"
        } else {
            "-14"
        });
        sandbox
            .update(maker_line, serde_json::to_vec(&line).unwrap())
            .unwrap();
        let last_offer: Value =
            serde_json::from_slice(&sandbox.read(&keylet::offer(&maker, 3)).unwrap()).unwrap();
        let last_dir: Hash256 = last_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let zero = AccountId::from([0u8; 20]);
        sandbox
            .insert(
                keylet::amm(&[0u8; 20], &zero, &currency, &issuer),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": MAKER2,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 1,
                    "TakerGets": "463503", "TakerPays": usd("46"),
                }),
            ),
            TransactionResult::TesSuccess
        );
        for seq in 1..=3 {
            assert!(
                sandbox.read(&keylet::offer(&maker, seq)).is_none(),
                "maker offer {seq} must be consumed or reaped"
            );
        }
        assert!(sandbox.read(&last_dir).is_none());
        assert_eq!(owner_count(&sandbox, MAKER), 0);
        let taker = decode_account_id(TAKER).unwrap();
        assert_eq!(
            crate::amm_helpers::iou_holding_number(&sandbox, &taker, &issuer, &currency)
                .to_iou()
                .to_decimal_string(),
            "46"
        );
    }

    #[test]
    fn ledger_105502632_funds_capped_partial_clob_preserves_next_self_offers() {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [
            (ISS, 1_000_000_000u64),
            (TAKER, 1_000_000_000),
            (MAKER, 1_000_000_000),
            (MAKER2, 1_265_366_171),
        ] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot", "Account": addr,
                        "Balance": balance.to_string(), "Sequence": 1,
                        "OwnerCount": 0, "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 47);
        put_usd_line(&mut ledger, MAKER, 0);
        put_usd_line(&mut ledger, MAKER2, 125_987);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": MAKER,
                    "Fee": "10", "Sequence": 1,
                    "TakerGets": "461861", "TakerPays": usd("46"),
                }),
            ),
            TransactionResult::TesSuccess
        );
        for (sequence, gets, pays) in [
            (1, "461079", "46"),
            (2, "230540", "23"),
            (3, "320751", "32"),
        ] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": TAKER,
                        "Fee": "10", "Sequence": sequence,
                        "TakerGets": gets, "TakerPays": usd(pays),
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let taker = decode_account_id(TAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        for (holder, balance) in [(TAKER, "46.03015956239124"), (MAKER2, "125987.5791174534")] {
            let holder_id = decode_account_id(holder).unwrap();
            let key = keylet::trust_line(&holder_id, &issuer, &currency);
            let mut line: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            line["Balance"]["value"] = Value::from(if holder_id.as_bytes() < issuer.as_bytes() {
                balance.to_string()
            } else {
                format!("-{balance}")
            });
            sandbox
                .update(key, serde_json::to_vec(&line).unwrap())
                .unwrap();
        }
        let zero = AccountId::from([0u8; 20]);
        sandbox
            .insert(
                keylet::amm(&[0u8; 20], &zero, &currency, &issuer),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": MAKER2,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let own_offers: Vec<_> = (1..=3).map(|seq| keylet::offer(&taker, seq)).collect();
        let own_before: Vec<_> = own_offers
            .iter()
            .map(|key| sandbox.read(key).unwrap())
            .collect();
        let maker = decode_account_id(MAKER).unwrap();
        let maker_key = keylet::offer(&maker, 1);
        let maker_before = sandbox.read(&maker_key).unwrap();
        let maker_offer: Value = serde_json::from_slice(&maker_before).unwrap();
        let maker_dir: Hash256 = maker_offer["BookDirectory"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let maker_quality = u64::from_be_bytes(maker_dir.as_bytes()[24..32].try_into().unwrap());
        for before in &own_before {
            let offer: Value = serde_json::from_slice(before).unwrap();
            let dir: Hash256 = offer["BookDirectory"].as_str().unwrap().parse().unwrap();
            let quality = u64::from_be_bytes(dir.as_bytes()[24..32].try_into().unwrap());
            assert!(maker_quality < quality, "self offer must follow maker");
        }
        let pool_xrp_before = xrp_balance(&sandbox, MAKER2);
        assert_eq!(owner_count(&sandbox, TAKER), 3);

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 4, "Flags": 0,
                    "TakerGets": usd("47"), "TakerPays": "469861",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(xrp_balance(&sandbox, MAKER2) < pool_xrp_before);
        assert_ne!(sandbox.read(&maker_key).unwrap(), maker_before);
        assert_eq!(owner_count(&sandbox, TAKER), 3);
        for (key, before) in own_offers.iter().zip(&own_before) {
            assert_eq!(sandbox.read(key).as_ref(), Some(before));
        }
        assert!(sandbox.read(&keylet::offer(&taker, 4)).is_none());
        assert_eq!(
            crate::amm_helpers::iou_holding_number(&sandbox, &taker, &issuer, &currency)
                .to_iou()
                .to_decimal_string(),
            "0"
        );
    }

    #[test]
    fn ledger_105502705_microfill_settles_without_touching_unchanged_offer() {
        const RLUSD_ISS: &str = "rMxCKbEDwqr76QuheSUMdEGf4B9xJ8m5De";
        let mut ledger = Ledger::genesis();
        for addr in [ISS, RLUSD_ISS, TAKER, MAKER, MAKER2] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot", "Account": addr,
                        "Balance": "1000000000", "Sequence": 1,
                        "OwnerCount": 0, "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }
        let put_line = |ledger: &mut Ledger,
                        holder: &str,
                        issuer: &str,
                        code: &str,
                        balance: &str| {
            let holder_id = decode_account_id(holder).unwrap();
            let issuer_id = decode_account_id(issuer).unwrap();
            let currency = helpers::currency_to_bytes(code);
            let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
            let stored = if balance == "0" || holder_is_low {
                balance.to_string()
            } else {
                format!("-{balance}")
            };
            let (low, high) = if holder_is_low {
                (holder, issuer)
            } else {
                (issuer, holder)
            };
            ledger
                .put_state(
                    keylet::trust_line(&holder_id, &issuer_id, &currency),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "RippleState",
                        "Balance": {"currency": code, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
                        "LowLimit": {"currency": code, "issuer": low, "value": "1000000"},
                        "HighLimit": {"currency": code, "issuer": high, "value": "1000000"},
                        "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        };
        for (holder, issuer, currency, balance) in [
            (TAKER, ISS, "BBR", "1056.354569321967"),
            (TAKER, RLUSD_ISS, "USD", "49.01300127837607"),
            (MAKER, ISS, "BBR", "0"),
            (MAKER, RLUSD_ISS, "USD", "204.6088496552258"),
            (MAKER2, ISS, "BBR", "0"),
            (MAKER2, RLUSD_ISS, "USD", "20000"),
        ] {
            put_line(&mut ledger, holder, issuer, currency, balance);
        }
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut placement = Sandbox::new(&view);
        let bbr =
            |value: &str| serde_json::json!({"currency": "BBR", "issuer": ISS, "value": value});
        let usd = |value: &str| serde_json::json!({"currency": "USD", "issuer": RLUSD_ISS, "value": value});
        for (account, gets, pays) in [
            (MAKER, "407.5063312366158", "2103.8736869084"),
            (MAKER2, "14948.505014623", "77180.626241"),
        ] {
            assert_eq!(
                apply_flow(
                    &mut placement,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": account,
                        "Fee": "10", "Sequence": 1,
                        "TakerGets": usd(gets), "TakerPays": bbr(pays),
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let maker = decode_account_id(MAKER).unwrap();
        let second = keylet::offer(&decode_account_id(MAKER2).unwrap(), 1);
        let first = keylet::offer(&maker, 1);
        let changes = placement.into_changes();
        for (key, bytes) in changes.inserts.into_iter().chain(changes.updates) {
            ledger.put_state(key, bytes).unwrap();
        }
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let second_before = sandbox.read(&second).unwrap();
        let tx = serde_json::json!({
            "TransactionType": "OfferCreate", "Account": TAKER,
            "Fee": "10", "Sequence": 1,
            "Flags": TF_SELL | TF_IMMEDIATE_OR_CANCEL,
            "TakerGets": bbr("1056.354569"), "TakerPays": usd("204.240553"),
        });
        let rules =
            Rules::from_enabled([feature_id("FlowCross"), feature_id("fixUniversalNumber")]);
        assert_eq!(
            OfferCreateTransactor
                .apply(&mut ApplyContext {
                    tx: &tx,
                    view: &mut sandbox,
                    rules: &rules,
                    fees: &fees,
                })
                .unwrap(),
            TransactionResult::TesSuccess
        );
        assert!(sandbox.read(&first).is_none());
        assert_eq!(sandbox.read(&second).unwrap(), second_before);
        let taker = decode_account_id(TAKER).unwrap();
        for (issuer, currency, expected) in [
            (ISS, "BBR", "0.000000321967"),
            (RLUSD_ISS, "USD", "253.6218509336021"),
        ] {
            let issuer_id = decode_account_id(issuer).unwrap();
            let key = keylet::trust_line(&taker, &issuer_id, &helpers::currency_to_bytes(currency));
            let line: Value = serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
            let stored = if taker.as_bytes() < issuer_id.as_bytes() {
                expected.to_string()
            } else {
                format!("-{expected}")
            };
            assert_eq!(line["Balance"]["value"], stored);
        }
        let changes = sandbox.into_changes();
        assert!(!changes.updates.contains_key(&second));
    }

    #[test]
    fn out_of_quality_ioc_keeps_sub_drop_maker_when_strand_is_rejected() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot", "Account": addr,
                        "Balance": "1000000000", "Sequence": 1,
                        "OwnerCount": 0, "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }
        put_usd_line(&mut ledger, MAKER, 100_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": MAKER,
                    "Fee": "10", "Sequence": 1, "Flags": TF_PASSIVE,
                    "TakerGets": usd("99085.77"), "TakerPays": "37010749",
                }),
            ),
            TransactionResult::TesSuccess
        );
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISS).unwrap();
        let offer_key = keylet::offer(&maker, 1);
        let offer: Value = serde_json::from_slice(&sandbox.read(&offer_key).unwrap()).unwrap();
        let book_dir: Hash256 = offer["BookDirectory"].as_str().unwrap().parse().unwrap();
        let line_key = keylet::trust_line(&maker, &issuer, &helpers::currency_to_bytes("USD"));
        let mut line: Value = serde_json::from_slice(&sandbox.read(&line_key).unwrap()).unwrap();
        line["Balance"]["value"] = Value::from(if maker.as_bytes() < issuer.as_bytes() {
            "0.00072515"
        } else {
            "-0.00072515"
        });
        sandbox
            .update(line_key, serde_json::to_vec(&line).unwrap())
            .unwrap();
        // Carry a partially funded maker offer into the IOC's base view.
        let changes = sandbox.into_changes();
        for (key, bytes) in changes.inserts.into_iter().chain(changes.updates) {
            ledger.put_state(key, bytes).unwrap();
        }
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        assert_eq!(owner_count(&sandbox, MAKER), 1);

        let maker_xrp = xrp_balance(&sandbox, MAKER);
        let taker_xrp = xrp_balance(&sandbox, TAKER);
        let taker = decode_account_id(TAKER).unwrap();
        let mut taker_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker)).unwrap()).unwrap();
        let taker_gets = Value::String("36820892".into());
        let taker_pays = usd("99084.78");
        let source_budget = Leg::parse(&taker_gets).unwrap();
        let out = Leg::parse(&taker_pays).unwrap();
        let inverse_book = keylet::book_dir(
            &source_budget.currency,
            &source_budget.issuer,
            &out.currency,
            &out.issuer,
        );
        let tx = serde_json::json!({"Flags": TF_SELL | TF_IMMEDIATE_OR_CANCEL});
        let rules = Rules::from_enabled([feature_id("FlowCross")]);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let threshold = flow_crossing_threshold(&ctx, &taker, &source_budget, &out, false, false);
        let quality = book_tip_quality(&ctx, &inverse_book).unwrap();
        assert!(quality > threshold);
        let offer_out = Leg::parse(&offer["TakerGets"]).unwrap();
        let offer_in = Leg::parse(&offer["TakerPays"]).unwrap();
        let funds = owner_funds_leg(&mut ctx, &maker, &offer_out);
        assert!(should_reap_tiny_increased_quality(
            &maker, &offer_in, &offer_out, &funds, quality
        ));
        assert!(!offer_crossing_strand_reaches_limit(
            &ctx,
            &taker,
            &taker_gets,
            &taker_pays,
            threshold,
        ));
        let (remaining_pays, remaining_gets, crossed, _) = cross_offers(
            &mut ctx,
            &taker,
            &mut taker_acct,
            &taker_pays,
            &taker_gets,
            &inverse_book,
            true,
            false,
            &source_budget,
            false,
        )
        .unwrap();
        assert_eq!(remaining_pays, taker_pays);
        assert_eq!(remaining_gets, taker_gets);
        assert!(!crossed);
        assert!(sandbox.read(&offer_key).is_some());
        assert!(sandbox.read(&book_dir).is_some());
        assert_eq!(owner_count(&sandbox, MAKER), 1);
        assert_eq!(xrp_balance(&sandbox, MAKER), maker_xrp);
        assert_eq!(xrp_balance(&sandbox, TAKER), taker_xrp);
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(TAKER).unwrap(), 1))
                .is_none()
        );
    }

    /// A self-owned book tip can make the first AMM probe dry. Flow then
    /// removes that tip and crosses the next maker before trying the AMM again.
    #[test]
    fn self_tip_does_not_reprobe_amm_before_next_book_offer() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER, MAKER2] {
            let id = decode_account_id(addr).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": "1000000000", "Sequence": 1,
                "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 1000);
        put_usd_line(&mut ledger, MAKER, 0);
        put_usd_line(&mut ledger, MAKER2, 99_400);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let usd =
            |value: &str| serde_json::json!({"currency": "USD", "issuer": ISS, "value": value});

        for (account, gets, pays) in [(TAKER, "463143", "46"), (MAKER, "231001", "23")] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": account,
                        "Fee": "10", "Sequence": 1,
                        "TakerGets": gets, "TakerPays": usd(pays),
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let zero = AccountId::from([0u8; 20]);
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let amm_key = keylet::amm(&[0u8; 20], &zero, &currency, &issuer);
        sandbox
            .insert(
                amm_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": MAKER2,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 2,
                    "TakerGets": usd("46"), "TakerPays": "461714",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(TAKER).unwrap(), 1))
                .is_none()
        );
        assert!(
            sandbox
                .read(&keylet::offer(&decode_account_id(MAKER).unwrap(), 1))
                .is_none()
        );
        assert_eq!(xrp_balance(&sandbox, MAKER), 999_768_999);
        assert_eq!(xrp_balance(&sandbox, MAKER2), 999_769_287);
    }

    /// When the taker asks for four drops and its maximum IOU input would
    /// also round to four drops, the AMM charges the amount needed for those
    /// four drops, leaving the rest of the input with the taker.
    #[test]
    fn exact_native_output_uses_amm_output_limit() {
        let mut ledger = Ledger::genesis();
        for (addr, drops) in [
            (ISS, 1_000_000_000u64),
            (TAKER, 1_000_000_000),
            (MAKER2, 219_684_506_991),
        ] {
            let id = decode_account_id(addr).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops.to_string(), "Sequence": 1,
                "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 1);
        put_usd_line(&mut ledger, MAKER2, 23_599_869_094);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let issuer = decode_account_id(ISS).unwrap();
        let currency = helpers::currency_to_bytes("USD");
        let zero = AccountId::from([0u8; 20]);
        sandbox
            .insert(
                keylet::amm(&[0u8; 20], &zero, &currency, &issuer),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AMM", "Account": MAKER2,
                    "TradingFee": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 1,
                    "TakerGets": {"currency": "USD", "issuer": ISS, "value": "0.4533786164"},
                    "TakerPays": "4",
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert_eq!(xrp_balance(&sandbox, MAKER2), 219_684_506_987);
        let taker = decode_account_id(TAKER).unwrap();
        let remaining =
            crate::amm_helpers::iou_holding_number(&sandbox, &taker, &issuer, &currency);
        assert!(
            !num_le(
                &remaining,
                &rxrpl_amount::number::Number::from_iou(
                    &IOUAmount::from_decimal_string("0.55").unwrap()
                )
            ),
            "the output-limited AMM must leave more than 0.55 USD"
        );
    }

    /// 105500215 91B10CB9: Flow's direct book removes funded inverse offers
    /// owned by the taker rather than silently stepping over them.
    #[test]
    fn flow_cross_reaps_funded_own_inverse_offers() {
        let mut ledger = Ledger::genesis();
        let taker_id = decode_account_id(TAKER).unwrap();
        let iss_id = decode_account_id(ISS).unwrap();
        for (addr, drops) in [(ISS, "1000000000"), (TAKER, "10000000000")] {
            let id = decode_account_id(addr).unwrap();
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let usd = helpers::currency_to_bytes("USD");
        let holder_is_low = taker_id.as_bytes() < iss_id.as_bytes();
        let (low, high) = if holder_is_low {
            (TAKER, ISS)
        } else {
            (ISS, TAKER)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {
                "currency": "USD", "issuer": ISS,
                "value": if holder_is_low { "400" } else { "-400" },
            },
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        for (sequence, pays) in [(1, "1000301600"), (2, "997863470")] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": TAKER,
                        "Fee": "10", "Sequence": sequence,
                        "TakerGets": {"currency": "USD", "issuer": ISS, "value": "199"},
                        "TakerPays": pays,
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        assert_eq!(owner_count(&sandbox, TAKER), 2);

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 3,
                    "TakerGets": "1002105230",
                    "TakerPays": {"currency": "USD", "issuer": ISS, "value": "199"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(sandbox.read(&keylet::offer(&taker_id, 1)).is_none());
        assert!(sandbox.read(&keylet::offer(&taker_id, 2)).is_none());
        assert!(sandbox.read(&keylet::offer(&taker_id, 3)).is_some());
        assert_eq!(owner_count(&sandbox, TAKER), 1);
    }

    #[test]
    fn ledger_30000048_inverse_book_prefix_matches_dcbe() {
        let xrp = [0u8; 20];
        let xrp_iss = AccountId::from([0u8; 20]);
        let mut btc = [0u8; 20];
        btc[12..15].copy_from_slice(b"BTC");
        let iss = decode_account_id("rchGBxcD1A1C2tdxF6papQYZ8kjRKMYcL").unwrap();
        let book = keylet::book_dir(&xrp, &xrp_iss, &btc, &iss);
        let prefix = hex::encode_upper(&book.as_bytes()[0..24]);
        assert_eq!(prefix, "DCBE2F8E6D6301D83DA0FB697299BFEECD45EF73EFD3EC72");
        let page_q =
            u64::from_be_bytes(hex::decode("5E1DFD8B851B2CA9").unwrap().try_into().unwrap());
        let pays = IOUAmount::from_decimal_string("1.404741775486649").unwrap();
        let gets = IOUAmount::from_decimal_string("11375915323").unwrap();
        let threshold = get_rate(&gets, &pays).unwrap();
        assert!(
            page_q > threshold,
            "45874350 should be past the new offer's quality"
        );
    }

    #[test]
    fn ledger_30000048_reaps_own_underfunded_sell() {
        // 45874350: lsfSell, TakerGets 2.011 BTC, owner holds 0.008. Not zero so
        // the old is_zero() path left it; rippled treats a sell that cannot
        // deliver its whole TakerGets as unfunded.
        let mut ledger = Ledger::genesis();
        let taker_id = decode_account_id(TAKER).unwrap();
        let iss_id = decode_account_id(ISS).unwrap();
        for (addr, drops) in [(ISS, "1000000000"), (TAKER, "1000000000")] {
            let id = decode_account_id(addr).unwrap();
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let usd = helpers::currency_to_bytes("USD");
        let holder_is_low = taker_id.as_bytes() < iss_id.as_bytes();
        let stored = if holder_is_low { "10" } else { "-10" };
        let (low, high) = if holder_is_low {
            (TAKER, ISS)
        } else {
            (ISS, TAKER)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": ISS, "value": stored},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        let r1 = apply_flow(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "10", "Sequence": 1,
                "Flags": TF_SELL,
                "TakerGets": {"currency": "USD", "issuer": ISS, "value": "10"},
                "TakerPays": "2000000000",
            }),
        );
        assert_eq!(r1, TransactionResult::TesSuccess);
        assert_eq!(owner_count(&sandbox, TAKER), 1);

        let mut line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&taker_id, &iss_id, &usd))
                .unwrap(),
        )
        .unwrap();
        let drained = if holder_is_low { "1" } else { "-1" };
        line["Balance"]["value"] = Value::String(drained.into());
        sandbox
            .update(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&line).unwrap(),
            )
            .unwrap();

        let r2 = apply_flow(
            &mut sandbox,
            &fees,
            &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "10", "Sequence": 2,
                "TakerGets": "100000000",
                "TakerPays": {"currency": "USD", "issuer": ISS, "value": "1"},
            }),
        );
        assert_eq!(r2, TransactionResult::TesSuccess);
        assert_eq!(
            owner_count(&sandbox, TAKER),
            1,
            "underfunded lsfSell own inverse offer must be reaped"
        );
        assert!(
            sandbox.read(&keylet::offer(&taker_id, 1)).is_none(),
            "seq 1 sell offer deleted"
        );
    }

    #[test]
    fn passive_offer_preserves_equal_quality_own_underfunded_sell() {
        let mut ledger = Ledger::genesis();
        let taker_id = decode_account_id(TAKER).unwrap();
        let iss_id = decode_account_id(ISS).unwrap();
        for (addr, drops) in [(ISS, "1000000000"), (TAKER, "1000000000")] {
            let id = decode_account_id(addr).unwrap();
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let usd = helpers::currency_to_bytes("USD");
        let holder_is_low = taker_id.as_bytes() < iss_id.as_bytes();
        let stored = if holder_is_low { "10" } else { "-10" };
        let (low, high) = if holder_is_low {
            (TAKER, ISS)
        } else {
            (ISS, TAKER)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": ISS, "value": stored},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let passive_sell = TF_PASSIVE | TF_SELL;
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 1, "Flags": passive_sell,
                    "TakerGets": {"currency": "USD", "issuer": ISS, "value": "10"},
                    "TakerPays": "100000000",
                }),
            ),
            TransactionResult::TesSuccess
        );

        let mut line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&taker_id, &iss_id, &usd))
                .unwrap(),
        )
        .unwrap();
        line["Balance"]["value"] = Value::String(if holder_is_low { "1" } else { "-1" }.into());
        sandbox
            .update(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&line).unwrap(),
            )
            .unwrap();

        // The next passive offer's quality gate is tighter than this old
        // inverse offer. A speculative AMM quote may still activate the strand,
        // but materialising its out-of-quality tip must leave the short self
        // sell in the book (105502135 / TX1).
        let offer_key = keylet::offer(&taker_id, 1);
        let old_offer = sandbox.read(&offer_key).unwrap();
        let old: Value = serde_json::from_slice(&old_offer).unwrap();
        let dir: Hash256 = old["BookDirectory"].as_str().unwrap().parse().unwrap();
        let mut taker_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker_id)).unwrap()).unwrap();
        let rules = Rules::new();
        let probe_tx = serde_json::json!({});
        {
            let mut ctx = ApplyContext {
                tx: &probe_tx,
                view: &mut sandbox,
                rules: &rules,
                fees: &fees,
            };
            assert!(
                advance_past_unfunded_quality(
                    &mut ctx,
                    &taker_id,
                    &mut taker_acct,
                    &dir,
                    QualityAdvanceMode::PreQuality,
                )
                .unwrap()
                .funded_tip_remains
            );
        }
        assert_eq!(sandbox.read(&offer_key).unwrap(), old_offer);
        assert_eq!(owner_count(&sandbox, TAKER), 1);

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER,
                    "Fee": "10", "Sequence": 2, "Flags": passive_sell,
                    "TakerGets": "100000000",
                    "TakerPays": {"currency": "USD", "issuer": ISS, "value": "10"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(sandbox.read(&keylet::offer(&taker_id, 1)).is_some());
        assert!(sandbox.read(&keylet::offer(&taker_id, 2)).is_some());
        assert_eq!(owner_count(&sandbox, TAKER), 2);
    }

    #[test]
    fn expired_offer_past_quality_threshold_is_not_reaped() {
        let mut ledger = Ledger::genesis();
        let taker_id = decode_account_id(TAKER).unwrap();
        let iss_id = decode_account_id(ISS).unwrap();
        for (addr, drops) in [(ISS, "10000000000"), (TAKER, "10000000000")] {
            let id = decode_account_id(addr).unwrap();
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let usd = helpers::currency_to_bytes("USD");
        let holder_is_low = taker_id.as_bytes() < iss_id.as_bytes();
        let (low, high) = if holder_is_low {
            (TAKER, ISS)
        } else {
            (ISS, TAKER)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": ISS, "value": "0"},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "2000"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "2000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": ISS, "Fee": "10",
                    "Sequence": 1, "TakerGets": {"currency": "USD", "issuer": ISS, "value": "324.134308"},
                    "TakerPays": "292000000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        let maker_offer = keylet::offer(&iss_id, 1);
        let mut expired: Value =
            serde_json::from_slice(&sandbox.read(&maker_offer).unwrap()).unwrap();
        expired["Expiration"] = Value::from(0u64);
        sandbox
            .update(maker_offer, serde_json::to_vec(&expired).unwrap())
            .unwrap();

        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "10",
                    "Sequence": 1, "TakerGets": "910000000",
                    "TakerPays": {"currency": "USD", "issuer": ISS, "value": "1045.044"},
                }),
            ),
            TransactionResult::TesSuccess
        );
        assert!(
            sandbox.read(&maker_offer).is_some(),
            "expired offer beyond the taker's quality must remain untouched"
        );
    }

    /// 105500288 736AFECC: after a funded tip drains its maker, OfferStream
    /// advances beyond the taker's quality and reaps the maker's now-unfunded
    /// offers before rejecting the next funded tip.
    #[test]
    fn exhausted_tip_reaps_unfunded_maker_but_keeps_worse_self_offer() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER] {
            let id = decode_account_id(addr).unwrap();
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": "10000000000",
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 1);
        put_usd_line(&mut ledger, MAKER, 1);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate",
                    "Account": TAKER,
                    "Fee": "10",
                    "Sequence": 1,
                    "Flags": TF_PASSIVE,
                    "TakerGets": {"currency": "USD", "issuer": ISS, "value": "1"},
                    "TakerPays": "2100000",
                }),
            ),
            TransactionResult::TesSuccess
        );
        for (sequence, xrp) in [(1, "1000000"), (2, "2000000")] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate",
                        "Account": MAKER,
                        "Fee": "10",
                        "Sequence": sequence,
                        "Flags": TF_PASSIVE,
                        "TakerGets": {"currency": "USD", "issuer": ISS, "value": "1"},
                        "TakerPays": xrp,
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }

        assert_eq!(owner_count(&sandbox, MAKER), 2);
        assert_eq!(
            apply_flow(
                &mut sandbox,
                &fees,
                &serde_json::json!({
                    "TransactionType": "OfferCreate",
                    "Account": TAKER,
                    "Fee": "10",
                    "Sequence": 2,
                    "Flags": TF_IMMEDIATE_OR_CANCEL,
                    "TakerGets": "3000000",
                    "TakerPays": {"currency": "USD", "issuer": ISS, "value": "2"},
                }),
            ),
            TransactionResult::TesSuccess
        );

        let maker = decode_account_id(MAKER).unwrap();
        let taker = decode_account_id(TAKER).unwrap();
        assert!(sandbox.read(&keylet::offer(&maker, 1)).is_none());
        assert!(sandbox.read(&keylet::offer(&maker, 2)).is_none());
        assert!(sandbox.read(&keylet::offer(&taker, 1)).is_some());
        assert_eq!(owner_count(&sandbox, MAKER), 0);
    }

    #[test]
    fn ledger_105502788_post_fill_keeps_worse_funded_self_offers() {
        let mut ledger = Ledger::genesis();
        for addr in [ISS, TAKER, MAKER] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot", "Account": addr,
                        "Balance": "1000000000", "Sequence": 1,
                        "OwnerCount": 0, "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }
        put_usd_line(&mut ledger, TAKER, 10);
        put_usd_line(&mut ledger, MAKER, 1);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        for (account, sequence) in [(MAKER, 1), (TAKER, 1), (TAKER, 2)] {
            assert_eq!(
                apply_flow(
                    &mut sandbox,
                    &fees,
                    &serde_json::json!({
                        "TransactionType": "OfferCreate", "Account": account,
                        "Fee": "10", "Sequence": sequence,
                        "TakerGets": {"currency": "USD", "issuer": ISS, "value": "1"},
                        "TakerPays": "1000000",
                    }),
                ),
                TransactionResult::TesSuccess
            );
        }
        let maker = decode_account_id(MAKER).unwrap();
        let taker = decode_account_id(TAKER).unwrap();
        let maker_offer = keylet::offer(&maker, 1);
        let own_offers = [keylet::offer(&taker, 1), keylet::offer(&taker, 2)];
        let own_before = own_offers.map(|key| sandbox.read(&key).unwrap());
        let offer: Value = serde_json::from_slice(&own_before[0]).unwrap();
        let dir: Hash256 = offer["BookDirectory"].as_str().unwrap().parse().unwrap();
        let quality = u64::from_be_bytes(dir.as_bytes()[24..32].try_into().unwrap());
        let maker_bytes = sandbox.read(&maker_offer).unwrap();
        let maker_value: Value = serde_json::from_slice(&maker_bytes).unwrap();
        assert_eq!(maker_value["BookDirectory"], offer["BookDirectory"]);

        // The first maker has spent its USD in the prior fill; OfferStream
        // must remove that unfunded tip, then stop at the funded self tips.
        let line_key = keylet::trust_line(
            &maker,
            &decode_account_id(ISS).unwrap(),
            &helpers::currency_to_bytes("USD"),
        );
        let mut line: Value = serde_json::from_slice(&sandbox.read(&line_key).unwrap()).unwrap();
        line["Balance"]["value"] = Value::String("0".into());
        sandbox
            .update(line_key, serde_json::to_vec(&line).unwrap())
            .unwrap();

        let remaining_out = Leg::parse(&serde_json::json!({
            "currency": "USD", "issuer": ISS, "value": "2"
        }))
        .unwrap();
        let mode = quality_advance_mode(quality, quality - 1, true, true, &remaining_out);
        assert!(matches!(mode, QualityAdvanceMode::PreQuality));
        assert!(matches!(
            quality_advance_mode(quality, quality, true, true, &remaining_out),
            QualityAdvanceMode::Executing
        ));
        let mut taker_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&taker)).unwrap()).unwrap();
        let tx = serde_json::json!({});
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert!(
            advance_past_unfunded_quality(&mut ctx, &taker, &mut taker_acct, &dir, mode)
                .unwrap()
                .funded_tip_remains
        );
        assert!(ctx.view.read(&maker_offer).is_none());
        for (key, before) in own_offers.iter().zip(&own_before) {
            assert_eq!(ctx.view.read(key).unwrap(), *before);
        }
        assert_eq!(owner_count(&sandbox, MAKER), 0);
        assert_eq!(owner_count(&sandbox, TAKER), 2);
    }

    #[test]
    fn ledger_30000048_taker_path_reaps_own_underfunded_sell() {
        let mut ledger = Ledger::genesis();
        let taker_id = decode_account_id(TAKER).unwrap();
        let iss_id = decode_account_id(ISS).unwrap();
        for (addr, drops) in [(ISS, "1000000000"), (TAKER, "1000000000")] {
            let id = decode_account_id(addr).unwrap();
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": drops, "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            });
            ledger
                .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let usd = helpers::currency_to_bytes("USD");
        let holder_is_low = taker_id.as_bytes() < iss_id.as_bytes();
        let stored = if holder_is_low { "10" } else { "-10" };
        let (low, high) = if holder_is_low {
            (TAKER, ISS)
        } else {
            (ISS, TAKER)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": ISS, "value": stored},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000"},
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules_flow = Rules::from_enabled([feature_id("FlowCross")]);
        let mut ctx = ApplyContext {
            tx: &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "10", "Sequence": 1,
                "Flags": TF_SELL,
                "TakerGets": {"currency": "USD", "issuer": ISS, "value": "10"},
                "TakerPays": "2000000000",
            }),
            view: &mut sandbox,
            rules: &rules_flow,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );
        let mut line: Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::trust_line(&taker_id, &iss_id, &usd))
                .unwrap(),
        )
        .unwrap();
        line["Balance"]["value"] = Value::String(if holder_is_low { "1" } else { "-1" }.into());
        sandbox
            .update(
                keylet::trust_line(&taker_id, &iss_id, &usd),
                serde_json::to_vec(&line).unwrap(),
            )
            .unwrap();
        let rules_taker = Rules::new();
        let mut ctx = ApplyContext {
            tx: &serde_json::json!({
                "TransactionType": "OfferCreate", "Account": TAKER, "Fee": "10", "Sequence": 2,
                "TakerGets": "100000000",
                "TakerPays": {"currency": "USD", "issuer": ISS, "value": "1"},
            }),
            view: &mut sandbox,
            rules: &rules_taker,
            fees: &fees,
        };
        assert_eq!(
            OfferCreateTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );
        let still = sandbox.read(&keylet::offer(&taker_id, 1)).is_some();
        let oc = owner_count(&sandbox, TAKER);
        assert!(
            !still,
            "taker path must delete underfunded lsfSell, oc={oc}"
        );
        assert_eq!(oc, 1, "OwnerCount after reap+place");
    }
}
