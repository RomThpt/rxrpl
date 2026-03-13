use crate::error::ProtocolError;

/// Category of a transaction result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResultCategory {
    /// tel: Local error, not applied, not forwarded (-399..-300)
    Tel,
    /// tem: Malformed, not applied, not forwarded (-299..-200)
    Tem,
    /// tef: Failure, not applied, forwarded (-199..-100)
    Tef,
    /// ter: Retry, not applied, forwarded (-99..-1)
    Ter,
    /// tes: Success (0)
    Tes,
    /// tec: Claimed cost, applied with failure code (100-198)
    Tec,
}

/// XRPL transaction result codes.
///
/// Codes sourced from rippled TER.h. Each variant maps to a specific i32 code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransactionResult {
    // tes: Success (0)
    TesSuccess,

    // tec: Claimed cost (100+)
    TecClaimCost,
    TecPathPartial,
    TecUnfundedAdd,
    TecUnfundedOffer,
    TecUnfundedPayment,
    TecFailedProcessing,
    TecDirFull,
    TecInsufReserveOffer,
    TecInsufReserveLine,
    TecInsufReserveSupply,
    TecNoDst,
    TecNoDstInsuf,
    TecNoLineInsuf,
    TecNoLineRedundant,
    TecPathDry,
    TecUnfunded,
    TecNoAlternativeKey,
    TecNoRegularKey,
    TecOwners,
    TecNoIssuer,
    TecNoAuth,
    TecNoLine,
    TecInsufficientReserve,
    TecNoTarget,
    TecNoPermission,
    TecNoEntry,
    TecInsufficientPayment,
    TecObjectNotFound,
    TecDuplicate,
    TecKilled,
    TecHasObligations,
    TecTooSoon,
    TecMaxSequenceReached,
    TecNoSuitableNFTokenPage,
    TecNFTokenBurnable,
    TecNFTokenNotBurnable,
    TecNFTokenOfferNotCleared,
    TecCannotRemoveGivenNode,
    TecInvariantFailed,
    TecOversize,
    TecExpired,
    TecInternalError,
    TecNeedMasterKey,
    TecDstTagNeeded,
    TecCryptoconditionError,
    TecXChainBadTransferPrice,
    TecXChainBadProof,
    TecXChainNonceNeeded,
    TecXChainBadPublicKeyAccountPair,
    TecXChainProofUnknownKey,
    TecXChainClaimNoQuorum,
    TecXChainCreateAccountNoQuorum,
    TecXChainAccountCreatePastSeq,
    TecXChainNoClaimId,
    TecXChainPaymentEmpty,
    TecXChainSelfCommit,
    TecXChainBadDest,
    TecXChainNoDst,
    TecXChainNoSignersList,
    TecXChainSendingNotEmpty,
    TecXChainInsufClaimFee,
    TecXChainBadClaimId,
    TecXChainRewardMismatch,
    TecEmptyDID,
    TecInvalidUpdateTime,
    TecTokenAlreadyOwned,
    TecMaxTokensReached,
    TecArrayEmpty,
    TecArrayTooLarge,
    TecBadCredential,
    TecPreconditionFailed,

    // tef: Failure (-199..-100)
    TefFailure,
    TefAlreadyMaster,
    TefBadAddOrDrop,
    TefBadAuth,
    TefBadAuthMaster,
    TefBadLedger,
    TefBadQuorum,
    TefBadSignature,
    TefCreated,
    TefException,
    TefInternal,
    TefMaxLedger,
    TefNoAuthRequired,
    TefNotMultiSigning,
    TefPastSeq,
    TefSeqNumPast,
    TefTooOld,
    TefWrongPrior,
    TefNotTrustLine,

    // ter: Retry (-99..-1)
    TerRetry,
    TerFundsSpent,
    TerInsufFee,
    TerNoAccount,
    TerNoAuth,
    TerNoLine,
    TerPreSeq,
    TerLastLedger,
    TerNoRipple,
    TerQueueFull,

    // tem: Malformed (-299..-200)
    TemMalformed,
    TemBadAmount,
    TemBadCurrency,
    TemBadExpiration,
    TemBadFee,
    TemBadIssuer,
    TemBadLimit,
    TemBadNFTokenTransfer,
    TemBadOffer,
    TemBadPath,
    TemBadPathLoop,
    TemBadRegKey,
    TemBadSend,
    TemBadSequence,
    TemBadSignature,
    TemBadSrc,
    TemBadTick,
    TemBadTickSize,
    TemBadTransferRate,
    TemCannotPreAuthSelf,
    TemDstIsObligatory,
    TemDstIsRequired,
    TemDstTagRequired,
    TemDstTagNotNeeded,
    TemInvalidFlag,
    TemInvalidAccountId,
    TemRedundant,
    TemRippleRedundant,
    TemDisabled,
    TemBadSigner,
    TemBadQuorum,
    TemBadWeight,
    TemSequenceTooHigh,
    TemInvalidCount,
    TemScaleOutOfRange,
    TemBadNFTokenBurnOffer,
    TemMalformedRequest,
    TemXChainBridge,
    TemXChainTooMany,

    // tel: Local error (-399..-300)
    TelLocalError,
    TelBadDomain,
    TelBadPathCount,
    TelBadPublicKey,
    TelFailedProcessing,
    TelInsufFeeP,
    TelNoAuthPeer,
    TelCanNotQueueBalance,
    TelCanNotQueueBlocked,
    TelCanNotQueueDrops,
    TelCanNotQueueFee,
    TelCanNotQueueFull,
    TelWrongNetwork,
    TelRequiresNetworkId,
    TelCanNotQueueBusy,
}

