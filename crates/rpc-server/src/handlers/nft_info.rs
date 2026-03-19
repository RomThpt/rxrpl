use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;
use rxrpl_primitives::Hash256;

/// Look up a specific NFT by its token ID.
///
/// Searches through NFTokenPage entries to find the NFT and its owner.
/// Returns the NFT details including owner, URI, flags, serial, and taxon.
pub async fn nft_info(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let nft_id_str = params
        .get("nft_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'nft_id'".into()))?;

    let _nft_id = Hash256::from_str(nft_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_id: {e}")))?;

    let nft_bytes = hex::decode(nft_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_id hex: {e}")))?;

    if nft_bytes.len() != 32 {
        return Err(RpcServerError::InvalidParams(
            "nft_id must be 32 bytes (64 hex chars)".into(),
        ));
    }

    let ledger = resolve_ledger(&params, ctx).await?;

    // Extract the issuer from the NFT ID bytes (bytes 4..24 contain the issuer AccountId).
    let issuer_bytes = &nft_bytes[4..24];

    // Parse flags (bytes 0..2) and taxon (bytes 24..28) and serial (bytes 28..32) from the NFT ID.
    let flags = u16::from_be_bytes([nft_bytes[0], nft_bytes[1]]);
    let transfer_fee = u16::from_be_bytes([nft_bytes[2], nft_bytes[3]]);
    let taxon_raw = u32::from_be_bytes([nft_bytes[24], nft_bytes[25], nft_bytes[26], nft_bytes[27]]);
    let serial = u32::from_be_bytes([nft_bytes[28], nft_bytes[29], nft_bytes[30], nft_bytes[31]]);

    // Walk all state entries looking for NFTokenPage objects that contain this NFT.
    // In a full implementation, we would index by issuer account and walk their NFT pages.
    // For now, search through the issuer's NFT pages using the page keylet.
    let issuer_id = rxrpl_primitives::AccountId(
        issuer_bytes
            .try_into()
            .map_err(|_| RpcServerError::Internal("invalid issuer bytes in nft_id".into()))?,
    );

    let page_key = rxrpl_protocol::keylet::nftoken_page_min(&issuer_id);

    let mut found_nft: Option<Value> = None;
    let mut owner_account = hex::encode_upper(issuer_bytes);

    let mut current_key = page_key;
    while let Some(data) = ledger.get_state(&current_key) {
        let page: Value = serde_json::from_slice(data).map_err(|e| {
            RpcServerError::Internal(format!("failed to deserialize nft page: {e}"))
        })?;

        if let Some(tokens) = page.get("NFTokens").and_then(|v| v.as_array()) {
            for token in tokens {
                let nft = token.get("NFToken").unwrap_or(token);
                if let Some(id) = nft.get("NFTokenID").and_then(|v| v.as_str()) {
                    if id.eq_ignore_ascii_case(nft_id_str) {
                        found_nft = Some(nft.clone());
                        // The owner is the account whose pages we are walking
                        if let Some(acct) = page.get("Owner").and_then(|v| v.as_str()) {
                            owner_account = acct.to_string();
                        }
                        break;
                    }
                }
            }
        }

        if found_nft.is_some() {
            break;
        }

        match page.get("NextPageMin").and_then(|v| v.as_str()) {
            Some(next_str) => {
                current_key = next_str
                    .parse()
                    .map_err(|e| RpcServerError::Internal(format!("invalid next page key: {e}")))?;
            }
            None => break,
        }
    }

    match found_nft {
        Some(nft) => Ok(serde_json::json!({
            "nft_id": nft_id_str,
            "owner": owner_account,
            "flags": flags,
            "transfer_fee": transfer_fee,
            "issuer": hex::encode_upper(issuer_bytes),
            "nft_taxon": taxon_raw,
            "nft_serial": serial,
            "uri": nft.get("URI").cloned().unwrap_or(Value::Null),
            "ledger_index": ledger.header.sequence,
        })),
        None => Err(RpcServerError::InvalidParams(format!(
            "NFT not found: {nft_id_str}"
        ))),
    }
}
