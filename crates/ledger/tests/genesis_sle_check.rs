#[test]
fn dump_genesis_sle() {
    let account = serde_json::json!({
        "LedgerEntryType": "AccountRoot",
        "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        "Balance": "100000000000000000",
        "Sequence": 1,
        "OwnerCount": 0,
        "Flags": 0,
        "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
        "PreviousTxnLgrSeq": 0,
    });
    let json_bytes = serde_json::to_vec(&account).unwrap();
    let binary = rxrpl_ledger::sle_codec::encode_sle(&json_bytes).unwrap();
    eprintln!("encoded len: {}", binary.len());
    eprintln!("first byte 0x{:02X} {}", binary[0], if binary[0] == b'{' { "(JSON FALLBACK!)" } else { "(binary)" });
    eprintln!("hex: {}", binary.iter().map(|b| format!("{:02X}", b)).collect::<String>());
}