impl TransactionResult {
    /// Return the i32 result code.
    pub fn code(&self) -> i32 {
        match self {
            // tes
            Self::TesSuccess => 0,

            // tec (100+)
            Self::TecClaimCost => 100,
            Self::TecPathPartial => 101,
            Self::TecUnfundedAdd => 102,
            Self::TecUnfundedOffer => 103,
            Self::TecUnfundedPayment => 104,
            Self::TecFailedProcessing => 105,
            Self::TecDirFull => 121,
            Self::TecInsufReserveOffer => 122,
            Self::TecInsufReserveLine => 123,
            Self::TecInsufReserveSupply => 124,
            Self::TecNoDst => 125,
            Self::TecNoDstInsuf => 126,
            Self::TecNoLineInsuf => 127,
            Self::TecNoLineRedundant => 128,
            Self::TecPathDry => 129,
            Self::TecUnfunded => 130,
            Self::TecNoAlternativeKey => 131,
            Self::TecNoRegularKey => 132,
            Self::TecOwners => 133,
            Self::TecNoIssuer => 134,
            Self::TecNoAuth => 135,
            Self::TecNoLine => 136,
            Self::TecInsufficientReserve => 137,
            Self::TecNoTarget => 138,
            Self::TecNoPermission => 139,
            Self::TecNoEntry => 140,
            Self::TecInsufficientPayment => 141,
            Self::TecObjectNotFound => 142,
            Self::TecDuplicate => 143,
            Self::TecKilled => 150,
            Self::TecHasObligations => 151,
            Self::TecTooSoon => 152,
            Self::TecMaxSequenceReached => 153,
            Self::TecNoSuitableNFTokenPage => 154,
            Self::TecNFTokenBurnable => 155,
            Self::TecNFTokenNotBurnable => 156,
            Self::TecNFTokenOfferNotCleared => 157,
            Self::TecCannotRemoveGivenNode => 158,
            Self::TecInvariantFailed => 159,
            Self::TecOversize => 160,
            Self::TecExpired => 161,
            Self::TecInternalError => 162,
            Self::TecNeedMasterKey => 163,
            Self::TecDstTagNeeded => 164,
            Self::TecCryptoconditionError => 165,
            Self::TecXChainBadTransferPrice => 166,
            Self::TecXChainBadProof => 167,
            Self::TecXChainNonceNeeded => 168,
            Self::TecXChainBadPublicKeyAccountPair => 169,
            Self::TecXChainProofUnknownKey => 170,
            Self::TecXChainClaimNoQuorum => 171,
            Self::TecXChainCreateAccountNoQuorum => 172,
            Self::TecXChainAccountCreatePastSeq => 173,
            Self::TecXChainNoClaimId => 174,
            Self::TecXChainPaymentEmpty => 175,
            Self::TecXChainSelfCommit => 176,
            Self::TecXChainBadDest => 177,
            Self::TecXChainNoDst => 178,
            Self::TecXChainNoSignersList => 179,
            Self::TecXChainSendingNotEmpty => 180,
            Self::TecXChainInsufClaimFee => 181,
            Self::TecXChainBadClaimId => 182,
            Self::TecXChainRewardMismatch => 183,
            Self::TecEmptyDID => 184,
            Self::TecInvalidUpdateTime => 185,
            Self::TecTokenAlreadyOwned => 186,
            Self::TecMaxTokensReached => 187,
            Self::TecArrayEmpty => 188,
            Self::TecArrayTooLarge => 189,
            Self::TecBadCredential => 190,
            Self::TecPreconditionFailed => 191,

            // tef (-199..-100)
            Self::TefFailure => -199,
            Self::TefAlreadyMaster => -198,
            Self::TefBadAddOrDrop => -197,
            Self::TefBadAuth => -196,
            Self::TefBadAuthMaster => -195,
            Self::TefBadLedger => -194,
            Self::TefBadQuorum => -193,
            Self::TefBadSignature => -192,
            Self::TefCreated => -191,
            Self::TefException => -190,
            Self::TefInternal => -189,
            Self::TefMaxLedger => -188,
            Self::TefNoAuthRequired => -187,
            Self::TefNotMultiSigning => -186,
            Self::TefPastSeq => -185,
            Self::TefSeqNumPast => -184,
            Self::TefTooOld => -183,
            Self::TefWrongPrior => -182,
            Self::TefNotTrustLine => -181,

            // ter (-99..-1)
            Self::TerRetry => -99,
            Self::TerFundsSpent => -98,
            Self::TerInsufFee => -97,
            Self::TerNoAccount => -96,
            Self::TerNoAuth => -95,
            Self::TerNoLine => -94,
            Self::TerPreSeq => -92,
            Self::TerLastLedger => -91,
            Self::TerNoRipple => -90,
            Self::TerQueueFull => -89,

            // tem (-299..-200)
            Self::TemMalformed => -299,
            Self::TemBadAmount => -298,
            Self::TemBadCurrency => -297,
            Self::TemBadExpiration => -296,
            Self::TemBadFee => -295,
            Self::TemBadIssuer => -294,
            Self::TemBadLimit => -293,
            Self::TemBadNFTokenTransfer => -292,
            Self::TemBadOffer => -291,
            Self::TemBadPath => -290,
            Self::TemBadPathLoop => -289,
            Self::TemBadRegKey => -288,
            Self::TemBadSend => -287,
            Self::TemBadSequence => -286,
            Self::TemBadSignature => -285,
            Self::TemBadSrc => -284,
            Self::TemBadTick => -283,
            Self::TemBadTickSize => -282,
            Self::TemBadTransferRate => -281,
            Self::TemCannotPreAuthSelf => -280,
            Self::TemDstIsObligatory => -279,
            Self::TemDstIsRequired => -278,
            Self::TemDstTagRequired => -277,
            Self::TemDstTagNotNeeded => -276,
            Self::TemInvalidFlag => -275,
            Self::TemInvalidAccountId => -274,
            Self::TemRedundant => -273,
            Self::TemRippleRedundant => -272,
            Self::TemDisabled => -271,
            Self::TemBadSigner => -270,
            Self::TemBadQuorum => -269,
            Self::TemBadWeight => -268,
            Self::TemSequenceTooHigh => -267,
            Self::TemInvalidCount => -266,
            Self::TemScaleOutOfRange => -265,
            Self::TemBadNFTokenBurnOffer => -264,
            Self::TemMalformedRequest => -263,
            Self::TemXChainBridge => -262,
            Self::TemXChainTooMany => -261,

            // tel (-399..-300)
            Self::TelLocalError => -399,
            Self::TelBadDomain => -398,
            Self::TelBadPathCount => -397,
            Self::TelBadPublicKey => -396,
            Self::TelFailedProcessing => -395,
            Self::TelInsufFeeP => -394,
            Self::TelNoAuthPeer => -393,
            Self::TelCanNotQueueBalance => -392,
            Self::TelCanNotQueueBlocked => -391,
            Self::TelCanNotQueueDrops => -390,
            Self::TelCanNotQueueFee => -389,
            Self::TelCanNotQueueFull => -388,
            Self::TelWrongNetwork => -387,
            Self::TelRequiresNetworkId => -386,
            Self::TelCanNotQueueBusy => -385,
        }
    }

