#![no_main]
use libfuzzer_sys::fuzz_target;

use rxrpl_ledger::header::LedgerHeader;

fuzz_target!(|data: &[u8]| {
    // Fuzz ledger header deserialization from raw bytes
    if let Some(header) = LedgerHeader::from_raw_bytes(data) {
        // Verify hash determinism: compute_hash should always match
        let hash1 = header.compute_hash();
        let hash2 = header.compute_hash();
        assert_eq!(hash1, hash2, "hash computation must be deterministic");
        assert_eq!(header.hash, hash1, "stored hash must match computed hash");

        // Exercise accessor methods
        let _ = header.close_time_agreed();
        let _ = header.sequence;
        let _ = header.drops;
    }

    // Also try with truncated/padded inputs to exercise bounds checks
    if data.len() > 0 {
        let _ = LedgerHeader::from_raw_bytes(&data[..data.len().min(117)]);
        let _ = LedgerHeader::from_raw_bytes(&data[..data.len().min(118)]);

        let mut padded = data.to_vec();
        padded.resize(118, 0);
        let _ = LedgerHeader::from_raw_bytes(&padded);
    }
});
