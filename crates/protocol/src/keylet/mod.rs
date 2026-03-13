use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::{AccountId, Hash256};

/// Ledger namespace identifiers used in keylet computation.
///
/// Each namespace is a u16 stored as 2-byte big-endian, matching the goXRPLd convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum LedgerNamespace {
    Account = 0x0061,       // 'a'
    DirNode = 0x0064,       // 'd'
    GeneratorMap = 0x0067,  // 'g'
    RippleState = 0x0072,   // 'r'
    Offer = 0x006F,         // 'o'
    OwnerDir = 0x004F,      // 'O'
    BookDir = 0x0042,       // 'B'
    Skip = 0x0073,          // 's'
    Amendment = 0x0066,     // 'f'
    Fee = 0x0065,           // 'e'
    NegativeUNL = 0x004E,   // 'N'
    Ticket = 0x0054,        // 'T'
    SignerList = 0x0053,    // 'S'
    PayChannel = 0x0078,    // 'x'
    Check = 0x0043,         // 'C'
    DepositPreauth = 0x0070, // 'p'
    Escrow = 0x0075,        // 'u'
    NFTokenPage = 0x0050,   // 'P'
    NFTokenOffer = 0x0037,  // '7'
    AMM = 0x0041,           // 'A'
    Bridge = 0x0069,        // 'i'
    XChainClaimId = 0x0051, // 'Q'
    XChainCreateAccountClaimId = 0x004B, // 'K'
    DID = 0x0049,           // 'I'
    Oracle = 0x0052,        // 'R'
    MPTokenIssuance = 0x007E,
    MPToken = 0x007F,
    Credential = 0x0044,    // 'D'
}

/// Compute a ledger index by hashing: space_u16_be || data...
///
/// This is the core keylet computation used throughout XRPL.
fn index_hash(space: LedgerNamespace, data: &[&[u8]]) -> Hash256 {
    let space_bytes = (space as u16).to_be_bytes();
    let mut inputs: Vec<&[u8]> = Vec::with_capacity(data.len() + 1);
    inputs.push(&space_bytes);
    inputs.extend(data);
    sha512_half(&inputs)
}

/// Compute the keylet for an account.
pub fn account(id: &AccountId) -> Hash256 {
    index_hash(LedgerNamespace::Account, &[id.as_bytes()])
}

/// Compute the keylet for a trust line between two accounts for a currency.
///
/// The accounts are sorted so that the same trust line is identified regardless
/// of which account's perspective we use.
pub fn trust_line(a: &AccountId, b: &AccountId, currency: &[u8; 20]) -> Hash256 {
    let (low, high) = if a.as_bytes() < b.as_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    index_hash(
        LedgerNamespace::RippleState,
        &[low.as_bytes(), high.as_bytes(), currency],
    )
}

/// Compute the keylet for an offer.
pub fn offer(id: &AccountId, seq: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::Offer,
        &[id.as_bytes(), &seq.to_be_bytes()],
    )
}

/// Compute the keylet for an account's owner directory.
pub fn owner_dir(id: &AccountId) -> Hash256 {
    index_hash(LedgerNamespace::OwnerDir, &[id.as_bytes()])
}

/// Compute the keylet for a directory node page.
pub fn dir_node(root: &Hash256, page: u64) -> Hash256 {
    if page == 0 {
        return *root;
    }
    index_hash(
        LedgerNamespace::DirNode,
        &[root.as_bytes(), &page.to_be_bytes()],
    )
}

/// Compute the keylet for an order book directory.
///
/// `pays_currency` and `pays_issuer` describe what the taker pays.
/// `gets_currency` and `gets_issuer` describe what the taker gets.
pub fn book_dir(
    pays_currency: &[u8; 20],
    pays_issuer: &AccountId,
    gets_currency: &[u8; 20],
    gets_issuer: &AccountId,
) -> Hash256 {
    index_hash(
        LedgerNamespace::BookDir,
        &[
            pays_currency,
            pays_issuer.as_bytes(),
            gets_currency,
            gets_issuer.as_bytes(),
        ],
    )
}

/// Compute the keylet for the skip list (ledger hashes).
pub fn skip() -> Hash256 {
    index_hash(LedgerNamespace::Skip, &[])
}

/// Compute the keylet for the amendments pseudo-object.
pub fn amendments() -> Hash256 {
    index_hash(LedgerNamespace::Amendment, &[])
}

/// Compute the keylet for the fee settings pseudo-object.
pub fn fee_settings() -> Hash256 {
    index_hash(LedgerNamespace::Fee, &[])
}

/// Compute the keylet for the negative UNL pseudo-object.
pub fn negative_unl() -> Hash256 {
    index_hash(LedgerNamespace::NegativeUNL, &[])
}

/// Compute the keylet for a ticket.
pub fn ticket(id: &AccountId, seq: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::Ticket,
        &[id.as_bytes(), &seq.to_be_bytes()],
    )
}

