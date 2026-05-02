use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct NFTokenAcceptOfferTransactor;

impl Transactor for NFTokenAcceptOfferTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let has_sell = helpers::get_str_field(ctx.tx, "NFTokenSellOffer").is_some();
        let has_buy = helpers::get_str_field(ctx.tx, "NFTokenBuyOffer").is_some();

        if !has_sell && !has_buy {
            return Err(TransactionResult::TemMalformed);
        }

        // Validate hex format
        if let Some(s) = helpers::get_str_field(ctx.tx, "NFTokenSellOffer") {
            if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(TransactionResult::TemMalformed);
            }
        }
        if let Some(s) = helpers::get_str_field(ctx.tx, "NFTokenBuyOffer") {
            if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Verify referenced offers exist
        if let Some(sell_id) = helpers::get_str_field(ctx.tx, "NFTokenSellOffer") {
            let key_bytes = hex::decode(sell_id).map_err(|_| TransactionResult::TemMalformed)?;
            let key =
                Hash256::from_slice(&key_bytes).map_err(|_| TransactionResult::TemMalformed)?;
            if !ctx.view.exists(&key) {
                return Err(TransactionResult::TecNoEntry);
            }
        }
        if let Some(buy_id) = helpers::get_str_field(ctx.tx, "NFTokenBuyOffer") {
            let key_bytes = hex::decode(buy_id).map_err(|_| TransactionResult::TemMalformed)?;
            let key =
                Hash256::from_slice(&key_bytes).map_err(|_| TransactionResult::TemMalformed)?;
            if !ctx.view.exists(&key) {
                return Err(TransactionResult::TecNoEntry);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let sell_id_str = helpers::get_str_field(ctx.tx, "NFTokenSellOffer").map(|s| s.to_string());
        let buy_id_str = helpers::get_str_field(ctx.tx, "NFTokenBuyOffer").map(|s| s.to_string());

        // Handle sell offer acceptance
        if let Some(ref sell_hex) = sell_id_str {
            if buy_id_str.is_none() {
                // Direct sell offer acceptance
                self.accept_sell_offer(ctx, account_str, sell_hex)?;
            }
        }

        // Handle buy offer acceptance
        if let Some(ref buy_hex) = buy_id_str {
            if sell_id_str.is_none() {
                self.accept_buy_offer(ctx, account_str, buy_hex)?;
            }
        }

        // Brokered mode (both sell and buy)
        if let (Some(sell_hex), Some(buy_hex)) = (&sell_id_str, &buy_id_str) {
            self.accept_brokered(ctx, account_str, sell_hex, buy_hex)?;
        }

        // Increment caller's sequence
        let caller_key = keylet::account(&account_id);
        let caller_bytes = ctx
            .view
            .read(&caller_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut caller: Value =
            serde_json::from_slice(&caller_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut caller);
        let caller_data =
            serde_json::to_vec(&caller).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(caller_key, caller_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

impl NFTokenAcceptOfferTransactor {
    fn read_offer(
        ctx: &mut ApplyContext<'_>,
        offer_hex: &str,
    ) -> Result<(Hash256, Value), TransactionResult> {
        let key_bytes = hex::decode(offer_hex).map_err(|_| TransactionResult::TemMalformed)?;
        let key = Hash256::from_slice(&key_bytes).map_err(|_| TransactionResult::TemMalformed)?;
        let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecNoEntry)?;
        let offer: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        Ok((key, offer))
    }

    fn transfer_token(
        ctx: &mut ApplyContext<'_>,
        nftoken_id: &str,
        from_addr: &str,
        to_addr: &str,
    ) -> Result<(), TransactionResult> {
        let from_id =
            decode_account_id(from_addr).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let to_id =
            decode_account_id(to_addr).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Remove from seller's page
        let from_page_key = keylet::nftoken_page_min(&from_id);
        if let Some(page_bytes) = ctx.view.read(&from_page_key) {
            let page: Value =
                serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let mut tokens: Vec<Value> = page
                .get("NFTokens")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Find and remove the token, saving it for the buyer
            let token_obj = tokens
                .iter()
                .find(|t| {
                    t.get("NFTokenID")
                        .and_then(|v| v.as_str())
                        .map(|s| s == nftoken_id)
                        .unwrap_or(false)
                })
                .cloned();

            tokens.retain(|t| {
                t.get("NFTokenID")
                    .and_then(|v| v.as_str())
                    .map(|s| s != nftoken_id)
                    .unwrap_or(true)
            });

            if tokens.is_empty() {
                ctx.view
                    .erase(&from_page_key)
                    .map_err(|_| TransactionResult::TefInternal)?;
            } else {
                let page_obj = serde_json::json!({
                    "LedgerEntryType": "NFTokenPage",
                    "NFTokens": tokens,
                });
                let data =
                    serde_json::to_vec(&page_obj).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(from_page_key, data)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }

            // Adjust seller owner count
            let from_acct_key = keylet::account(&from_id);
            let from_acct_bytes = ctx
                .view
                .read(&from_acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut from_acct: Value = serde_json::from_slice(&from_acct_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut from_acct, -1);
            let from_data =
                serde_json::to_vec(&from_acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(from_acct_key, from_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Add to buyer's page
            if let Some(token) = token_obj {
                let to_page_key = keylet::nftoken_page_min(&to_id);
                let mut to_tokens: Vec<Value> =
                    if let Some(to_page_bytes) = ctx.view.read(&to_page_key) {
                        let to_page: Value = serde_json::from_slice(&to_page_bytes)
                            .map_err(|_| TransactionResult::TefInternal)?;
                        to_page
                            .get("NFTokens")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };

                to_tokens.push(token);

                let to_page_obj = serde_json::json!({
                    "LedgerEntryType": "NFTokenPage",
                    "NFTokens": to_tokens,
                });
                let to_data =
                    serde_json::to_vec(&to_page_obj).map_err(|_| TransactionResult::TefInternal)?;

                if ctx.view.exists(&to_page_key) {
                    ctx.view
                        .update(to_page_key, to_data)
                        .map_err(|_| TransactionResult::TefInternal)?;
                } else {
                    ctx.view
                        .insert(to_page_key, to_data)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }

                // Adjust buyer owner count
                let to_acct_key = keylet::account(&to_id);
                let to_acct_bytes = ctx
                    .view
                    .read(&to_acct_key)
                    .ok_or(TransactionResult::TerNoAccount)?;
                let mut to_acct: Value = serde_json::from_slice(&to_acct_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
                helpers::adjust_owner_count(&mut to_acct, 1);
                let to_acct_data =
                    serde_json::to_vec(&to_acct).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(to_acct_key, to_acct_data)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        }

        Ok(())
    }

    fn transfer_xrp(
        ctx: &mut ApplyContext<'_>,
        from_addr: &str,
        to_addr: &str,
        amount: u64,
    ) -> Result<(), TransactionResult> {
        if amount == 0 {
            return Ok(());
        }
        let from_id =
            decode_account_id(from_addr).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let to_id =
            decode_account_id(to_addr).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let from_key = keylet::account(&from_id);
        let from_bytes = ctx
            .view
            .read(&from_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut from_acct: Value =
            serde_json::from_slice(&from_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let from_balance = helpers::get_balance(&from_acct);
        if from_balance < amount {
            return Err(TransactionResult::TecUnfundedPayment);
        }
        helpers::set_balance(&mut from_acct, from_balance - amount);
        let from_data =
            serde_json::to_vec(&from_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(from_key, from_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        let to_key = keylet::account(&to_id);
        let to_bytes = ctx
            .view
            .read(&to_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut to_acct: Value =
            serde_json::from_slice(&to_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let to_balance = helpers::get_balance(&to_acct);
        helpers::set_balance(&mut to_acct, to_balance + amount);
        let to_data = serde_json::to_vec(&to_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(to_key, to_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(())
    }

    fn erase_offer_and_adjust(
        ctx: &mut ApplyContext<'_>,
        offer_key: &Hash256,
        owner_addr: &str,
    ) -> Result<(), TransactionResult> {
        ctx.view
            .erase(offer_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        let owner_id =
            decode_account_id(owner_addr).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let owner_acct_key = keylet::account(&owner_id);
        let owner_bytes = ctx
            .view
            .read(&owner_acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut owner_acct: Value =
            serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut owner_acct, -1);
        let data = serde_json::to_vec(&owner_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(owner_acct_key, data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(())
    }

    fn accept_sell_offer(
        &self,
        ctx: &mut ApplyContext<'_>,
        buyer_addr: &str,
        sell_hex: &str,
    ) -> Result<(), TransactionResult> {
        let (sell_key, sell_offer) = Self::read_offer(ctx, sell_hex)?;
        let seller_addr = sell_offer["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let nftoken_id = sell_offer["NFTokenID"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let amount: u64 = sell_offer["Amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // If sell offer is targeted (has Destination), only that destination
        // can accept it. Otherwise the offer is open to anyone.
        if let Some(dst) = sell_offer.get("Destination").and_then(|v| v.as_str()) {
            if dst != buyer_addr {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        // Transfer XRP from buyer to seller
        Self::transfer_xrp(ctx, buyer_addr, &seller_addr, amount)?;
        // Transfer token from seller to buyer
        Self::transfer_token(ctx, &nftoken_id, &seller_addr, buyer_addr)?;
        // Erase offer
        Self::erase_offer_and_adjust(ctx, &sell_key, &seller_addr)?;

        Ok(())
    }

    fn accept_buy_offer(
        &self,
        ctx: &mut ApplyContext<'_>,
        seller_addr: &str,
        buy_hex: &str,
    ) -> Result<(), TransactionResult> {
        let (buy_key, buy_offer) = Self::read_offer(ctx, buy_hex)?;
        let buyer_addr = buy_offer["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let nftoken_id = buy_offer["NFTokenID"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let amount: u64 = buy_offer["Amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Caller (seller) must own the NFT to accept a buy offer.
        // Verify by checking the seller's NFTokenPage contains the token.
        let seller_id = decode_account_id(seller_addr)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let page_key = keylet::nftoken_page_min(&seller_id);
        let owns = ctx
            .view
            .read(&page_key)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|page| page.get("NFTokens").cloned())
            .and_then(|tokens| tokens.as_array().cloned())
            .map(|toks| {
                toks.iter().any(|t| {
                    t.get("NFTokenID")
                        .and_then(|v| v.as_str())
                        .map(|s| s == nftoken_id)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if !owns {
            return Err(TransactionResult::TecNoPermission);
        }

        // The accept caller is the token owner (seller)
        // Transfer XRP from buyer to seller (caller)
        Self::transfer_xrp(ctx, &buyer_addr, seller_addr, amount)?;
        // Transfer token from seller (caller) to buyer
        Self::transfer_token(ctx, &nftoken_id, seller_addr, &buyer_addr)?;
        // Erase offer
        Self::erase_offer_and_adjust(ctx, &buy_key, &buyer_addr)?;

        Ok(())
    }

    fn accept_brokered(
        &self,
        ctx: &mut ApplyContext<'_>,
        _broker_addr: &str,
        sell_hex: &str,
        buy_hex: &str,
    ) -> Result<(), TransactionResult> {
        let (sell_key, sell_offer) = Self::read_offer(ctx, sell_hex)?;
        let (buy_key, buy_offer) = Self::read_offer(ctx, buy_hex)?;

        let seller_addr = sell_offer["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let buyer_addr = buy_offer["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let nftoken_id = sell_offer["NFTokenID"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        let sell_amount: u64 = sell_offer["Amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let buy_amount: u64 = buy_offer["Amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Buyer pays seller the sell amount, broker gets the difference
        Self::transfer_xrp(ctx, &buyer_addr, &seller_addr, sell_amount)?;
        if buy_amount > sell_amount {
            let broker_fee = buy_amount - sell_amount;
            Self::transfer_xrp(ctx, &buyer_addr, _broker_addr, broker_fee)?;
        }

        // Transfer token
        Self::transfer_token(ctx, &nftoken_id, &seller_addr, &buyer_addr)?;

        // Erase both offers
        Self::erase_offer_and_adjust(ctx, &sell_key, &seller_addr)?;
        Self::erase_offer_and_adjust(ctx, &buy_key, &buyer_addr)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::handlers::nftoken_create_offer::NFTokenCreateOfferTransactor;
    use crate::handlers::nftoken_mint::NFTokenMintTransactor;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SELLER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BUYER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_sell_offer() -> (Ledger, String, String) {
        let mut ledger = Ledger::genesis();

        // Setup seller account
        let seller_id = decode_account_id(SELLER).unwrap();
        let seller_key = keylet::account(&seller_id);
        let seller_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": SELLER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(seller_key, serde_json::to_vec(&seller_acct).unwrap())
            .unwrap();

        // Setup buyer account
        let buyer_id = decode_account_id(BUYER).unwrap();
        let buyer_key = keylet::account(&buyer_id);
        let buyer_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": BUYER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(buyer_key, serde_json::to_vec(&buyer_acct).unwrap())
            .unwrap();

        // Mint token
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let mint_tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": SELLER,
            "NFTokenTaxon": 0,
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &mint_tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        NFTokenMintTransactor.apply(&mut ctx).unwrap();

        // Get token ID
        let page_key = keylet::nftoken_page_min(&seller_id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let nftoken_id = page["NFTokens"][0]["NFTokenID"]
            .as_str()
            .unwrap()
            .to_string();

        sandbox.into_changes().apply_to_ledger(&mut ledger).unwrap();

        // Create sell offer
        let view2 = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox2 = Sandbox::new(&view2);
        let offer_tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": SELLER,
            "NFTokenID": nftoken_id,
            "Amount": "5000000",
            "Flags": 1, // sell
            "Fee": "12",
            "Sequence": 2,
        });
        let mut ctx2 = ApplyContext {
            tx: &offer_tx,
            view: &mut sandbox2,
            rules: &rules,
            fees: &fees,
        };
        NFTokenCreateOfferTransactor.apply(&mut ctx2).unwrap();

        let offer_key = keylet::nftoken_offer(&seller_id, 2);
        let offer_id = hex::encode(offer_key.as_bytes()).to_uppercase();

        sandbox2
            .into_changes()
            .apply_to_ledger(&mut ledger)
            .unwrap();

        (ledger, nftoken_id, offer_id)
    }

    #[test]
    fn accept_sell_offer() {
        let (ledger, _nftoken_id, offer_id) = setup_with_sell_offer();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenAcceptOffer",
            "Account": BUYER,
            "NFTokenSellOffer": offer_id,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenAcceptOfferTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify buyer has the token
        let buyer_id = decode_account_id(BUYER).unwrap();
        let buyer_page_key = keylet::nftoken_page_min(&buyer_id);
        let buyer_page_bytes = sandbox.read(&buyer_page_key).unwrap();
        let buyer_page: Value = serde_json::from_slice(&buyer_page_bytes).unwrap();
        assert_eq!(buyer_page["NFTokens"].as_array().unwrap().len(), 1);

        // Verify seller no longer has the token
        let seller_id = decode_account_id(SELLER).unwrap();
        let seller_page_key = keylet::nftoken_page_min(&seller_id);
        assert!(sandbox.read(&seller_page_key).is_none());

        // Verify XRP transferred
        let buyer_key = keylet::account(&buyer_id);
        let buyer_bytes = sandbox.read(&buyer_key).unwrap();
        let buyer: Value = serde_json::from_slice(&buyer_bytes).unwrap();
        assert_eq!(
            buyer["Balance"].as_str().unwrap().parse::<u64>().unwrap(),
            100_000_000 - 5_000_000
        );
    }

    #[test]
    fn accept_buy_offer() {
        let (mut ledger, nftoken_id, _) = setup_with_sell_offer();
        let fees = FeeSettings::default();
        let rules = Rules::new();

        // Create a buy offer from buyer
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let buy_tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": BUYER,
            "NFTokenID": nftoken_id,
            "Amount": "3000000",
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &buy_tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        NFTokenCreateOfferTransactor.apply(&mut ctx).unwrap();

        let buyer_id = decode_account_id(BUYER).unwrap();
        let buy_offer_key = keylet::nftoken_offer(&buyer_id, 1);
        let buy_offer_id = hex::encode(buy_offer_key.as_bytes()).to_uppercase();

        sandbox.into_changes().apply_to_ledger(&mut ledger).unwrap();

        // Seller accepts buy offer
        let view2 = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox2 = Sandbox::new(&view2);
        let accept_tx = serde_json::json!({
            "TransactionType": "NFTokenAcceptOffer",
            "Account": SELLER,
            "NFTokenBuyOffer": buy_offer_id,
            "Fee": "12",
            "Sequence": 3,
        });
        let mut ctx2 = ApplyContext {
            tx: &accept_tx,
            view: &mut sandbox2,
            rules: &rules,
            fees: &fees,
        };
        let result = NFTokenAcceptOfferTransactor.apply(&mut ctx2).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }

    #[test]
    fn preflight_no_offer() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenAcceptOffer",
            "Account": BUYER,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            NFTokenAcceptOfferTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
