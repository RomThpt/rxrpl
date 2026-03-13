use crate::hash_prefix::HashPrefix;

/// Build multi-signing data: prepend the multi-sign prefix to the transaction data.
pub fn build_multi_signing_data(tx_data: &[u8]) -> Vec<u8> {
    let prefix = HashPrefix::TX_MULTI_SIGN.to_bytes();
    let mut result = Vec::with_capacity(prefix.len() + tx_data.len());
    result.extend_from_slice(&prefix);
    result.extend_from_slice(tx_data);
    result
}

/// Append the signer's account ID (20 bytes) to the multi-signing data.
pub fn finish_multi_signing_data(signing_data: &[u8], account_id: &[u8; 20]) -> Vec<u8> {
    let mut result = Vec::with_capacity(signing_data.len() + 20);
    result.extend_from_slice(signing_data);
    result.extend_from_slice(account_id);
    result
}

/// Build the complete multi-signing data in one step.
pub fn build_complete_multi_signing_data(tx_data: &[u8], account_id: &[u8; 20]) -> Vec<u8> {
    let prefix = HashPrefix::TX_MULTI_SIGN.to_bytes();
    let mut result = Vec::with_capacity(prefix.len() + tx_data.len() + 20);
    result.extend_from_slice(&prefix);
    result.extend_from_slice(tx_data);
    result.extend_from_slice(account_id);
    result
}
