//! HookOn bitmask filtering.
//!
//! The HookOn field is a 256-bit inverted bitmask that controls which
//! transaction types trigger a hook. A bit value of 0 means the hook
//! fires for that transaction type; 1 means it does not.
//!
//! Byte ordering is big-endian: byte 31 contains bits for transaction
//! types 0-7, byte 30 for types 8-15, and so on.

/// Check whether a hook should fire for a given transaction type.
///
/// Returns `true` if the hook should execute, `false` if it should be skipped.
/// Transaction type codes > 255 always return `false`.
pub fn should_hook_fire(hook_on: &[u8; 32], tx_type: u16) -> bool {
    if tx_type > 255 {
        return false;
    }
    let byte_index = (tx_type / 8) as usize;
    let bit_index = tx_type % 8;
    // Big-endian: byte 31 holds the lowest tx types (0-7)
    let actual_byte = 31 - byte_index;
    (hook_on[actual_byte] >> bit_index) & 1 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zeros_fires_for_all() {
        let hook_on = [0u8; 32];
        // 0 = Payment, 7 = OfferCreate, 20 = TrustSet, 100 = EnableAmendment
        for tx_type in [0, 7, 20, 100] {
            assert!(
                should_hook_fire(&hook_on, tx_type),
                "should fire for type {tx_type}"
            );
        }
    }

    #[test]
    fn all_ones_fires_for_none() {
        let hook_on = [0xFF; 32];
        for tx_type in [0, 7, 20, 100] {
            assert!(
                !should_hook_fire(&hook_on, tx_type),
                "should not fire for type {tx_type}"
            );
        }
    }

    #[test]
    fn block_payment_only() {
        let mut hook_on = [0u8; 32];
        // Set bit 0 of byte 31 (Payment = type 0) to 1 (blocked)
        hook_on[31] |= 1;

        assert!(!should_hook_fire(&hook_on, 0), "Payment should be blocked");
        assert!(
            should_hook_fire(&hook_on, 7),
            "OfferCreate should still fire"
        );
        assert!(should_hook_fire(&hook_on, 20), "TrustSet should still fire");
    }

    #[test]
    fn block_offer_create_only() {
        let mut hook_on = [0u8; 32];
        // OfferCreate = type 7, bit 7 of byte 31
        hook_on[31] |= 1 << 7;

        assert!(should_hook_fire(&hook_on, 0), "Payment should fire");
        assert!(
            !should_hook_fire(&hook_on, 7),
            "OfferCreate should be blocked"
        );
    }

    #[test]
    fn block_set_hook() {
        let mut hook_on = [0u8; 32];
        // SetHook = type 22, byte_index = 22/8 = 2, bit = 22%8 = 6
        // actual_byte = 31 - 2 = 29
        hook_on[29] |= 1 << 6;

        assert!(should_hook_fire(&hook_on, 0), "Payment should fire");
        assert!(!should_hook_fire(&hook_on, 22), "SetHook should be blocked");
    }

    #[test]
    fn tx_type_over_255_never_fires() {
        let hook_on = [0u8; 32]; // all zeros = fire for everything
        assert!(!should_hook_fire(&hook_on, 256));
        assert!(!should_hook_fire(&hook_on, 1000));
    }
}