    /// Create from the numeric i32 code.
    pub fn from_code(code: i32) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(Self::TesSuccess),

            100 => Ok(Self::TecClaimCost),
            101 => Ok(Self::TecPathPartial),
            102 => Ok(Self::TecUnfundedAdd),
            103 => Ok(Self::TecUnfundedOffer),
            104 => Ok(Self::TecUnfundedPayment),
            105 => Ok(Self::TecFailedProcessing),
            121 => Ok(Self::TecDirFull),
            122 => Ok(Self::TecInsufReserveOffer),
            123 => Ok(Self::TecInsufReserveLine),
            124 => Ok(Self::TecInsufReserveSupply),
            125 => Ok(Self::TecNoDst),
            126 => Ok(Self::TecNoDstInsuf),
            127 => Ok(Self::TecNoLineInsuf),
            128 => Ok(Self::TecNoLineRedundant),
            129 => Ok(Self::TecPathDry),
            130 => Ok(Self::TecUnfunded),
            131 => Ok(Self::TecNoAlternativeKey),
            132 => Ok(Self::TecNoRegularKey),
            133 => Ok(Self::TecOwners),
            134 => Ok(Self::TecNoIssuer),
            135 => Ok(Self::TecNoAuth),
            136 => Ok(Self::TecNoLine),
            137 => Ok(Self::TecInsufficientReserve),
            138 => Ok(Self::TecNoTarget),
            139 => Ok(Self::TecNoPermission),
            140 => Ok(Self::TecNoEntry),
            141 => Ok(Self::TecInsufficientPayment),
            142 => Ok(Self::TecObjectNotFound),
            143 => Ok(Self::TecDuplicate),
            150 => Ok(Self::TecKilled),
            151 => Ok(Self::TecHasObligations),
            152 => Ok(Self::TecTooSoon),
            153 => Ok(Self::TecMaxSequenceReached),
            154 => Ok(Self::TecNoSuitableNFTokenPage),
            155 => Ok(Self::TecNFTokenBurnable),
            156 => Ok(Self::TecNFTokenNotBurnable),
            157 => Ok(Self::TecNFTokenOfferNotCleared),
            158 => Ok(Self::TecCannotRemoveGivenNode),
            159 => Ok(Self::TecInvariantFailed),
            160 => Ok(Self::TecOversize),
            161 => Ok(Self::TecExpired),
            162 => Ok(Self::TecInternalError),
            163 => Ok(Self::TecNeedMasterKey),
            164 => Ok(Self::TecDstTagNeeded),
            165 => Ok(Self::TecCryptoconditionError),
            166 => Ok(Self::TecXChainBadTransferPrice),
            167 => Ok(Self::TecXChainBadProof),
            168 => Ok(Self::TecXChainNonceNeeded),
            169 => Ok(Self::TecXChainBadPublicKeyAccountPair),
            170 => Ok(Self::TecXChainProofUnknownKey),
            171 => Ok(Self::TecXChainClaimNoQuorum),
            172 => Ok(Self::TecXChainCreateAccountNoQuorum),
            173 => Ok(Self::TecXChainAccountCreatePastSeq),
            174 => Ok(Self::TecXChainNoClaimId),
            175 => Ok(Self::TecXChainPaymentEmpty),
            176 => Ok(Self::TecXChainSelfCommit),
            177 => Ok(Self::TecXChainBadDest),
            178 => Ok(Self::TecXChainNoDst),
            179 => Ok(Self::TecXChainNoSignersList),
            180 => Ok(Self::TecXChainSendingNotEmpty),
            181 => Ok(Self::TecXChainInsufClaimFee),
            182 => Ok(Self::TecXChainBadClaimId),
            183 => Ok(Self::TecXChainRewardMismatch),
            184 => Ok(Self::TecEmptyDID),
            185 => Ok(Self::TecInvalidUpdateTime),
            186 => Ok(Self::TecTokenAlreadyOwned),
            187 => Ok(Self::TecMaxTokensReached),
            188 => Ok(Self::TecArrayEmpty),
            189 => Ok(Self::TecArrayTooLarge),
            190 => Ok(Self::TecBadCredential),
            191 => Ok(Self::TecPreconditionFailed),

