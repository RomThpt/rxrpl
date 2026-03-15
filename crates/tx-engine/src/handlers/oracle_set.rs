use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct OracleSetTransactor;

impl Transactor for OracleSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_u32_field(ctx.tx, "OracleDocumentID").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        if helpers::get_u32_field(ctx.tx, "LastUpdateTime").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        let series = helpers::get_array_field(ctx.tx, "PriceDataSeries")
            .ok_or(TransactionResult::TemMalformed)?;
        if series.is_empty() {
            return Err(TransactionResult::TecArrayEmpty);
        }
        if series.len() > 10 {
            return Err(TransactionResult::TecArrayTooLarge);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let doc_id = helpers::get_u32_field(ctx.tx, "OracleDocumentID").unwrap();
        let oracle_key = keylet::oracle(&account_id, doc_id);

        // On update, Provider cannot change
        if ctx.view.exists(&oracle_key) {
            if let Some(new_provider) = helpers::get_str_field(ctx.tx, "Provider") {
                let entry_bytes = ctx.view.read(&oracle_key)
                    .ok_or(TransactionResult::TefInternal)?;
                let entry: serde_json::Value = serde_json::from_slice(&entry_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
                if let Some(existing_provider) = entry.get("Provider").and_then(|v| v.as_str()) {
                    if existing_provider != new_provider {
                        return Err(TransactionResult::TemMalformed);
                    }
                }
            }
        } else {
            // On create, Provider is required
            if helpers::get_str_field(ctx.tx, "Provider").is_none() {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let doc_id = helpers::get_u32_field(ctx.tx, "OracleDocumentID").unwrap();
        let oracle_key = keylet::oracle(&account_id, doc_id);
        let is_create = !ctx.view.exists(&oracle_key);

        let mut entry = if is_create {
            serde_json::json!({
                "LedgerEntryType": "Oracle",
                "Owner": account_str,
                "OracleDocumentID": doc_id,
                "Flags": 0,
            })
        } else {
            let entry_bytes = ctx
                .view
                .read(&oracle_key)
                .ok_or(TransactionResult::TefInternal)?;
            serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?
        };

        if let Some(provider) = helpers::get_str_field(ctx.tx, "Provider") {
            entry["Provider"] = serde_json::Value::String(provider.to_string());
        }
        if let Some(asset_class) = helpers::get_str_field(ctx.tx, "AssetClass") {
            entry["AssetClass"] = serde_json::Value::String(asset_class.to_string());
        }
        if let Some(last_update_time) = helpers::get_u32_field(ctx.tx, "LastUpdateTime") {
            entry["LastUpdateTime"] = serde_json::Value::from(last_update_time);
        }
        if let Some(series) = ctx.tx.get("PriceDataSeries") {
            entry["PriceDataSeries"] = series.clone();
        }

        let entry_data =
            serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;

        if is_create {
            ctx.view
                .insert(oracle_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut account, 1);
        } else {
            ctx.view
                .update(oracle_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        let account_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(account_key, account_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_account() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_missing_oracle_document_id() {
        let tx = serde_json::json!({
            "TransactionType": "OracleSet",
            "Account": ALICE,
            "LastUpdateTime": 1000,
            "PriceDataSeries": [{"price": 1}],
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            OracleSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_empty_price_data_series() {
        let tx = serde_json::json!({
            "TransactionType": "OracleSet",
            "Account": ALICE,
            "OracleDocumentID": 1,
            "LastUpdateTime": 1000,
            "PriceDataSeries": [],
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            OracleSetTransactor.preflight(&ctx),
            Err(TransactionResult::TecArrayEmpty)
        );
    }

    #[test]
    fn preflight_too_many_price_entries() {
        let series: Vec<serde_json::Value> = (0..11)
            .map(|i| serde_json::json!({"price": i}))
            .collect();
        let tx = serde_json::json!({
            "TransactionType": "OracleSet",
            "Account": ALICE,
            "OracleDocumentID": 1,
            "LastUpdateTime": 1000,
            "PriceDataSeries": series,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            OracleSetTransactor.preflight(&ctx),
            Err(TransactionResult::TecArrayTooLarge)
        );
    }

    #[test]
    fn preclaim_create_without_provider_rejects() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "OracleSet",
            "Account": ALICE,
            "OracleDocumentID": 1,
            "LastUpdateTime": 1000,
            "PriceDataSeries": [{"price": 1}],
            "Fee": "12",
        });
        let ctx = PreclaimContext { tx: &tx, view: &view, rules: &rules };
        assert_eq!(
            OracleSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_creates_oracle_entry() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "OracleSet",
            "Account": ALICE,
            "OracleDocumentID": 1,
            "Provider": "chainlink",
            "LastUpdateTime": 1000,
            "PriceDataSeries": [{"BaseAsset": "XRP", "QuoteAsset": "USD", "AssetPrice": "500"}],
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = OracleSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        let oracle_key = keylet::oracle(&id, 1);
        assert!(sandbox.exists(&oracle_key));

        let entry_bytes = sandbox.read(&oracle_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["Provider"].as_str().unwrap(), "chainlink");
        assert_eq!(entry["Owner"].as_str().unwrap(), ALICE);

        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
    }
}
