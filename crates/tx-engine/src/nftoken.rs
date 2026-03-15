use rxrpl_protocol::TransactionResult;

/// Generate an NFTokenID from its components.
///
/// Format (64 hex chars = 32 bytes):
/// flags(8) + transfer_fee(4) + reserved(4) + issuer(40) + seq(8)
pub fn generate_nftoken_id(
    flags: u32,
    transfer_fee: u16,
    issuer_hex: &str,
    token_seq: u32,
) -> String {
    format!(
        "{:08X}{:04X}{:04X}{}{:08X}",
        flags, transfer_fee, 0u16, issuer_hex, token_seq
    )
}

/// Parse an NFTokenID string into (flags, transfer_fee, issuer_hex, token_seq).
pub fn parse_nftoken_id(id: &str) -> Result<(u32, u16, String, u32), TransactionResult> {
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(TransactionResult::TemMalformed);
    }

    let flags = u32::from_str_radix(&id[0..8], 16)
        .map_err(|_| TransactionResult::TemMalformed)?;
    let transfer_fee = u16::from_str_radix(&id[8..12], 16)
        .map_err(|_| TransactionResult::TemMalformed)?;
    // bytes 12..16 are reserved
    let issuer_hex = id[16..56].to_string();
    let token_seq = u32::from_str_radix(&id[56..64], 16)
        .map_err(|_| TransactionResult::TemMalformed)?;

    Ok((flags, transfer_fee, issuer_hex, token_seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_parse_roundtrip() {
        let issuer = "B5F762798A53D543A014CAF8B297CFF8F2F937E8";
        let id = generate_nftoken_id(0x0008, 500, issuer, 1);
        assert_eq!(id.len(), 64);

        let (flags, fee, parsed_issuer, seq) = parse_nftoken_id(&id).unwrap();
        assert_eq!(flags, 0x0008);
        assert_eq!(fee, 500);
        assert_eq!(parsed_issuer, issuer);
        assert_eq!(seq, 1);
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
