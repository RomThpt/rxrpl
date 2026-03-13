use crate::error::ProtocolError;

/// All XRPL transaction types with their numeric type codes.
///
/// Codes sourced from rippled SField.h and xrpl.js definitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum TransactionType {
    Payment = 0,
    EscrowCreate = 1,
    EscrowFinish = 2,
    AccountSet = 3,
    EscrowCancel = 4,
    SetRegularKey = 5,
    NickNameSet = 6,
    OfferCreate = 7,
    OfferCancel = 8,
    SignerListSet = 12,
    PaymentChannelCreate = 13,
    PaymentChannelFund = 14,
    PaymentChannelClaim = 15,
    CheckCreate = 16,
    CheckCash = 17,
    CheckCancel = 18,
    DepositPreauth = 19,
    TrustSet = 20,
    AccountDelete = 21,
    SetHook = 22,
    NFTokenMint = 25,
    NFTokenBurn = 26,
    NFTokenCreateOffer = 27,
    NFTokenCancelOffer = 28,
    NFTokenAcceptOffer = 29,
    Clawback = 30,
    AMMCreate = 35,
    AMMDeposit = 36,
    AMMWithdraw = 37,
    AMMVote = 38,
    AMMBid = 39,
    AMMDelete = 40,
    XChainCreateClaimId = 41,
    XChainCommit = 42,
    XChainClaim = 43,
    XChainAccountCreateCommit = 44,
    XChainAddClaimAttestation = 45,
    XChainAddAccountCreateAttestation = 46,
    XChainModifyBridge = 47,
    XChainCreateBridge = 48,
    DIDSet = 49,
    DIDDelete = 50,
    OracleSet = 51,
    OracleDelete = 52,
    LedgerStateFix = 53,
    MPTokenAuthorize = 54,
    MPTokenIssuanceCreate = 55,
    MPTokenIssuanceDestroy = 56,
    MPTokenIssuanceSet = 57,
    CredentialCreate = 58,
    CredentialAccept = 59,
    CredentialDelete = 60,
    PermissionedDomainSet = 61,
    PermissionedDomainDelete = 62,
    BatchSubmit = 63,
    EnableAmendment = 100,
    SetFee = 101,
    UNLModify = 102,
    TicketCreate = 10,
}