            -199 => Ok(Self::TefFailure),
            -198 => Ok(Self::TefAlreadyMaster),
            -197 => Ok(Self::TefBadAddOrDrop),
            -196 => Ok(Self::TefBadAuth),
            -195 => Ok(Self::TefBadAuthMaster),
            -194 => Ok(Self::TefBadLedger),
            -193 => Ok(Self::TefBadQuorum),
            -192 => Ok(Self::TefBadSignature),
            -191 => Ok(Self::TefCreated),
            -190 => Ok(Self::TefException),
            -189 => Ok(Self::TefInternal),
            -188 => Ok(Self::TefMaxLedger),
            -187 => Ok(Self::TefNoAuthRequired),
            -186 => Ok(Self::TefNotMultiSigning),
            -185 => Ok(Self::TefPastSeq),
            -184 => Ok(Self::TefSeqNumPast),
            -183 => Ok(Self::TefTooOld),
            -182 => Ok(Self::TefWrongPrior),
            -181 => Ok(Self::TefNotTrustLine),

            -99 => Ok(Self::TerRetry),
            -98 => Ok(Self::TerFundsSpent),
            -97 => Ok(Self::TerInsufFee),
            -96 => Ok(Self::TerNoAccount),
            -95 => Ok(Self::TerNoAuth),
            -94 => Ok(Self::TerNoLine),
            -92 => Ok(Self::TerPreSeq),
            -91 => Ok(Self::TerLastLedger),
            -90 => Ok(Self::TerNoRipple),
            -89 => Ok(Self::TerQueueFull),

            -299 => Ok(Self::TemMalformed),
            -298 => Ok(Self::TemBadAmount),
            -297 => Ok(Self::TemBadCurrency),
            -296 => Ok(Self::TemBadExpiration),
            -295 => Ok(Self::TemBadFee),
            -294 => Ok(Self::TemBadIssuer),
            -293 => Ok(Self::TemBadLimit),
            -292 => Ok(Self::TemBadNFTokenTransfer),
            -291 => Ok(Self::TemBadOffer),
            -290 => Ok(Self::TemBadPath),
            -289 => Ok(Self::TemBadPathLoop),
            -288 => Ok(Self::TemBadRegKey),
            -287 => Ok(Self::TemBadSend),
            -286 => Ok(Self::TemBadSequence),
            -285 => Ok(Self::TemBadSignature),
            -284 => Ok(Self::TemBadSrc),
            -283 => Ok(Self::TemBadTick),
            -282 => Ok(Self::TemBadTickSize),
            -281 => Ok(Self::TemBadTransferRate),
            -280 => Ok(Self::TemCannotPreAuthSelf),
            -279 => Ok(Self::TemDstIsObligatory),
            -278 => Ok(Self::TemDstIsRequired),
            -277 => Ok(Self::TemDstTagRequired),
            -276 => Ok(Self::TemDstTagNotNeeded),
            -275 => Ok(Self::TemInvalidFlag),
            -274 => Ok(Self::TemInvalidAccountId),
            -273 => Ok(Self::TemRedundant),
            -272 => Ok(Self::TemRippleRedundant),
            -271 => Ok(Self::TemDisabled),
            -270 => Ok(Self::TemBadSigner),
            -269 => Ok(Self::TemBadQuorum),
            -268 => Ok(Self::TemBadWeight),
            -267 => Ok(Self::TemSequenceTooHigh),
            -266 => Ok(Self::TemInvalidCount),
            -265 => Ok(Self::TemScaleOutOfRange),
            -264 => Ok(Self::TemBadNFTokenBurnOffer),
            -263 => Ok(Self::TemMalformedRequest),
            -262 => Ok(Self::TemXChainBridge),
            -261 => Ok(Self::TemXChainTooMany),

            -399 => Ok(Self::TelLocalError),
            -398 => Ok(Self::TelBadDomain),
            -397 => Ok(Self::TelBadPathCount),
            -396 => Ok(Self::TelBadPublicKey),
            -395 => Ok(Self::TelFailedProcessing),
            -394 => Ok(Self::TelInsufFeeP),
            -393 => Ok(Self::TelNoAuthPeer),
            -392 => Ok(Self::TelCanNotQueueBalance),
            -391 => Ok(Self::TelCanNotQueueBlocked),
            -390 => Ok(Self::TelCanNotQueueDrops),
            -389 => Ok(Self::TelCanNotQueueFee),
            -388 => Ok(Self::TelCanNotQueueFull),
            -387 => Ok(Self::TelWrongNetwork),
            -386 => Ok(Self::TelRequiresNetworkId),
            -385 => Ok(Self::TelCanNotQueueBusy),