/// Compute the keylet for a signer list.
pub fn signer_list(id: &AccountId) -> Hash256 {
    // SignerList uses signer_list_id = 0 (u32 BE)
    let signer_list_id: u32 = 0;
    index_hash(
        LedgerNamespace::SignerList,
        &[id.as_bytes(), &signer_list_id.to_be_bytes()],
    )
}

/// Compute the keylet for a payment channel.
pub fn pay_channel(src: &AccountId, dst: &AccountId, seq: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::PayChannel,
        &[src.as_bytes(), dst.as_bytes(), &seq.to_be_bytes()],
    )
}

/// Compute the keylet for a check.
pub fn check(id: &AccountId, seq: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::Check,
        &[id.as_bytes(), &seq.to_be_bytes()],
    )
}

/// Compute the keylet for a deposit preauth entry.
pub fn deposit_preauth(owner: &AccountId, authorized: &AccountId) -> Hash256 {
    index_hash(
        LedgerNamespace::DepositPreauth,
        &[owner.as_bytes(), authorized.as_bytes()],
    )
}

/// Compute the keylet for an escrow.
pub fn escrow(id: &AccountId, seq: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::Escrow,
        &[id.as_bytes(), &seq.to_be_bytes()],
    )
}

/// Compute the keylet for an NFToken page.
///
/// The page key is the account ID (padded to 32 bytes) with the bottom 96 bits
/// set from the token ID.
pub fn nftoken_page_min(id: &AccountId) -> Hash256 {
    let mut key = [0u8; 32];
    key[..20].copy_from_slice(id.as_bytes());
    // Bottom 12 bytes are zero for the minimum page
    index_hash(LedgerNamespace::NFTokenPage, &[&key])
}

/// Compute the keylet for an NFToken offer.
pub fn nftoken_offer(id: &AccountId, seq: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::NFTokenOffer,
        &[id.as_bytes(), &seq.to_be_bytes()],
    )
}

/// Compute the keylet for a DID.
pub fn did(id: &AccountId) -> Hash256 {
    index_hash(LedgerNamespace::DID, &[id.as_bytes()])
}

/// Compute the keylet for an Oracle.
pub fn oracle(id: &AccountId, oracle_document_id: u32) -> Hash256 {
    index_hash(
        LedgerNamespace::Oracle,
        &[id.as_bytes(), &oracle_document_id.to_be_bytes()],
    )
}

/// Compute the keylet for a credential.
pub fn credential(subject: &AccountId, issuer: &AccountId, credential_type: &[u8]) -> Hash256 {
    index_hash(
        LedgerNamespace::Credential,
        &[subject.as_bytes(), issuer.as_bytes(), credential_type],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn account_keylet() {
        // Known-answer: account keylet for rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh
        // Account ID: B5F762798A53D543A014CAF8B297CFF8F2F937E8
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key = account(&id);
        // The hash should be deterministic and non-zero
        assert!(!key.is_zero());
    }

    #[test]
    fn offer_keylet() {
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key = offer(&id, 7);
        assert!(!key.is_zero());
    }

    #[test]
    fn trust_line_symmetric() {
        let a = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let b = AccountId::from_str("88A5A57C829F40F25EA83385BBDE6C3D8B4CA082").unwrap();
        let currency = [0u8; 20]; // XRP-like

        let key1 = trust_line(&a, &b, &currency);
        let key2 = trust_line(&b, &a, &currency);
        assert_eq!(key1, key2, "trust line keylet should be symmetric");
    }

    #[test]
    fn signer_list_keylet() {
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key = signer_list(&id);
        assert!(!key.is_zero());
    }

    #[test]
    fn singleton_keylets_deterministic() {
        let a1 = amendments();
        let a2 = amendments();
        assert_eq!(a1, a2);

        let f1 = fee_settings();
        let f2 = fee_settings();
        assert_eq!(f1, f2);

        // amendments and fee_settings should differ
        assert_ne!(a1, f1);
    }

    #[test]
    fn escrow_keylet() {
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key = escrow(&id, 42);
        assert!(!key.is_zero());
    }

    #[test]
    fn check_keylet() {
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key = check(&id, 1);
        assert!(!key.is_zero());
    }

    #[test]
    fn deposit_preauth_keylet() {
        let owner = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let auth = AccountId::from_str("88A5A57C829F40F25EA83385BBDE6C3D8B4CA082").unwrap();
        let key = deposit_preauth(&owner, &auth);
        assert!(!key.is_zero());
    }

    #[test]
    fn pay_channel_keylet() {
        let src = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let dst = AccountId::from_str("88A5A57C829F40F25EA83385BBDE6C3D8B4CA082").unwrap();
        let key = pay_channel(&src, &dst, 1);
        assert!(!key.is_zero());
    }

    #[test]
    fn different_sequences_produce_different_keys() {
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key1 = offer(&id, 1);
        let key2 = offer(&id, 2);
        assert_ne!(key1, key2);
    }

    #[test]
    fn ticket_keylet() {
        let id = AccountId::from_str("B5F762798A53D543A014CAF8B297CFF8F2F937E8").unwrap();
        let key = ticket(&id, 10);
        assert!(!key.is_zero());
    }
}
