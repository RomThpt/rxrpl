use crate::error::ProtocolError;

/// All XRPL ledger entry (object) types with their numeric type codes.
///
/// Codes sourced from rippled LedgerFormats.h.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum LedgerEntryType {
    AccountRoot = 0x0061,                     // 'a'
    DirectoryNode = 0x0064,                   // 'd'
    RippleState = 0x0072,                     // 'r'
    Ticket = 0x0054,                          // 'T'
    SignerList = 0x0053,                      // 'S'
    Offer = 0x006F,                           // 'o'
    LedgerHashes = 0x0068,                    // 'h'
    Amendments = 0x0066,                      // 'f'
    FeeSettings = 0x0073,                     // 's'
    Escrow = 0x0075,                          // 'u'
    PayChannel = 0x0078,                      // 'x'
    Check = 0x0043,                           // 'C'
    DepositPreauth = 0x0070,                  // 'p'
    NegativeUNL = 0x004E,                     // 'N'
    NFTokenPage = 0x0050,                     // 'P'
    NFTokenOffer = 0x0037,                    // '7'
    AMM = 0x0079,                             // 'y'
    Bridge = 0x0069,                          // 'i'
    XChainOwnedClaimId = 0x0071,              // 'q'
    XChainOwnedCreateAccountClaimId = 0x0074, // 't'
    DID = 0x0049,                             // 'I'
    Oracle = 0x0080,
    MPTokenIssuance = 0x007E,
    MPToken = 0x007F,
    Credential = 0x0081,
    PermissionedDomain = 0x0082,
    Delegate = 0x0083,
    Vault = 0x0084,
    HookDefinition = 0x0085,
    HookState = 0x0086,
    LoanBroker = 0x0088, // 136
    Loan = 0x0089,       // 137
}

impl LedgerEntryType {
    /// Create from the numeric u16 code.
    pub fn from_code(code: u16) -> Result<Self, ProtocolError> {
        match code {
            0x0061 => Ok(Self::AccountRoot),
            0x0064 => Ok(Self::DirectoryNode),
            0x0072 => Ok(Self::RippleState),
            0x0054 => Ok(Self::Ticket),
            0x0053 => Ok(Self::SignerList),
            0x006F => Ok(Self::Offer),
            0x0068 => Ok(Self::LedgerHashes),
            0x0066 => Ok(Self::Amendments),
            0x0073 => Ok(Self::FeeSettings),
            0x0075 => Ok(Self::Escrow),
            0x0078 => Ok(Self::PayChannel),
            0x0043 => Ok(Self::Check),
            0x0070 => Ok(Self::DepositPreauth),
            0x004E => Ok(Self::NegativeUNL),
            0x0050 => Ok(Self::NFTokenPage),
            0x0037 => Ok(Self::NFTokenOffer),
            0x0079 => Ok(Self::AMM),
            0x0069 => Ok(Self::Bridge),
            0x0071 => Ok(Self::XChainOwnedClaimId),
            0x0074 => Ok(Self::XChainOwnedCreateAccountClaimId),
            0x0049 => Ok(Self::DID),
            0x0080 => Ok(Self::Oracle),
            0x007E => Ok(Self::MPTokenIssuance),
            0x007F => Ok(Self::MPToken),
            0x0081 => Ok(Self::Credential),
            0x0082 => Ok(Self::PermissionedDomain),
            0x0083 => Ok(Self::Delegate),
            0x0084 => Ok(Self::Vault),
            0x0085 => Ok(Self::HookDefinition),
            0x0086 => Ok(Self::HookState),
            0x0088 => Ok(Self::LoanBroker),
            0x0089 => Ok(Self::Loan),
            _ => Err(ProtocolError::UnknownLedgerEntryType(code)),
        }
    }