            _ => Err(ProtocolError::UnknownResultCode(code)),
        }
    }

    /// Return the canonical string name (e.g., "tesSUCCESS", "tecNO_DST").
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TesSuccess => "tesSUCCESS",

            Self::TecClaimCost => "tecCLAIM",
            Self::TecPathPartial => "tecPATH_PARTIAL",
            Self::TecUnfundedAdd => "tecUNFUNDED_ADD",
            Self::TecUnfundedOffer => "tecUNFUNDED_OFFER",
            Self::TecUnfundedPayment => "tecUNFUNDED_PAYMENT",
            Self::TecFailedProcessing => "tecFAILED_PROCESSING",
            Self::TecDirFull => "tecDIR_FULL",
            Self::TecInsufReserveOffer => "tecINSUF_RESERVE_OFFER",
            Self::TecInsufReserveLine => "tecINSUF_RESERVE_LINE",
            Self::TecInsufReserveSupply => "tecINSUF_RESERVE_SUPPLY",
            Self::TecNoDst => "tecNO_DST",
            Self::TecNoDstInsuf => "tecNO_DST_INSUF",
            Self::TecNoLineInsuf => "tecNO_LINE_INSUF",
            Self::TecNoLineRedundant => "tecNO_LINE_REDUNDANT",
            Self::TecPathDry => "tecPATH_DRY",
            Self::TecUnfunded => "tecUNFUNDED",
            Self::TecNoAlternativeKey => "tecNO_ALTERNATIVE_KEY",
            Self::TecNoRegularKey => "tecNO_REGULAR_KEY",
            Self::TecOwners => "tecOWNERS",
            Self::TecNoIssuer => "tecNO_ISSUER",
            Self::TecNoAuth => "tecNO_AUTH",
            Self::TecNoLine => "tecNO_LINE",
            Self::TecInsufficientReserve => "tecINSUFFICIENT_RESERVE",
            Self::TecNoTarget => "tecNO_TARGET",
            Self::TecNoPermission => "tecNO_PERMISSION",
            Self::TecNoEntry => "tecNO_ENTRY",
            Self::TecInsufficientPayment => "tecINSUFFICIENT_PAYMENT",
            Self::TecObjectNotFound => "tecOBJECT_NOT_FOUND",
            Self::TecDuplicate => "tecDUPLICATE",
            Self::TecKilled => "tecKILLED",
            Self::TecHasObligations => "tecHAS_OBLIGATIONS",
            Self::TecTooSoon => "tecTOO_SOON",
            Self::TecMaxSequenceReached => "tecMAX_SEQUENCE_REACHED",
            Self::TecNoSuitableNFTokenPage => "tecNO_SUITABLE_NFTOKEN_PAGE",
            Self::TecNFTokenBurnable => "tecNFTOKEN_BUR_NABLE",
            Self::TecNFTokenNotBurnable => "tecNFTOKEN_NOT_BURNABLE",
            Self::TecNFTokenOfferNotCleared => "tecNFTOKEN_OFFER_NOT_CLEARED",
            Self::TecCannotRemoveGivenNode => "tecCANNOT_REMOVE_GIVEN_NODE",
            Self::TecInvariantFailed => "tecINVARIANT_FAILED",
            Self::TecOversize => "tecOVERSIZE",
            Self::TecExpired => "tecEXPIRED",
            Self::TecInternalError => "tecINTERNAL",
            Self::TecNeedMasterKey => "tecNEED_MASTER_KEY",
            Self::TecDstTagNeeded => "tecDST_TAG_NEEDED",
            Self::TecCryptoconditionError => "tecCRYPTOCONDITION_ERROR",
            Self::TecXChainBadTransferPrice => "tecXCHAIN_BAD_TRANSFER_PRICE",
            Self::TecXChainBadProof => "tecXCHAIN_BAD_PROOF",
            Self::TecXChainNonceNeeded => "tecXCHAIN_NONCE_NEEDED",
            Self::TecXChainBadPublicKeyAccountPair => "tecXCHAIN_BAD_PUBLIC_KEY_ACCOUNT_PAIR",
            Self::TecXChainProofUnknownKey => "tecXCHAIN_PROOF_UNKNOWN_KEY",
            Self::TecXChainClaimNoQuorum => "tecXCHAIN_CLAIM_NO_QUORUM",
            Self::TecXChainCreateAccountNoQuorum => "tecXCHAIN_CREATE_ACCOUNT_NO_QUORUM",
            Self::TecXChainAccountCreatePastSeq => "tecXCHAIN_ACCOUNT_CREATE_PAST_SEQ",
            Self::TecXChainNoClaimId => "tecXCHAIN_NO_CLAIM_ID",
            Self::TecXChainPaymentEmpty => "tecXCHAIN_PAYMENT_EMPTY",
            Self::TecXChainSelfCommit => "tecXCHAIN_SELF_COMMIT",
            Self::TecXChainBadDest => "tecXCHAIN_BAD_DEST",
            Self::TecXChainNoDst => "tecXCHAIN_NO_DST",
            Self::TecXChainNoSignersList => "tecXCHAIN_NO_SIGNERS_LIST",
            Self::TecXChainSendingNotEmpty => "tecXCHAIN_SENDING_NOT_EMPTY",
            Self::TecXChainInsufClaimFee => "tecXCHAIN_INSUF_CLAIM_FEE",
            Self::TecXChainBadClaimId => "tecXCHAIN_BAD_CLAIM_ID",
            Self::TecXChainRewardMismatch => "tecXCHAIN_REWARD_MISMATCH",
            Self::TecEmptyDID => "tecEMPTY_DID",
            Self::TecInvalidUpdateTime => "tecINVALID_UPDATE_TIME",
            Self::TecTokenAlreadyOwned => "tecTOKEN_ALREADY_OWNED",
            Self::TecMaxTokensReached => "tecMAX_TOKENS_REACHED",
            Self::TecArrayEmpty => "tecARRAY_EMPTY",
            Self::TecArrayTooLarge => "tecARRAY_TOO_LARGE",
            Self::TecBadCredential => "tecBAD_CREDENTIAL",
            Self::TecPreconditionFailed => "tecPRECONDITION_FAILED",

            Self::TefFailure => "tefFAILURE",
            Self::TefAlreadyMaster => "tefALREADY",
            Self::TefBadAddOrDrop => "tefBAD_ADD_AUTH",
            Self::TefBadAuth => "tefBAD_AUTH",
            Self::TefBadAuthMaster => "tefBAD_AUTH_MASTER",
            Self::TefBadLedger => "tefBAD_LEDGER",
            Self::TefBadQuorum => "tefBAD_QUORUM",
            Self::TefBadSignature => "tefBAD_SIGNATURE",
            Self::TefCreated => "tefCREATED",
            Self::TefException => "tefEXCEPTION",
            Self::TefInternal => "tefINTERNAL",
            Self::TefMaxLedger => "tefMAX_LEDGER",
            Self::TefNoAuthRequired => "tefNO_AUTH_REQUIRED",
            Self::TefNotMultiSigning => "tefNOT_MULTI_SIGNING",
            Self::TefPastSeq => "tefPAST_SEQ",
            Self::TefSeqNumPast => "tefSEQ_NUM_PAST",
            Self::TefTooOld => "tefTOO_OLD",
            Self::TefWrongPrior => "tefWRONG_PRIOR",
            Self::TefNotTrustLine => "tefNOT_TRUST_LINE",

            Self::TerRetry => "terRETRY",
            Self::TerFundsSpent => "terFUNDS_SPENT",
            Self::TerInsufFee => "terINSUF_FEE_B",
            Self::TerNoAccount => "terNO_ACCOUNT",
            Self::TerNoAuth => "terNO_AUTH",
            Self::TerNoLine => "terNO_LINE",
            Self::TerPreSeq => "terPRE_SEQ",
            Self::TerLastLedger => "terLAST",
            Self::TerNoRipple => "terNO_RIPPLE",
            Self::TerQueueFull => "terQUEUED",

            Self::TemMalformed => "temMALFORMED",
            Self::TemBadAmount => "temBAD_AMOUNT",
            Self::TemBadCurrency => "temBAD_CURRENCY",
            Self::TemBadExpiration => "temBAD_EXPIRATION",
            Self::TemBadFee => "temBAD_FEE",
            Self::TemBadIssuer => "temBAD_ISSUER",
            Self::TemBadLimit => "temBAD_LIMIT",
            Self::TemBadNFTokenTransfer => "temBAD_NFTOKEN_TRANSFER",
            Self::TemBadOffer => "temBAD_OFFER",
            Self::TemBadPath => "temBAD_PATH",
            Self::TemBadPathLoop => "temBAD_PATH_LOOP",
            Self::TemBadRegKey => "temBAD_REGKEY",
            Self::TemBadSend => "temBAD_SEND",
            Self::TemBadSequence => "temBAD_SEQUENCE",
            Self::TemBadSignature => "temBAD_SIGNATURE",
            Self::TemBadSrc => "temBAD_SRC",
            Self::TemBadTick => "temBAD_TICK",
            Self::TemBadTickSize => "temBAD_TICK_SIZE",
            Self::TemBadTransferRate => "temBAD_TRANSFER_RATE",
            Self::TemCannotPreAuthSelf => "temCANNOT_PREAUTH_SELF",
            Self::TemDstIsObligatory => "temDST_IS_OBLIGATORY",
            Self::TemDstIsRequired => "temDST_IS_REQUIRED",
            Self::TemDstTagRequired => "temDST_TAG_REQUIRED",
            Self::TemDstTagNotNeeded => "temDST_TAG_NOT_NEEDED",
            Self::TemInvalidFlag => "temINVALID_FLAG",
            Self::TemInvalidAccountId => "temINVALID_ACCOUNT_ID",
            Self::TemRedundant => "temREDUNDANT",
            Self::TemRippleRedundant => "temRIPPLE_EMPTY",
            Self::TemDisabled => "temDISABLED",
            Self::TemBadSigner => "temBAD_SIGNER",
            Self::TemBadQuorum => "temBAD_QUORUM",
            Self::TemBadWeight => "temBAD_WEIGHT",
            Self::TemSequenceTooHigh => "temSEQ_TOO_HIGH",
            Self::TemInvalidCount => "temINVALID_COUNT",
            Self::TemScaleOutOfRange => "temSCALE_OUT_OF_RANGE",
            Self::TemBadNFTokenBurnOffer => "temBAD_NFTOKEN_BURN_OFFER",
            Self::TemMalformedRequest => "temMALFORMED_REQUEST",
            Self::TemXChainBridge => "temXCHAIN_BRIDGE_BAD_ISSUES",
            Self::TemXChainTooMany => "temXCHAIN_TOO_MANY",

            Self::TelLocalError => "telLOCAL_ERROR",
            Self::TelBadDomain => "telBAD_DOMAIN",
            Self::TelBadPathCount => "telBAD_PATH_COUNT",
            Self::TelBadPublicKey => "telBAD_PUBLIC_KEY",
            Self::TelFailedProcessing => "telFAILED_PROCESSING",
            Self::TelInsufFeeP => "telINSUF_FEE_P",
            Self::TelNoAuthPeer => "telNO_AUTH_PEER",
            Self::TelCanNotQueueBalance => "telCAN_NOT_QUEUE_BALANCE",
            Self::TelCanNotQueueBlocked => "telCAN_NOT_QUEUE_BLOCKED",
            Self::TelCanNotQueueDrops => "telCAN_NOT_QUEUE_DROPS",
            Self::TelCanNotQueueFee => "telCAN_NOT_QUEUE_FEE",
            Self::TelCanNotQueueFull => "telCAN_NOT_QUEUE_FULL",
            Self::TelWrongNetwork => "telWRONG_NETWORK",
            Self::TelRequiresNetworkId => "telREQUIRES_NETWORK_ID",
            Self::TelCanNotQueueBusy => "telCAN_NOT_QUEUE_BUSY",
        }
    }

    /// Parse from the canonical string name.
    pub fn from_name(name: &str) -> Result<Self, ProtocolError> {
        // Linear scan -- there are only ~150 variants and this is not a hot path.
        // All variants listed above.
        ALL_RESULTS
            .iter()
            .find(|r| r.as_str() == name)
            .copied()
            .ok_or_else(|| ProtocolError::UnknownResultCodeName(name.to_string()))
    }

    /// Return the result category.
    pub fn category(&self) -> ResultCategory {
        let c = self.code();
        if c == 0 {
            ResultCategory::Tes
        } else if c >= 100 {
            ResultCategory::Tec
        } else if c <= -300 {
            ResultCategory::Tel
        } else if c <= -200 {
            ResultCategory::Tem
        } else if c <= -100 {
            ResultCategory::Tef
        } else {
            ResultCategory::Ter
        }
    }

    /// Returns true if this is tesSUCCESS.
    pub fn is_success(&self) -> bool {
        self.code() == 0
    }

    /// Returns true if the transaction was applied to the ledger (tes or tec).
    pub fn is_claimed(&self) -> bool {
        matches!(self.category(), ResultCategory::Tes | ResultCategory::Tec)
    }

    /// Returns true if the transaction might succeed on retry (ter).
    pub fn is_retryable(&self) -> bool {
        matches!(self.category(), ResultCategory::Ter)
    }
}