impl TransactionType {
    /// Create from the numeric u16 code.
    pub fn from_code(code: u16) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(Self::Payment),
            1 => Ok(Self::EscrowCreate),
            2 => Ok(Self::EscrowFinish),
            3 => Ok(Self::AccountSet),
            4 => Ok(Self::EscrowCancel),
            5 => Ok(Self::SetRegularKey),
            6 => Ok(Self::NickNameSet),
            7 => Ok(Self::OfferCreate),
            8 => Ok(Self::OfferCancel),
            10 => Ok(Self::TicketCreate),
            12 => Ok(Self::SignerListSet),
            13 => Ok(Self::PaymentChannelCreate),
            14 => Ok(Self::PaymentChannelFund),
            15 => Ok(Self::PaymentChannelClaim),
            16 => Ok(Self::CheckCreate),
            17 => Ok(Self::CheckCash),
            18 => Ok(Self::CheckCancel),
            19 => Ok(Self::DepositPreauth),
            20 => Ok(Self::TrustSet),
            21 => Ok(Self::AccountDelete),
            22 => Ok(Self::SetHook),
            25 => Ok(Self::NFTokenMint),
            26 => Ok(Self::NFTokenBurn),
            27 => Ok(Self::NFTokenCreateOffer),
            28 => Ok(Self::NFTokenCancelOffer),
            29 => Ok(Self::NFTokenAcceptOffer),
            30 => Ok(Self::Clawback),
            35 => Ok(Self::AMMCreate),
            36 => Ok(Self::AMMDeposit),
            37 => Ok(Self::AMMWithdraw),
            38 => Ok(Self::AMMVote),
            39 => Ok(Self::AMMBid),
            40 => Ok(Self::AMMDelete),
            41 => Ok(Self::XChainCreateClaimId),
            42 => Ok(Self::XChainCommit),
            43 => Ok(Self::XChainClaim),
            44 => Ok(Self::XChainAccountCreateCommit),
            45 => Ok(Self::XChainAddClaimAttestation),
            46 => Ok(Self::XChainAddAccountCreateAttestation),
            47 => Ok(Self::XChainModifyBridge),
            48 => Ok(Self::XChainCreateBridge),
            49 => Ok(Self::DIDSet),
            50 => Ok(Self::DIDDelete),
            51 => Ok(Self::OracleSet),
            52 => Ok(Self::OracleDelete),
            53 => Ok(Self::LedgerStateFix),
            54 => Ok(Self::MPTokenAuthorize),
            55 => Ok(Self::MPTokenIssuanceCreate),
            56 => Ok(Self::MPTokenIssuanceDestroy),
            57 => Ok(Self::MPTokenIssuanceSet),
            58 => Ok(Self::CredentialCreate),
            59 => Ok(Self::CredentialAccept),
            60 => Ok(Self::CredentialDelete),
            61 => Ok(Self::PermissionedDomainSet),
            62 => Ok(Self::PermissionedDomainDelete),
            63 => Ok(Self::BatchSubmit),
            100 => Ok(Self::EnableAmendment),
            101 => Ok(Self::SetFee),
            102 => Ok(Self::UNLModify),
            _ => Err(ProtocolError::UnknownTransactionType(code)),
        }
    }

    /// Return the canonical string name (matches rippled JSON `TransactionType` field).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Payment => "Payment",
            Self::EscrowCreate => "EscrowCreate",
            Self::EscrowFinish => "EscrowFinish",
            Self::AccountSet => "AccountSet",
            Self::EscrowCancel => "EscrowCancel",
            Self::SetRegularKey => "SetRegularKey",
            Self::NickNameSet => "NickNameSet",
            Self::OfferCreate => "OfferCreate",
            Self::OfferCancel => "OfferCancel",
            Self::TicketCreate => "TicketCreate",
            Self::SignerListSet => "SignerListSet",
            Self::PaymentChannelCreate => "PaymentChannelCreate",
            Self::PaymentChannelFund => "PaymentChannelFund",
            Self::PaymentChannelClaim => "PaymentChannelClaim",
            Self::CheckCreate => "CheckCreate",
            Self::CheckCash => "CheckCash",
            Self::CheckCancel => "CheckCancel",
            Self::DepositPreauth => "DepositPreauth",
            Self::TrustSet => "TrustSet",
            Self::AccountDelete => "AccountDelete",
            Self::SetHook => "SetHook",
            Self::NFTokenMint => "NFTokenMint",
            Self::NFTokenBurn => "NFTokenBurn",
            Self::NFTokenCreateOffer => "NFTokenCreateOffer",
            Self::NFTokenCancelOffer => "NFTokenCancelOffer",
            Self::NFTokenAcceptOffer => "NFTokenAcceptOffer",
            Self::Clawback => "Clawback",
            Self::AMMCreate => "AMMCreate",
            Self::AMMDeposit => "AMMDeposit",
            Self::AMMWithdraw => "AMMWithdraw",
            Self::AMMVote => "AMMVote",
            Self::AMMBid => "AMMBid",
            Self::AMMDelete => "AMMDelete",
            Self::XChainCreateClaimId => "XChainCreateClaimId",
            Self::XChainCommit => "XChainCommit",
            Self::XChainClaim => "XChainClaim",
            Self::XChainAccountCreateCommit => "XChainAccountCreateCommit",
            Self::XChainAddClaimAttestation => "XChainAddClaimAttestation",
            Self::XChainAddAccountCreateAttestation => "XChainAddAccountCreateAttestation",
            Self::XChainModifyBridge => "XChainModifyBridge",
            Self::XChainCreateBridge => "XChainCreateBridge",
            Self::DIDSet => "DIDSet",
            Self::DIDDelete => "DIDDelete",
            Self::OracleSet => "OracleSet",
            Self::OracleDelete => "OracleDelete",
            Self::LedgerStateFix => "LedgerStateFix",
            Self::MPTokenAuthorize => "MPTokenAuthorize",
            Self::MPTokenIssuanceCreate => "MPTokenIssuanceCreate",
            Self::MPTokenIssuanceDestroy => "MPTokenIssuanceDestroy",
            Self::MPTokenIssuanceSet => "MPTokenIssuanceSet",
            Self::CredentialCreate => "CredentialCreate",
            Self::CredentialAccept => "CredentialAccept",
            Self::CredentialDelete => "CredentialDelete",
            Self::PermissionedDomainSet => "PermissionedDomainSet",
            Self::PermissionedDomainDelete => "PermissionedDomainDelete",
            Self::BatchSubmit => "BatchSubmit",
            Self::EnableAmendment => "EnableAmendment",
            Self::SetFee => "SetFee",
            Self::UNLModify => "UNLModify",
        }
    }

    /// Parse from the canonical string name.
    pub fn from_name(name: &str) -> Result<Self, ProtocolError> {
        match name {
            "Payment" => Ok(Self::Payment),
            "EscrowCreate" => Ok(Self::EscrowCreate),
            "EscrowFinish" => Ok(Self::EscrowFinish),
            "AccountSet" => Ok(Self::AccountSet),
            "EscrowCancel" => Ok(Self::EscrowCancel),
            "SetRegularKey" => Ok(Self::SetRegularKey),
            "NickNameSet" => Ok(Self::NickNameSet),
            "OfferCreate" => Ok(Self::OfferCreate),
            "OfferCancel" => Ok(Self::OfferCancel),
            "TicketCreate" => Ok(Self::TicketCreate),
            "SignerListSet" => Ok(Self::SignerListSet),
            "PaymentChannelCreate" => Ok(Self::PaymentChannelCreate),
            "PaymentChannelFund" => Ok(Self::PaymentChannelFund),
            "PaymentChannelClaim" => Ok(Self::PaymentChannelClaim),
            "CheckCreate" => Ok(Self::CheckCreate),
            "CheckCash" => Ok(Self::CheckCash),
            "CheckCancel" => Ok(Self::CheckCancel),
            "DepositPreauth" => Ok(Self::DepositPreauth),
            "TrustSet" => Ok(Self::TrustSet),
            "AccountDelete" => Ok(Self::AccountDelete),
            "SetHook" => Ok(Self::SetHook),
            "NFTokenMint" => Ok(Self::NFTokenMint),
            "NFTokenBurn" => Ok(Self::NFTokenBurn),
            "NFTokenCreateOffer" => Ok(Self::NFTokenCreateOffer),
            "NFTokenCancelOffer" => Ok(Self::NFTokenCancelOffer),
            "NFTokenAcceptOffer" => Ok(Self::NFTokenAcceptOffer),
            "Clawback" => Ok(Self::Clawback),
            "AMMCreate" => Ok(Self::AMMCreate),
            "AMMDeposit" => Ok(Self::AMMDeposit),
            "AMMWithdraw" => Ok(Self::AMMWithdraw),
            "AMMVote" => Ok(Self::AMMVote),
            "AMMBid" => Ok(Self::AMMBid),
            "AMMDelete" => Ok(Self::AMMDelete),
            "XChainCreateClaimId" => Ok(Self::XChainCreateClaimId),
            "XChainCommit" => Ok(Self::XChainCommit),
            "XChainClaim" => Ok(Self::XChainClaim),
            "XChainAccountCreateCommit" => Ok(Self::XChainAccountCreateCommit),
            "XChainAddClaimAttestation" => Ok(Self::XChainAddClaimAttestation),
            "XChainAddAccountCreateAttestation" => Ok(Self::XChainAddAccountCreateAttestation),
            "XChainModifyBridge" => Ok(Self::XChainModifyBridge),
            "XChainCreateBridge" => Ok(Self::XChainCreateBridge),
            "DIDSet" => Ok(Self::DIDSet),
            "DIDDelete" => Ok(Self::DIDDelete),
            "OracleSet" => Ok(Self::OracleSet),
            "OracleDelete" => Ok(Self::OracleDelete),
            "LedgerStateFix" => Ok(Self::LedgerStateFix),
            "MPTokenAuthorize" => Ok(Self::MPTokenAuthorize),
            "MPTokenIssuanceCreate" => Ok(Self::MPTokenIssuanceCreate),
            "MPTokenIssuanceDestroy" => Ok(Self::MPTokenIssuanceDestroy),
            "MPTokenIssuanceSet" => Ok(Self::MPTokenIssuanceSet),
            "CredentialCreate" => Ok(Self::CredentialCreate),
            "CredentialAccept" => Ok(Self::CredentialAccept),
            "CredentialDelete" => Ok(Self::CredentialDelete),
            "PermissionedDomainSet" => Ok(Self::PermissionedDomainSet),
            "PermissionedDomainDelete" => Ok(Self::PermissionedDomainDelete),
            "BatchSubmit" => Ok(Self::BatchSubmit),
            "EnableAmendment" => Ok(Self::EnableAmendment),
            "SetFee" => Ok(Self::SetFee),
            "UNLModify" => Ok(Self::UNLModify),
            _ => Err(ProtocolError::UnknownTransactionTypeName(name.to_string())),
        }
    }

    /// Return the u16 type code.
    pub fn code(&self) -> u16 {
        *self as u16
    }
}