    /// Return the canonical string name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AccountRoot => "AccountRoot",
            Self::DirectoryNode => "DirectoryNode",
            Self::RippleState => "RippleState",
            Self::Ticket => "Ticket",
            Self::SignerList => "SignerList",
            Self::Offer => "Offer",
            Self::LedgerHashes => "LedgerHashes",
            Self::Amendments => "Amendments",
            Self::FeeSettings => "FeeSettings",
            Self::Escrow => "Escrow",
            Self::PayChannel => "PayChannel",
            Self::Check => "Check",
            Self::DepositPreauth => "DepositPreauth",
            Self::NegativeUNL => "NegativeUNL",
            Self::NFTokenPage => "NFTokenPage",
            Self::NFTokenOffer => "NFTokenOffer",
            Self::AMM => "AMM",
            Self::Bridge => "Bridge",
            Self::XChainOwnedClaimId => "XChainOwnedClaimId",
            Self::XChainOwnedCreateAccountClaimId => "XChainOwnedCreateAccountClaimId",
            Self::DID => "DID",
            Self::Oracle => "Oracle",
            Self::MPTokenIssuance => "MPTokenIssuance",
            Self::MPToken => "MPToken",
            Self::Credential => "Credential",
            Self::PermissionedDomain => "PermissionedDomain",
            Self::Delegate => "Delegate",
            Self::Vault => "Vault",
            Self::HookDefinition => "HookDefinition",
            Self::HookState => "HookState",
            Self::LoanBroker => "LoanBroker",
            Self::Loan => "Loan",
        }
    }

    /// Parse from the canonical string name.
    pub fn from_name(name: &str) -> Result<Self, ProtocolError> {
        match name {
            "AccountRoot" => Ok(Self::AccountRoot),
            "DirectoryNode" => Ok(Self::DirectoryNode),
            "RippleState" => Ok(Self::RippleState),
            "Ticket" => Ok(Self::Ticket),
            "SignerList" => Ok(Self::SignerList),
            "Offer" => Ok(Self::Offer),
            "LedgerHashes" => Ok(Self::LedgerHashes),
            "Amendments" => Ok(Self::Amendments),
            "FeeSettings" => Ok(Self::FeeSettings),
            "Escrow" => Ok(Self::Escrow),
            "PayChannel" => Ok(Self::PayChannel),
            "Check" => Ok(Self::Check),
            "DepositPreauth" => Ok(Self::DepositPreauth),
            "NegativeUNL" => Ok(Self::NegativeUNL),
            "NFTokenPage" => Ok(Self::NFTokenPage),
            "NFTokenOffer" => Ok(Self::NFTokenOffer),
            "AMM" => Ok(Self::AMM),
            "Bridge" => Ok(Self::Bridge),
            "XChainOwnedClaimId" => Ok(Self::XChainOwnedClaimId),
            "XChainOwnedCreateAccountClaimId" => Ok(Self::XChainOwnedCreateAccountClaimId),
            "DID" => Ok(Self::DID),
            "Oracle" => Ok(Self::Oracle),
            "MPTokenIssuance" => Ok(Self::MPTokenIssuance),
            "MPToken" => Ok(Self::MPToken),
            "Credential" => Ok(Self::Credential),
            "PermissionedDomain" => Ok(Self::PermissionedDomain),
            "Delegate" => Ok(Self::Delegate),
            "Vault" => Ok(Self::Vault),
            "HookDefinition" => Ok(Self::HookDefinition),
            "HookState" => Ok(Self::HookState),
            "LoanBroker" => Ok(Self::LoanBroker),
            "Loan" => Ok(Self::Loan),
            _ => Err(ProtocolError::UnknownLedgerEntryTypeName(name.to_string())),
        }
    }

    /// Return the u16 type code.
    pub fn code(&self) -> u16 {
        *self as u16
    }
}

impl std::fmt::Display for LedgerEntryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for LedgerEntryType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for LedgerEntryType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_name(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_root_code() {
        assert_eq!(LedgerEntryType::AccountRoot.code(), 0x0061);
    }

    #[test]
    fn from_code_roundtrip() {
        let variants = [
            LedgerEntryType::AccountRoot,
            LedgerEntryType::DirectoryNode,
            LedgerEntryType::RippleState,
            LedgerEntryType::Offer,
            LedgerEntryType::SignerList,
            LedgerEntryType::Escrow,
            LedgerEntryType::PayChannel,
            LedgerEntryType::Check,
            LedgerEntryType::NFTokenPage,
            LedgerEntryType::NFTokenOffer,
        ];
        for v in variants {
            let code = v.code();
            assert_eq!(LedgerEntryType::from_code(code).unwrap(), v);
        }
    }

    #[test]
    fn from_name_roundtrip() {
        let le = LedgerEntryType::RippleState;
        assert_eq!(le.as_str(), "RippleState");
        assert_eq!(LedgerEntryType::from_name("RippleState").unwrap(), le);
    }

    #[test]
    fn unknown_code() {
        assert!(LedgerEntryType::from_code(0xFFFF).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let le = LedgerEntryType::Offer;
        let json = serde_json::to_string(&le).unwrap();
        assert_eq!(json, "\"Offer\"");
        let decoded: LedgerEntryType = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, le);
    }
}