impl std::fmt::Display for TransactionResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for TransactionResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for TransactionResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_name(&s).map_err(serde::de::Error::custom)
    }
}

/// All result variants for linear scan in from_name.
const ALL_RESULTS: &[TransactionResult] = &[
    TransactionResult::TesSuccess,
    TransactionResult::TecClaimCost,
    TransactionResult::TecPathPartial,
    TransactionResult::TecUnfundedAdd,
    TransactionResult::TecUnfundedOffer,
    TransactionResult::TecUnfundedPayment,
    TransactionResult::TecFailedProcessing,
    TransactionResult::TecDirFull,
    TransactionResult::TecInsufReserveOffer,
    TransactionResult::TecInsufReserveLine,
    TransactionResult::TecInsufReserveSupply,
    TransactionResult::TecNoDst,
    TransactionResult::TecNoDstInsuf,
    TransactionResult::TecNoLineInsuf,
    TransactionResult::TecNoLineRedundant,
    TransactionResult::TecPathDry,
    TransactionResult::TecUnfunded,
    TransactionResult::TecNoAlternativeKey,
    TransactionResult::TecNoRegularKey,
    TransactionResult::TecOwners,
    TransactionResult::TecNoIssuer,
    TransactionResult::TecNoAuth,
    TransactionResult::TecNoLine,
    TransactionResult::TecInsufficientReserve,
    TransactionResult::TecNoTarget,
    TransactionResult::TecNoPermission,
    TransactionResult::TecNoEntry,
    TransactionResult::TecInsufficientPayment,
    TransactionResult::TecObjectNotFound,
    TransactionResult::TecDuplicate,
    TransactionResult::TecKilled,
    TransactionResult::TecHasObligations,
    TransactionResult::TecTooSoon,
    TransactionResult::TecMaxSequenceReached,
    TransactionResult::TecNoSuitableNFTokenPage,
    TransactionResult::TecNFTokenBurnable,
    TransactionResult::TecNFTokenNotBurnable,
    TransactionResult::TecNFTokenOfferNotCleared,
    TransactionResult::TecCannotRemoveGivenNode,
    TransactionResult::TecInvariantFailed,
    TransactionResult::TecOversize,
    TransactionResult::TecExpired,
    TransactionResult::TecInternalError,
    TransactionResult::TecNeedMasterKey,
    TransactionResult::TecDstTagNeeded,
    TransactionResult::TecCryptoconditionError,
    TransactionResult::TecXChainBadTransferPrice,
    TransactionResult::TecXChainBadProof,
    TransactionResult::TecXChainNonceNeeded,
    TransactionResult::TecXChainBadPublicKeyAccountPair,
    TransactionResult::TecXChainProofUnknownKey,
    TransactionResult::TecXChainClaimNoQuorum,
    TransactionResult::TecXChainCreateAccountNoQuorum,
    TransactionResult::TecXChainAccountCreatePastSeq,
    TransactionResult::TecXChainNoClaimId,
    TransactionResult::TecXChainPaymentEmpty,
    TransactionResult::TecXChainSelfCommit,
    TransactionResult::TecXChainBadDest,
    TransactionResult::TecXChainNoDst,
    TransactionResult::TecXChainNoSignersList,
    TransactionResult::TecXChainSendingNotEmpty,
    TransactionResult::TecXChainInsufClaimFee,
    TransactionResult::TecXChainBadClaimId,
    TransactionResult::TecXChainRewardMismatch,
    TransactionResult::TecEmptyDID,
    TransactionResult::TecInvalidUpdateTime,
    TransactionResult::TecTokenAlreadyOwned,
    TransactionResult::TecMaxTokensReached,
    TransactionResult::TecArrayEmpty,
    TransactionResult::TecArrayTooLarge,
    TransactionResult::TecBadCredential,
    TransactionResult::TecPreconditionFailed,
    TransactionResult::TefFailure,
    TransactionResult::TefAlreadyMaster,
    TransactionResult::TefBadAddOrDrop,
    TransactionResult::TefBadAuth,
    TransactionResult::TefBadAuthMaster,
    TransactionResult::TefBadLedger,
    TransactionResult::TefBadQuorum,
    TransactionResult::TefBadSignature,
    TransactionResult::TefCreated,
    TransactionResult::TefException,
    TransactionResult::TefInternal,
    TransactionResult::TefMaxLedger,
    TransactionResult::TefNoAuthRequired,
    TransactionResult::TefNotMultiSigning,
    TransactionResult::TefPastSeq,
    TransactionResult::TefSeqNumPast,
    TransactionResult::TefTooOld,
    TransactionResult::TefWrongPrior,
    TransactionResult::TefNotTrustLine,
    TransactionResult::TerRetry,
    TransactionResult::TerFundsSpent,
    TransactionResult::TerInsufFee,
    TransactionResult::TerNoAccount,
    TransactionResult::TerNoAuth,
    TransactionResult::TerNoLine,
    TransactionResult::TerPreSeq,
    TransactionResult::TerLastLedger,
    TransactionResult::TerNoRipple,
    TransactionResult::TerQueueFull,
    TransactionResult::TemMalformed,
    TransactionResult::TemBadAmount,
    TransactionResult::TemBadCurrency,
    TransactionResult::TemBadExpiration,
    TransactionResult::TemBadFee,
    TransactionResult::TemBadIssuer,
    TransactionResult::TemBadLimit,
    TransactionResult::TemBadNFTokenTransfer,
    TransactionResult::TemBadOffer,
    TransactionResult::TemBadPath,
    TransactionResult::TemBadPathLoop,
    TransactionResult::TemBadRegKey,
    TransactionResult::TemBadSend,
    TransactionResult::TemBadSequence,
    TransactionResult::TemBadSignature,
    TransactionResult::TemBadSrc,
    TransactionResult::TemBadTick,
    TransactionResult::TemBadTickSize,
    TransactionResult::TemBadTransferRate,
    TransactionResult::TemCannotPreAuthSelf,
    TransactionResult::TemDstIsObligatory,
    TransactionResult::TemDstIsRequired,
    TransactionResult::TemDstTagRequired,
    TransactionResult::TemDstTagNotNeeded,
    TransactionResult::TemInvalidFlag,
    TransactionResult::TemInvalidAccountId,
    TransactionResult::TemRedundant,
    TransactionResult::TemRippleRedundant,
    TransactionResult::TemDisabled,
    TransactionResult::TemBadSigner,
    TransactionResult::TemBadQuorum,
    TransactionResult::TemBadWeight,
    TransactionResult::TemSequenceTooHigh,
    TransactionResult::TemInvalidCount,
    TransactionResult::TemScaleOutOfRange,
    TransactionResult::TemBadNFTokenBurnOffer,
    TransactionResult::TemMalformedRequest,
    TransactionResult::TemXChainBridge,
    TransactionResult::TemXChainTooMany,
    TransactionResult::TelLocalError,
    TransactionResult::TelBadDomain,
    TransactionResult::TelBadPathCount,
    TransactionResult::TelBadPublicKey,
    TransactionResult::TelFailedProcessing,
    TransactionResult::TelInsufFeeP,
    TransactionResult::TelNoAuthPeer,
    TransactionResult::TelCanNotQueueBalance,
    TransactionResult::TelCanNotQueueBlocked,
    TransactionResult::TelCanNotQueueDrops,
    TransactionResult::TelCanNotQueueFee,
    TransactionResult::TelCanNotQueueFull,
    TransactionResult::TelWrongNetwork,
    TransactionResult::TelRequiresNetworkId,
    TransactionResult::TelCanNotQueueBusy,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tes_success() {
        let r = TransactionResult::TesSuccess;
        assert_eq!(r.code(), 0);
        assert_eq!(r.as_str(), "tesSUCCESS");
        assert!(r.is_success());
        assert!(r.is_claimed());
        assert!(!r.is_retryable());
        assert_eq!(r.category(), ResultCategory::Tes);
    }

    #[test]
    fn tec_codes() {
        let r = TransactionResult::TecNoDst;
        assert_eq!(r.code(), 125);
        assert!(!r.is_success());
        assert!(r.is_claimed());
        assert_eq!(r.category(), ResultCategory::Tec);
    }

    #[test]
    fn ter_retryable() {
        let r = TransactionResult::TerPreSeq;
        assert_eq!(r.code(), -92);
        assert!(r.is_retryable());
        assert!(!r.is_claimed());
        assert_eq!(r.category(), ResultCategory::Ter);
    }

    #[test]
    fn tem_malformed() {
        let r = TransactionResult::TemBadFee;
        assert_eq!(r.code(), -295);
        assert_eq!(r.category(), ResultCategory::Tem);
    }

    #[test]
    fn tef_failure() {
        let r = TransactionResult::TefBadAuth;
        assert_eq!(r.code(), -196);
        assert_eq!(r.category(), ResultCategory::Tef);
    }

    #[test]
    fn tel_local() {
        let r = TransactionResult::TelInsufFeeP;
        assert_eq!(r.code(), -394);
        assert_eq!(r.category(), ResultCategory::Tel);
    }

    #[test]
    fn from_code_roundtrip() {
        for r in ALL_RESULTS {
            let code = r.code();
            let recovered = TransactionResult::from_code(code).unwrap();
            assert_eq!(*r, recovered, "mismatch for code {code}");
        }
    }

    #[test]
    fn from_name_roundtrip() {
        for r in ALL_RESULTS {
            let name = r.as_str();
            let recovered = TransactionResult::from_name(name).unwrap();
            assert_eq!(*r, recovered, "mismatch for name {name}");
        }
    }

    #[test]
    fn unknown_code() {
        assert!(TransactionResult::from_code(999).is_err());
    }

    #[test]
    fn unknown_name() {
        assert!(TransactionResult::from_name("bogus").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let r = TransactionResult::TesSuccess;
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"tesSUCCESS\"");
        let decoded: TransactionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, r);
    }
}