impl std::fmt::Display for TransactionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for TransactionType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for TransactionType {
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
    fn from_code_roundtrip() {
        let tt = TransactionType::Payment;
        let code = tt.code();
        assert_eq!(code, 0);
        assert_eq!(TransactionType::from_code(code).unwrap(), tt);
    }

    #[test]
    fn from_name_roundtrip() {
        let tt = TransactionType::TrustSet;
        let name = tt.as_str();
        assert_eq!(name, "TrustSet");
        assert_eq!(TransactionType::from_name(name).unwrap(), tt);
    }

    #[test]
    fn all_variants_code_roundtrip() {
        let variants = [
            TransactionType::Payment,
            TransactionType::EscrowCreate,
            TransactionType::EscrowFinish,
            TransactionType::AccountSet,
            TransactionType::EscrowCancel,
            TransactionType::SetRegularKey,
            TransactionType::OfferCreate,
            TransactionType::OfferCancel,
            TransactionType::TicketCreate,
            TransactionType::SignerListSet,
            TransactionType::PaymentChannelCreate,
            TransactionType::PaymentChannelFund,
            TransactionType::PaymentChannelClaim,
            TransactionType::CheckCreate,
            TransactionType::CheckCash,
            TransactionType::CheckCancel,
            TransactionType::DepositPreauth,
            TransactionType::TrustSet,
            TransactionType::AccountDelete,
            TransactionType::NFTokenMint,
            TransactionType::NFTokenBurn,
            TransactionType::NFTokenCreateOffer,
            TransactionType::NFTokenCancelOffer,
            TransactionType::NFTokenAcceptOffer,
            TransactionType::EnableAmendment,
            TransactionType::SetFee,
        ];
        for v in variants {
            let code = v.code();
            let name = v.as_str();
            assert_eq!(TransactionType::from_code(code).unwrap(), v);
            assert_eq!(TransactionType::from_name(name).unwrap(), v);
        }
    }

    #[test]
    fn unknown_code() {
        assert!(TransactionType::from_code(9999).is_err());
    }

    #[test]
    fn unknown_name() {
        assert!(TransactionType::from_name("Bogus").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let tt = TransactionType::Payment;
        let json = serde_json::to_string(&tt).unwrap();
        assert_eq!(json, "\"Payment\"");
        let decoded: TransactionType = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, tt);
    }

    #[test]
    fn display() {
        assert_eq!(TransactionType::OfferCreate.to_string(), "OfferCreate");
    }
}
