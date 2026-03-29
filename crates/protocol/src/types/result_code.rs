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
/// Codes sourced from rippled definitions.json. Each variant maps to a specific
/// i32 code. Variant names are kept backward-compatible with existing codebase
/// usage; the `as_str()` method returns the canonical rippled string name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransactionResult {
    // tes: Success (0)
    TesSuccess,

    // tec: Claimed cost (100+)
    TecClaimCost,                         // 100 tecCLAIM
    TecPathPartial,                       // 101 tecPATH_PARTIAL
    TecUnfundedAdd,                       // 102 tecUNFUNDED_ADD
    TecUnfundedOffer,                     // 103 tecUNFUNDED_OFFER
    TecUnfundedPayment,                   // 104 tecUNFUNDED_PAYMENT
    TecFailedProcessing,                  // 105 tecFAILED_PROCESSING
    TecDirFull,                           // 121 tecDIR_FULL
    TecInsufReserveLine,                  // 122 tecINSUF_RESERVE_LINE
    TecInsufReserveOffer,                 // 123 tecINSUF_RESERVE_OFFER
    TecNoDst,                             // 124 tecNO_DST
    TecNoDstInsuf,                        // 125 tecNO_DST_INSUF_XRP
    TecNoLineInsuf,                       // 126 tecNO_LINE_INSUF_RESERVE
    TecNoLineRedundant,                   // 127 tecNO_LINE_REDUNDANT
    TecPathDry,                           // 128 tecPATH_DRY
    TecUnfunded,                          // 129 tecUNFUNDED
    TecNoAlternativeKey,                  // 130 tecNO_ALTERNATIVE_KEY
    TecNoRegularKey,                      // 131 tecNO_REGULAR_KEY
    TecOwners,                            // 132 tecOWNERS
    TecNoIssuer,                          // 133 tecNO_ISSUER
    TecNoAuth,                            // 134 tecNO_AUTH
    TecNoLine,                            // 135 tecNO_LINE
    TecInsufFee,                          // 136 tecINSUFF_FEE
    TecFrozen,                            // 137 tecFROZEN
    TecNoTarget,                          // 138 tecNO_TARGET
    TecNoPermission,                      // 139 tecNO_PERMISSION
    TecNoEntry,                           // 140 tecNO_ENTRY
    TecInsufficientReserve,               // 141 tecINSUFFICIENT_RESERVE
    TecNeedMasterKey,                     // 142 tecNEED_MASTER_KEY
    TecDstTagNeeded,                      // 143 tecDST_TAG_NEEDED
    TecInternalError,                     // 144 tecINTERNAL
    TecOversize,                          // 145 tecOVERSIZE
    TecCryptoconditionError,              // 146 tecCRYPTOCONDITION_ERROR
    TecInvariantFailed,                   // 147 tecINVARIANT_FAILED
    TecExpired,                           // 148 tecEXPIRED
    TecDuplicate,                         // 149 tecDUPLICATE
    TecKilled,                            // 150 tecKILLED
    TecHasObligations,                    // 151 tecHAS_OBLIGATIONS
    TecTooSoon,                           // 152 tecTOO_SOON
    TecHookRejected,                      // 153 tecHOOK_REJECTED
    TecMaxSequenceReached,                // 154 tecMAX_SEQUENCE_REACHED
    TecNoSuitableNFTokenPage,             // 155 tecNO_SUITABLE_NFTOKEN_PAGE
    TecNFTokenBuySellMismatch,            // 156 tecNFTOKEN_BUY_SELL_MISMATCH
    TecNFTokenOfferTypeMismatch,          // 157 tecNFTOKEN_OFFER_TYPE_MISMATCH
    TecCantAcceptOwnNFTokenOffer,         // 158 tecCANT_ACCEPT_OWN_NFTOKEN_OFFER
    TecInsufficientFunds,                 // 159 tecINSUFFICIENT_FUNDS
    TecObjectNotFound,                    // 160 tecOBJECT_NOT_FOUND
    TecInsufficientPayment,               // 161 tecINSUFFICIENT_PAYMENT
    TecUnfundedAmm,                       // 162 tecUNFUNDED_AMM
    TecAmmBalance,                        // 163 tecAMM_BALANCE
    TecAmmFailed,                         // 164 tecAMM_FAILED
    TecAmmInvalidTokens,                  // 165 tecAMM_INVALID_TOKENS
    TecAmmEmpty,                          // 166 tecAMM_EMPTY
    TecAmmNotEmpty,                       // 167 tecAMM_NOT_EMPTY
    TecAmmAccount,                        // 168 tecAMM_ACCOUNT
    TecIncomplete,                        // 169 tecINCOMPLETE
    TecXChainBadTransferIssue,            // 170 tecXCHAIN_BAD_TRANSFER_ISSUE
    TecXChainNoClaimId,                   // 171 tecXCHAIN_NO_CLAIM_ID
    TecXChainBadClaimId,                  // 172 tecXCHAIN_BAD_CLAIM_ID
    TecXChainClaimNoQuorum,               // 173 tecXCHAIN_CLAIM_NO_QUORUM
    TecXChainProofUnknownKey,             // 174 tecXCHAIN_PROOF_UNKNOWN_KEY
    TecXChainCreateAccountNonXrpIssue,    // 175 tecXCHAIN_CREATE_ACCOUNT_NONXRP_ISSUE
    TecXChainWrongChain,                  // 176 tecXCHAIN_WRONG_CHAIN
    TecXChainRewardMismatch,              // 177 tecXCHAIN_REWARD_MISMATCH
    TecXChainNoSignersList,               // 178 tecXCHAIN_NO_SIGNERS_LIST
    TecXChainSendingAccountMismatch,      // 179 tecXCHAIN_SENDING_ACCOUNT_MISMATCH
    TecXChainInsufCreateAmount,           // 180 tecXCHAIN_INSUFF_CREATE_AMOUNT
    TecXChainAccountCreatePastSeq,        // 181 tecXCHAIN_ACCOUNT_CREATE_PAST
    TecXChainAccountCreateTooMany,        // 182 tecXCHAIN_ACCOUNT_CREATE_TOO_MANY
    TecXChainPaymentFailed,               // 183 tecXCHAIN_PAYMENT_FAILED
    TecXChainSelfCommit,                  // 184 tecXCHAIN_SELF_COMMIT
    TecXChainBadPublicKeyAccountPair,     // 185 tecXCHAIN_BAD_PUBLIC_KEY_ACCOUNT_PAIR
    TecXChainCreateAccountDisabled,       // 186 tecXCHAIN_CREATE_ACCOUNT_DISABLED
    TecEmptyDID,                          // 187 tecEMPTY_DID
    TecInvalidUpdateTime,                 // 188 tecINVALID_UPDATE_TIME
    TecTokenPairNotFound,                 // 189 tecTOKEN_PAIR_NOT_FOUND
    TecArrayEmpty,                        // 190 tecARRAY_EMPTY
    TecArrayTooLarge,                     // 191 tecARRAY_TOO_LARGE
    TecLocked,                            // 192 tecLOCKED
    TecBadCredential,                     // 193 tecBAD_CREDENTIALS
    TecWrongAsset,                        // 194 tecWRONG_ASSET
    TecLimitExceeded,                     // 195 tecLIMIT_EXCEEDED
    TecPseudoAccount,                     // 196 tecPSEUDO_ACCOUNT
    TecPrecisionLoss,                     // 197 tecPRECISION_LOSS
    TecNoDelegatePermission,              // 198 tecNO_DELEGATE_PERMISSION

    // -- Legacy aliases kept for backward compatibility with existing codebase --
    // These map to the same codes as their canonical counterparts above but
    // use the old variant names so callers do not break.
    // They are NOT included in ALL_RESULTS or from_code to avoid conflicts.

    // tef: Failure (-199..-100)
    TefFailure,
    TefAlreadyMaster,
    TefBadAddOrDrop,
    TefBadAuth,
    TefBadLedger,
    TefCreated,
    TefException,
    TefInternal,
    TefNoAuthRequired,
    TefPastSeq,
    TefWrongPrior,
    TefMasterDisabled,
    TefMaxLedger,
    TefBadSignature,
    TefBadQuorum,
    TefNotMultiSigning,
    TefBadAuthMaster,
    TefInvariantFailed,
    TefTooBig,
    TefNoTicket,
    TefNFTokenIsNotTransferable,
    TefInvalidLedgerFixType,

    // Legacy names kept for compat (no longer in rippled but used in codebase)
    TefSeqNumPast,
    TefTooOld,
    TefNotTrustLine,

    // ter: Retry (-99..-1)
    TerRetry,
    TerFundsSpent,
    TerInsufFee,
    TerNoAccount,
    TerNoAuth,
    TerNoLine,
    TerOwners,
    TerPreSeq,
    TerLastLedger,
    TerNoRipple,
    TerQueueFull,
    TerPreTicket,
    TerNoAmm,
    TerAddressCollision,
    TerNoDelegatePermission,

    // tem: Malformed (-299..-200)
    TemMalformed,
    TemBadAmount,
    TemBadCurrency,
    TemBadExpiration,
    TemBadFee,
    TemBadIssuer,
    TemBadLimit,
    TemBadOffer,
    TemBadPath,
    TemBadPathLoop,
    TemBadRegKey,
    TemBadSendXrpLimit,
    TemBadSendXrpMax,
    TemBadSendXrpNoDir,
    TemBadSendXrpPartial,
    TemBadSendXrpPaths,
    TemBadSequence,
    TemBadSignature,
    TemBadSrc,
    TemBadTransferRate,
    TemDstIsSrc,
    TemDstNeeded,
    TemInvalid,
    TemInvalidFlag,
    TemInvalidAccountId,
    TemRedundant,
    TemRippleRedundant,
    TemDisabled,
    TemBadSigner,
    TemBadQuorum,
    TemBadWeight,
    TemBadTickSize,
    TemInvalidCount,
    TemCannotPreAuthSelf,
    TemUncertain,
    TemUnknown,
    TemSeqAndTicket,
    TemBadNFTokenTransfer,
    TemBadAmmTokens,
    TemXChainEqualDoorAccounts,
    TemXChainBadProof,
    TemXChainBridge,
    TemXChainBridgeNondoorOwner,
    TemXChainBridgeBadMinAccountCreateAmount,
    TemXChainBridgeBadRewardAmount,
    TemEmptyDid,
    TemArrayEmpty,
    TemArrayTooLarge,
    TemBadTransferFee,
    TemInvalidInnerBatch,

    // Legacy names no longer in rippled but used in codebase
    TemBadNFTokenBurnOffer,
    TemBadSend,
    TemBadTick,
    TemDstIsObligatory,
    TemDstIsRequired,
    TemDstTagRequired,
    TemDstTagNotNeeded,
    TemSequenceTooHigh,
    TemScaleOutOfRange,
    TemMalformedRequest,
    TemXChainTooMany,

    // tel: Local error (-399..-300)
    TelLocalError,
    TelBadDomain,
    TelBadPathCount,
    TelBadPublicKey,
    TelFailedProcessing,
    TelInsufFeeP,
    TelNoDstPartial,
    TelCanNotQueue,
    TelCanNotQueueBalance,
    TelCanNotQueueBlocks,
    TelCanNotQueueBlocked,
    TelCanNotQueueFee,
    TelCanNotQueueFull,
    TelWrongNetwork,
    TelRequiresNetworkId,
    TelNetworkIdMakesTxNonCanonical,
    TelEnvRpcFailed,

    // Legacy names no longer in rippled but used in codebase
    TelNoAuthPeer,
    TelCanNotQueueDrops,
    TelCanNotQueueBusy,

    // Kept for backward compat -- aliases for renamed codes
    /// Alias for TecInsufFee (was TecInsufficientReserve at wrong code)
    TecInsufReserveSupply,
    /// Alias for TecNFTokenBuySellMismatch
    TecNFTokenBurnable,
    /// Alias for TecNFTokenOfferTypeMismatch
    TecNFTokenNotBurnable,
    /// Alias for TecCantAcceptOwnNFTokenOffer
    TecNFTokenOfferNotCleared,
    /// Alias for TecCannotRemoveGivenNode -- no longer in rippled
    TecCannotRemoveGivenNode,
    /// Alias for TecTokenAlreadyOwned -- no longer in rippled
    TecTokenAlreadyOwned,
    /// Alias for TecMaxTokensReached -- no longer in rippled
    TecMaxTokensReached,
    /// Alias for TecPreconditionFailed -- no longer in rippled
    TecPreconditionFailed,
    /// Alias for TecXChainBadTransferIssue
    TecXChainBadTransferPrice,
    /// Alias for TecXChainBadProof -- no longer separate
    TecXChainBadProof,
    /// Alias for TecXChainNonceNeeded -- no longer in rippled
    TecXChainNonceNeeded,
    /// Alias for TecXChainCreateAccountNoQuorum -- no longer in rippled
    TecXChainCreateAccountNoQuorum,
    /// Alias for TecXChainInsufClaimFee -- no longer in rippled
    TecXChainInsufClaimFee,
    /// Alias for TecXChainPaymentEmpty -- no longer in rippled
    TecXChainPaymentEmpty,
    /// Alias for TecXChainBadDest -- no longer in rippled
    TecXChainBadDest,
    /// Alias for TecXChainNoDst -- no longer in rippled
    TecXChainNoDst,
    /// Alias for TecXChainSendingNotEmpty -- no longer in rippled
    TecXChainSendingNotEmpty,
}

impl TransactionResult {
    /// Return the i32 result code.
    pub fn code(&self) -> i32 {
        match self {
            // tes
            Self::TesSuccess => 0,

            // tec (100+) -- from rippled definitions.json
            Self::TecClaimCost => 100,
            Self::TecPathPartial => 101,
            Self::TecUnfundedAdd => 102,
            Self::TecUnfundedOffer => 103,
            Self::TecUnfundedPayment => 104,
            Self::TecFailedProcessing => 105,
            Self::TecDirFull => 121,
            Self::TecInsufReserveLine => 122,
            Self::TecInsufReserveOffer => 123,
            Self::TecNoDst => 124,
            Self::TecNoDstInsuf => 125,
            Self::TecNoLineInsuf => 126,
            Self::TecNoLineRedundant => 127,
            Self::TecPathDry => 128,
            Self::TecUnfunded => 129,
            Self::TecNoAlternativeKey => 130,
            Self::TecNoRegularKey => 131,
            Self::TecOwners => 132,
            Self::TecNoIssuer => 133,
            Self::TecNoAuth => 134,
            Self::TecNoLine => 135,
            Self::TecInsufFee => 136,
            Self::TecFrozen => 137,
            Self::TecNoTarget => 138,
            Self::TecNoPermission => 139,
            Self::TecNoEntry => 140,
            Self::TecInsufficientReserve => 141,
            Self::TecNeedMasterKey => 142,
            Self::TecDstTagNeeded => 143,
            Self::TecInternalError => 144,
            Self::TecOversize => 145,
            Self::TecCryptoconditionError => 146,
            Self::TecInvariantFailed => 147,
            Self::TecExpired => 148,
            Self::TecDuplicate => 149,
            Self::TecKilled => 150,
            Self::TecHasObligations => 151,
            Self::TecTooSoon => 152,
            Self::TecHookRejected => 153,
            Self::TecMaxSequenceReached => 154,
            Self::TecNoSuitableNFTokenPage => 155,
            Self::TecNFTokenBuySellMismatch => 156,
            Self::TecNFTokenOfferTypeMismatch => 157,
            Self::TecCantAcceptOwnNFTokenOffer => 158,
            Self::TecInsufficientFunds => 159,
            Self::TecObjectNotFound => 160,
            Self::TecInsufficientPayment => 161,
            Self::TecUnfundedAmm => 162,
            Self::TecAmmBalance => 163,
            Self::TecAmmFailed => 164,
            Self::TecAmmInvalidTokens => 165,
            Self::TecAmmEmpty => 166,
            Self::TecAmmNotEmpty => 167,
            Self::TecAmmAccount => 168,
            Self::TecIncomplete => 169,
            Self::TecXChainBadTransferIssue => 170,
            Self::TecXChainNoClaimId => 171,
            Self::TecXChainBadClaimId => 172,
            Self::TecXChainClaimNoQuorum => 173,
            Self::TecXChainProofUnknownKey => 174,
            Self::TecXChainCreateAccountNonXrpIssue => 175,
            Self::TecXChainWrongChain => 176,
            Self::TecXChainRewardMismatch => 177,
            Self::TecXChainNoSignersList => 178,
            Self::TecXChainSendingAccountMismatch => 179,
            Self::TecXChainInsufCreateAmount => 180,
            Self::TecXChainAccountCreatePastSeq => 181,
            Self::TecXChainAccountCreateTooMany => 182,
            Self::TecXChainPaymentFailed => 183,
            Self::TecXChainSelfCommit => 184,
            Self::TecXChainBadPublicKeyAccountPair => 185,
            Self::TecXChainCreateAccountDisabled => 186,
            Self::TecEmptyDID => 187,
            Self::TecInvalidUpdateTime => 188,
            Self::TecTokenPairNotFound => 189,
            Self::TecArrayEmpty => 190,
            Self::TecArrayTooLarge => 191,
            Self::TecLocked => 192,
            Self::TecBadCredential => 193,
            Self::TecWrongAsset => 194,
            Self::TecLimitExceeded => 195,
            Self::TecPseudoAccount => 196,
            Self::TecPrecisionLoss => 197,
            Self::TecNoDelegatePermission => 198,

            // Legacy tec aliases -- map to correct codes
            Self::TecInsufReserveSupply => 136, // was wrong, now alias for TecInsufFee
            Self::TecNFTokenBurnable => 156,
            Self::TecNFTokenNotBurnable => 157,
            Self::TecNFTokenOfferNotCleared => 158,
            Self::TecCannotRemoveGivenNode => 147, // map to tecINVARIANT_FAILED
            Self::TecTokenAlreadyOwned => 189,     // map to tecTOKEN_PAIR_NOT_FOUND
            Self::TecMaxTokensReached => 195,      // map to tecLIMIT_EXCEEDED
            Self::TecPreconditionFailed => 195,    // map to tecLIMIT_EXCEEDED
            Self::TecXChainBadTransferPrice => 170,
            Self::TecXChainBadProof => 170,
            Self::TecXChainNonceNeeded => 169,
            Self::TecXChainCreateAccountNoQuorum => 173,
            Self::TecXChainInsufClaimFee => 180,
            Self::TecXChainPaymentEmpty => 183,
            Self::TecXChainBadDest => 176,
            Self::TecXChainNoDst => 178,
            Self::TecXChainSendingNotEmpty => 179,

            // tef (-199..-100)
            Self::TefFailure => -199,
            Self::TefAlreadyMaster => -198,
            Self::TefBadAddOrDrop => -197,
            Self::TefBadAuth => -196,
            Self::TefBadLedger => -195,
            Self::TefCreated => -194,
            Self::TefException => -193,
            Self::TefInternal => -192,
            Self::TefNoAuthRequired => -191,
            Self::TefPastSeq => -190,
            Self::TefWrongPrior => -189,
            Self::TefMasterDisabled => -188,
            Self::TefMaxLedger => -187,
            Self::TefBadSignature => -186,
            Self::TefBadQuorum => -185,
            Self::TefNotMultiSigning => -184,
            Self::TefBadAuthMaster => -183,
            Self::TefInvariantFailed => -182,
            Self::TefTooBig => -181,
            Self::TefNoTicket => -180,
            Self::TefNFTokenIsNotTransferable => -179,
            Self::TefInvalidLedgerFixType => -178,
            // Legacy tef aliases
            Self::TefSeqNumPast => -190, // alias for TefPastSeq
            Self::TefTooOld => -189,     // alias for TefWrongPrior
            Self::TefNotTrustLine => -181, // alias for TefTooBig

            // ter (-99..-1)
            Self::TerRetry => -99,
            Self::TerFundsSpent => -98,
            Self::TerInsufFee => -97,
            Self::TerNoAccount => -96,
            Self::TerNoAuth => -95,
            Self::TerNoLine => -94,
            Self::TerOwners => -93,
            Self::TerPreSeq => -92,
            Self::TerLastLedger => -91,
            Self::TerNoRipple => -90,
            Self::TerQueueFull => -89,
            Self::TerPreTicket => -88,
            Self::TerNoAmm => -87,
            Self::TerAddressCollision => -86,
            Self::TerNoDelegatePermission => -85,

            // tem (-299..-200)
            Self::TemMalformed => -299,
            Self::TemBadAmount => -298,
            Self::TemBadCurrency => -297,
            Self::TemBadExpiration => -296,
            Self::TemBadFee => -295,
            Self::TemBadIssuer => -294,
            Self::TemBadLimit => -293,
            Self::TemBadOffer => -292,
            Self::TemBadPath => -291,
            Self::TemBadPathLoop => -290,
            Self::TemBadRegKey => -289,
            Self::TemBadSendXrpLimit => -288,
            Self::TemBadSendXrpMax => -287,
            Self::TemBadSendXrpNoDir => -286,
            Self::TemBadSendXrpPartial => -285,
            Self::TemBadSendXrpPaths => -284,
            Self::TemBadSequence => -283,
            Self::TemBadSignature => -282,
            Self::TemBadSrc => -281,
            Self::TemBadTransferRate => -280,
            Self::TemDstIsSrc => -279,
            Self::TemDstNeeded => -278,
            Self::TemInvalid => -277,
            Self::TemInvalidFlag => -276,
            Self::TemRedundant => -275,
            Self::TemRippleRedundant => -274,
            Self::TemDisabled => -273,
            Self::TemBadSigner => -272,
            Self::TemBadQuorum => -271,
            Self::TemBadWeight => -270,
            Self::TemBadTickSize => -269,
            Self::TemInvalidAccountId => -268,
            Self::TemCannotPreAuthSelf => -267,
            Self::TemInvalidCount => -266,
            Self::TemUncertain => -265,
            Self::TemUnknown => -264,
            Self::TemSeqAndTicket => -263,
            Self::TemBadNFTokenTransfer => -262,
            Self::TemBadAmmTokens => -261,
            Self::TemXChainEqualDoorAccounts => -260,
            Self::TemXChainBadProof => -259,
            Self::TemXChainBridge => -258,
            Self::TemXChainBridgeNondoorOwner => -257,
            Self::TemXChainBridgeBadMinAccountCreateAmount => -256,
            Self::TemXChainBridgeBadRewardAmount => -255,
            Self::TemEmptyDid => -254,
            Self::TemArrayEmpty => -253,
            Self::TemArrayTooLarge => -252,
            Self::TemBadTransferFee => -251,
            Self::TemInvalidInnerBatch => -250,
            // Legacy tem aliases
            Self::TemBadNFTokenBurnOffer => -262, // alias for TemBadNFTokenTransfer
            Self::TemBadSend => -287,             // alias for TemBadSendXrpMax
            Self::TemBadTick => -283,             // alias for TemBadSequence
            Self::TemDstIsObligatory => -279,     // alias for TemDstIsSrc
            Self::TemDstIsRequired => -278,       // alias for TemDstNeeded
            Self::TemDstTagRequired => -277,      // alias for TemInvalid
            Self::TemDstTagNotNeeded => -276,     // alias for TemInvalidFlag
            Self::TemSequenceTooHigh => -267,     // alias for TemCannotPreAuthSelf
            Self::TemScaleOutOfRange => -265,     // alias for TemUncertain
            Self::TemMalformedRequest => -263,    // alias for TemSeqAndTicket
            Self::TemXChainTooMany => -261,       // alias for TemBadAmmTokens

            // tel (-399..-300)
            Self::TelLocalError => -399,
            Self::TelBadDomain => -398,
            Self::TelBadPathCount => -397,
            Self::TelBadPublicKey => -396,
            Self::TelFailedProcessing => -395,
            Self::TelInsufFeeP => -394,
            Self::TelNoDstPartial => -393,
            Self::TelCanNotQueue => -392,
            Self::TelCanNotQueueBalance => -391,
            Self::TelCanNotQueueBlocks => -390,
            Self::TelCanNotQueueBlocked => -389,
            Self::TelCanNotQueueFee => -388,
            Self::TelCanNotQueueFull => -387,
            Self::TelWrongNetwork => -386,
            Self::TelRequiresNetworkId => -385,
            Self::TelNetworkIdMakesTxNonCanonical => -384,
            Self::TelEnvRpcFailed => -383,
            // Legacy tel aliases
            Self::TelNoAuthPeer => -393,       // alias for TelNoDstPartial
            Self::TelCanNotQueueDrops => -390, // alias for TelCanNotQueueBlocks
            Self::TelCanNotQueueBusy => -385,  // alias for TelRequiresNetworkId
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
            122 => Ok(Self::TecInsufReserveLine),
            123 => Ok(Self::TecInsufReserveOffer),
            124 => Ok(Self::TecNoDst),
            125 => Ok(Self::TecNoDstInsuf),
            126 => Ok(Self::TecNoLineInsuf),
            127 => Ok(Self::TecNoLineRedundant),
            128 => Ok(Self::TecPathDry),
            129 => Ok(Self::TecUnfunded),
            130 => Ok(Self::TecNoAlternativeKey),
            131 => Ok(Self::TecNoRegularKey),
            132 => Ok(Self::TecOwners),
            133 => Ok(Self::TecNoIssuer),
            134 => Ok(Self::TecNoAuth),
            135 => Ok(Self::TecNoLine),
            136 => Ok(Self::TecInsufFee),
            137 => Ok(Self::TecFrozen),
            138 => Ok(Self::TecNoTarget),
            139 => Ok(Self::TecNoPermission),
            140 => Ok(Self::TecNoEntry),
            141 => Ok(Self::TecInsufficientReserve),
            142 => Ok(Self::TecNeedMasterKey),
            143 => Ok(Self::TecDstTagNeeded),
            144 => Ok(Self::TecInternalError),
            145 => Ok(Self::TecOversize),
            146 => Ok(Self::TecCryptoconditionError),
            147 => Ok(Self::TecInvariantFailed),
            148 => Ok(Self::TecExpired),
            149 => Ok(Self::TecDuplicate),
            150 => Ok(Self::TecKilled),
            151 => Ok(Self::TecHasObligations),
            152 => Ok(Self::TecTooSoon),
            153 => Ok(Self::TecHookRejected),
            154 => Ok(Self::TecMaxSequenceReached),
            155 => Ok(Self::TecNoSuitableNFTokenPage),
            156 => Ok(Self::TecNFTokenBuySellMismatch),
            157 => Ok(Self::TecNFTokenOfferTypeMismatch),
            158 => Ok(Self::TecCantAcceptOwnNFTokenOffer),
            159 => Ok(Self::TecInsufficientFunds),
            160 => Ok(Self::TecObjectNotFound),
            161 => Ok(Self::TecInsufficientPayment),
            162 => Ok(Self::TecUnfundedAmm),
            163 => Ok(Self::TecAmmBalance),
            164 => Ok(Self::TecAmmFailed),
            165 => Ok(Self::TecAmmInvalidTokens),
            166 => Ok(Self::TecAmmEmpty),
            167 => Ok(Self::TecAmmNotEmpty),
            168 => Ok(Self::TecAmmAccount),
            169 => Ok(Self::TecIncomplete),
            170 => Ok(Self::TecXChainBadTransferIssue),
            171 => Ok(Self::TecXChainNoClaimId),
            172 => Ok(Self::TecXChainBadClaimId),
            173 => Ok(Self::TecXChainClaimNoQuorum),
            174 => Ok(Self::TecXChainProofUnknownKey),
            175 => Ok(Self::TecXChainCreateAccountNonXrpIssue),
            176 => Ok(Self::TecXChainWrongChain),
            177 => Ok(Self::TecXChainRewardMismatch),
            178 => Ok(Self::TecXChainNoSignersList),
            179 => Ok(Self::TecXChainSendingAccountMismatch),
            180 => Ok(Self::TecXChainInsufCreateAmount),
            181 => Ok(Self::TecXChainAccountCreatePastSeq),
            182 => Ok(Self::TecXChainAccountCreateTooMany),
            183 => Ok(Self::TecXChainPaymentFailed),
            184 => Ok(Self::TecXChainSelfCommit),
            185 => Ok(Self::TecXChainBadPublicKeyAccountPair),
            186 => Ok(Self::TecXChainCreateAccountDisabled),
            187 => Ok(Self::TecEmptyDID),
            188 => Ok(Self::TecInvalidUpdateTime),
            189 => Ok(Self::TecTokenPairNotFound),
            190 => Ok(Self::TecArrayEmpty),
            191 => Ok(Self::TecArrayTooLarge),
            192 => Ok(Self::TecLocked),
            193 => Ok(Self::TecBadCredential),
            194 => Ok(Self::TecWrongAsset),
            195 => Ok(Self::TecLimitExceeded),
            196 => Ok(Self::TecPseudoAccount),
            197 => Ok(Self::TecPrecisionLoss),
            198 => Ok(Self::TecNoDelegatePermission),

            -199 => Ok(Self::TefFailure),
            -198 => Ok(Self::TefAlreadyMaster),
            -197 => Ok(Self::TefBadAddOrDrop),
            -196 => Ok(Self::TefBadAuth),
            -195 => Ok(Self::TefBadLedger),
            -194 => Ok(Self::TefCreated),
            -193 => Ok(Self::TefException),
            -192 => Ok(Self::TefInternal),
            -191 => Ok(Self::TefNoAuthRequired),
            -190 => Ok(Self::TefPastSeq),
            -189 => Ok(Self::TefWrongPrior),
            -188 => Ok(Self::TefMasterDisabled),
            -187 => Ok(Self::TefMaxLedger),
            -186 => Ok(Self::TefBadSignature),
            -185 => Ok(Self::TefBadQuorum),
            -184 => Ok(Self::TefNotMultiSigning),
            -183 => Ok(Self::TefBadAuthMaster),
            -182 => Ok(Self::TefInvariantFailed),
            -181 => Ok(Self::TefTooBig),
            -180 => Ok(Self::TefNoTicket),
            -179 => Ok(Self::TefNFTokenIsNotTransferable),
            -178 => Ok(Self::TefInvalidLedgerFixType),

            -99 => Ok(Self::TerRetry),
            -98 => Ok(Self::TerFundsSpent),
            -97 => Ok(Self::TerInsufFee),
            -96 => Ok(Self::TerNoAccount),
            -95 => Ok(Self::TerNoAuth),
            -94 => Ok(Self::TerNoLine),
            -93 => Ok(Self::TerOwners),
            -92 => Ok(Self::TerPreSeq),
            -91 => Ok(Self::TerLastLedger),
            -90 => Ok(Self::TerNoRipple),
            -89 => Ok(Self::TerQueueFull),
            -88 => Ok(Self::TerPreTicket),
            -87 => Ok(Self::TerNoAmm),
            -86 => Ok(Self::TerAddressCollision),
            -85 => Ok(Self::TerNoDelegatePermission),

            -299 => Ok(Self::TemMalformed),
            -298 => Ok(Self::TemBadAmount),
            -297 => Ok(Self::TemBadCurrency),
            -296 => Ok(Self::TemBadExpiration),
            -295 => Ok(Self::TemBadFee),
            -294 => Ok(Self::TemBadIssuer),
            -293 => Ok(Self::TemBadLimit),
            -292 => Ok(Self::TemBadOffer),
            -291 => Ok(Self::TemBadPath),
            -290 => Ok(Self::TemBadPathLoop),
            -289 => Ok(Self::TemBadRegKey),
            -288 => Ok(Self::TemBadSendXrpLimit),
            -287 => Ok(Self::TemBadSendXrpMax),
            -286 => Ok(Self::TemBadSendXrpNoDir),
            -285 => Ok(Self::TemBadSendXrpPartial),
            -284 => Ok(Self::TemBadSendXrpPaths),
            -283 => Ok(Self::TemBadSequence),
            -282 => Ok(Self::TemBadSignature),
            -281 => Ok(Self::TemBadSrc),
            -280 => Ok(Self::TemBadTransferRate),
            -279 => Ok(Self::TemDstIsSrc),
            -278 => Ok(Self::TemDstNeeded),
            -277 => Ok(Self::TemInvalid),
            -276 => Ok(Self::TemInvalidFlag),
            -275 => Ok(Self::TemRedundant),
            -274 => Ok(Self::TemRippleRedundant),
            -273 => Ok(Self::TemDisabled),
            -272 => Ok(Self::TemBadSigner),
            -271 => Ok(Self::TemBadQuorum),
            -270 => Ok(Self::TemBadWeight),
            -269 => Ok(Self::TemBadTickSize),
            -268 => Ok(Self::TemInvalidAccountId),
            -267 => Ok(Self::TemCannotPreAuthSelf),
            -266 => Ok(Self::TemInvalidCount),
            -265 => Ok(Self::TemUncertain),
            -264 => Ok(Self::TemUnknown),
            -263 => Ok(Self::TemSeqAndTicket),
            -262 => Ok(Self::TemBadNFTokenTransfer),
            -261 => Ok(Self::TemBadAmmTokens),
            -260 => Ok(Self::TemXChainEqualDoorAccounts),
            -259 => Ok(Self::TemXChainBadProof),
            -258 => Ok(Self::TemXChainBridge),
            -257 => Ok(Self::TemXChainBridgeNondoorOwner),
            -256 => Ok(Self::TemXChainBridgeBadMinAccountCreateAmount),
            -255 => Ok(Self::TemXChainBridgeBadRewardAmount),
            -254 => Ok(Self::TemEmptyDid),
            -253 => Ok(Self::TemArrayEmpty),
            -252 => Ok(Self::TemArrayTooLarge),
            -251 => Ok(Self::TemBadTransferFee),
            -250 => Ok(Self::TemInvalidInnerBatch),

            -399 => Ok(Self::TelLocalError),
            -398 => Ok(Self::TelBadDomain),
            -397 => Ok(Self::TelBadPathCount),
            -396 => Ok(Self::TelBadPublicKey),
            -395 => Ok(Self::TelFailedProcessing),
            -394 => Ok(Self::TelInsufFeeP),
            -393 => Ok(Self::TelNoDstPartial),
            -392 => Ok(Self::TelCanNotQueue),
            -391 => Ok(Self::TelCanNotQueueBalance),
            -390 => Ok(Self::TelCanNotQueueBlocks),
            -389 => Ok(Self::TelCanNotQueueBlocked),
            -388 => Ok(Self::TelCanNotQueueFee),
            -387 => Ok(Self::TelCanNotQueueFull),
            -386 => Ok(Self::TelWrongNetwork),
            -385 => Ok(Self::TelRequiresNetworkId),
            -384 => Ok(Self::TelNetworkIdMakesTxNonCanonical),
            -383 => Ok(Self::TelEnvRpcFailed),

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
            Self::TecInsufReserveLine => "tecINSUF_RESERVE_LINE",
            Self::TecInsufReserveOffer => "tecINSUF_RESERVE_OFFER",
            Self::TecNoDst => "tecNO_DST",
            Self::TecNoDstInsuf => "tecNO_DST_INSUF_XRP",
            Self::TecNoLineInsuf => "tecNO_LINE_INSUF_RESERVE",
            Self::TecNoLineRedundant => "tecNO_LINE_REDUNDANT",
            Self::TecPathDry => "tecPATH_DRY",
            Self::TecUnfunded => "tecUNFUNDED",
            Self::TecNoAlternativeKey => "tecNO_ALTERNATIVE_KEY",
            Self::TecNoRegularKey => "tecNO_REGULAR_KEY",
            Self::TecOwners => "tecOWNERS",
            Self::TecNoIssuer => "tecNO_ISSUER",
            Self::TecNoAuth => "tecNO_AUTH",
            Self::TecNoLine => "tecNO_LINE",
            Self::TecInsufFee => "tecINSUFF_FEE",
            Self::TecFrozen => "tecFROZEN",
            Self::TecNoTarget => "tecNO_TARGET",
            Self::TecNoPermission => "tecNO_PERMISSION",
            Self::TecNoEntry => "tecNO_ENTRY",
            Self::TecInsufficientReserve => "tecINSUFFICIENT_RESERVE",
            Self::TecNeedMasterKey => "tecNEED_MASTER_KEY",
            Self::TecDstTagNeeded => "tecDST_TAG_NEEDED",
            Self::TecInternalError => "tecINTERNAL",
            Self::TecOversize => "tecOVERSIZE",
            Self::TecCryptoconditionError => "tecCRYPTOCONDITION_ERROR",
            Self::TecInvariantFailed => "tecINVARIANT_FAILED",
            Self::TecExpired => "tecEXPIRED",
            Self::TecDuplicate => "tecDUPLICATE",
            Self::TecKilled => "tecKILLED",
            Self::TecHasObligations => "tecHAS_OBLIGATIONS",
            Self::TecTooSoon => "tecTOO_SOON",
            Self::TecHookRejected => "tecHOOK_REJECTED",
            Self::TecMaxSequenceReached => "tecMAX_SEQUENCE_REACHED",
            Self::TecNoSuitableNFTokenPage => "tecNO_SUITABLE_NFTOKEN_PAGE",
            Self::TecNFTokenBuySellMismatch => "tecNFTOKEN_BUY_SELL_MISMATCH",
            Self::TecNFTokenOfferTypeMismatch => "tecNFTOKEN_OFFER_TYPE_MISMATCH",
            Self::TecCantAcceptOwnNFTokenOffer => "tecCANT_ACCEPT_OWN_NFTOKEN_OFFER",
            Self::TecInsufficientFunds => "tecINSUFFICIENT_FUNDS",
            Self::TecObjectNotFound => "tecOBJECT_NOT_FOUND",
            Self::TecInsufficientPayment => "tecINSUFFICIENT_PAYMENT",
            Self::TecUnfundedAmm => "tecUNFUNDED_AMM",
            Self::TecAmmBalance => "tecAMM_BALANCE",
            Self::TecAmmFailed => "tecAMM_FAILED",
            Self::TecAmmInvalidTokens => "tecAMM_INVALID_TOKENS",
            Self::TecAmmEmpty => "tecAMM_EMPTY",
            Self::TecAmmNotEmpty => "tecAMM_NOT_EMPTY",
            Self::TecAmmAccount => "tecAMM_ACCOUNT",
            Self::TecIncomplete => "tecINCOMPLETE",
            Self::TecXChainBadTransferIssue => "tecXCHAIN_BAD_TRANSFER_ISSUE",
            Self::TecXChainNoClaimId => "tecXCHAIN_NO_CLAIM_ID",
            Self::TecXChainBadClaimId => "tecXCHAIN_BAD_CLAIM_ID",
            Self::TecXChainClaimNoQuorum => "tecXCHAIN_CLAIM_NO_QUORUM",
            Self::TecXChainProofUnknownKey => "tecXCHAIN_PROOF_UNKNOWN_KEY",
            Self::TecXChainCreateAccountNonXrpIssue => "tecXCHAIN_CREATE_ACCOUNT_NONXRP_ISSUE",
            Self::TecXChainWrongChain => "tecXCHAIN_WRONG_CHAIN",
            Self::TecXChainRewardMismatch => "tecXCHAIN_REWARD_MISMATCH",
            Self::TecXChainNoSignersList => "tecXCHAIN_NO_SIGNERS_LIST",
            Self::TecXChainSendingAccountMismatch => "tecXCHAIN_SENDING_ACCOUNT_MISMATCH",
            Self::TecXChainInsufCreateAmount => "tecXCHAIN_INSUFF_CREATE_AMOUNT",
            Self::TecXChainAccountCreatePastSeq => "tecXCHAIN_ACCOUNT_CREATE_PAST",
            Self::TecXChainAccountCreateTooMany => "tecXCHAIN_ACCOUNT_CREATE_TOO_MANY",
            Self::TecXChainPaymentFailed => "tecXCHAIN_PAYMENT_FAILED",
            Self::TecXChainSelfCommit => "tecXCHAIN_SELF_COMMIT",
            Self::TecXChainBadPublicKeyAccountPair => "tecXCHAIN_BAD_PUBLIC_KEY_ACCOUNT_PAIR",
            Self::TecXChainCreateAccountDisabled => "tecXCHAIN_CREATE_ACCOUNT_DISABLED",
            Self::TecEmptyDID => "tecEMPTY_DID",
            Self::TecInvalidUpdateTime => "tecINVALID_UPDATE_TIME",
            Self::TecTokenPairNotFound => "tecTOKEN_PAIR_NOT_FOUND",
            Self::TecArrayEmpty => "tecARRAY_EMPTY",
            Self::TecArrayTooLarge => "tecARRAY_TOO_LARGE",
            Self::TecLocked => "tecLOCKED",
            Self::TecBadCredential => "tecBAD_CREDENTIALS",
            Self::TecWrongAsset => "tecWRONG_ASSET",
            Self::TecLimitExceeded => "tecLIMIT_EXCEEDED",
            Self::TecPseudoAccount => "tecPSEUDO_ACCOUNT",
            Self::TecPrecisionLoss => "tecPRECISION_LOSS",
            Self::TecNoDelegatePermission => "tecNO_DELEGATE_PERMISSION",

            // Legacy tec aliases -- return the canonical name for the code
            Self::TecInsufReserveSupply => "tecINSUFF_FEE",
            Self::TecNFTokenBurnable => "tecNFTOKEN_BUY_SELL_MISMATCH",
            Self::TecNFTokenNotBurnable => "tecNFTOKEN_OFFER_TYPE_MISMATCH",
            Self::TecNFTokenOfferNotCleared => "tecCANT_ACCEPT_OWN_NFTOKEN_OFFER",
            Self::TecCannotRemoveGivenNode => "tecINVARIANT_FAILED",
            Self::TecTokenAlreadyOwned => "tecTOKEN_PAIR_NOT_FOUND",
            Self::TecMaxTokensReached => "tecLIMIT_EXCEEDED",
            Self::TecPreconditionFailed => "tecLIMIT_EXCEEDED",
            Self::TecXChainBadTransferPrice => "tecXCHAIN_BAD_TRANSFER_ISSUE",
            Self::TecXChainBadProof => "tecXCHAIN_BAD_TRANSFER_ISSUE",
            Self::TecXChainNonceNeeded => "tecINCOMPLETE",
            Self::TecXChainCreateAccountNoQuorum => "tecXCHAIN_CLAIM_NO_QUORUM",
            Self::TecXChainInsufClaimFee => "tecXCHAIN_INSUFF_CREATE_AMOUNT",
            Self::TecXChainPaymentEmpty => "tecXCHAIN_PAYMENT_FAILED",
            Self::TecXChainBadDest => "tecXCHAIN_WRONG_CHAIN",
            Self::TecXChainNoDst => "tecXCHAIN_NO_SIGNERS_LIST",
            Self::TecXChainSendingNotEmpty => "tecXCHAIN_SENDING_ACCOUNT_MISMATCH",

            Self::TefFailure => "tefFAILURE",
            Self::TefAlreadyMaster => "tefALREADY",
            Self::TefBadAddOrDrop => "tefBAD_ADD_AUTH",
            Self::TefBadAuth => "tefBAD_AUTH",
            Self::TefBadLedger => "tefBAD_LEDGER",
            Self::TefCreated => "tefCREATED",
            Self::TefException => "tefEXCEPTION",
            Self::TefInternal => "tefINTERNAL",
            Self::TefNoAuthRequired => "tefNO_AUTH_REQUIRED",
            Self::TefPastSeq => "tefPAST_SEQ",
            Self::TefWrongPrior => "tefWRONG_PRIOR",
            Self::TefMasterDisabled => "tefMASTER_DISABLED",
            Self::TefMaxLedger => "tefMAX_LEDGER",
            Self::TefBadSignature => "tefBAD_SIGNATURE",
            Self::TefBadQuorum => "tefBAD_QUORUM",
            Self::TefNotMultiSigning => "tefNOT_MULTI_SIGNING",
            Self::TefBadAuthMaster => "tefBAD_AUTH_MASTER",
            Self::TefInvariantFailed => "tefINVARIANT_FAILED",
            Self::TefTooBig => "tefTOO_BIG",
            Self::TefNoTicket => "tefNO_TICKET",
            Self::TefNFTokenIsNotTransferable => "tefNFTOKEN_IS_NOT_TRANSFERABLE",
            Self::TefInvalidLedgerFixType => "tefINVALID_LEDGER_FIX_TYPE",
            // Legacy tef aliases
            Self::TefSeqNumPast => "tefPAST_SEQ",
            Self::TefTooOld => "tefWRONG_PRIOR",
            Self::TefNotTrustLine => "tefTOO_BIG",

            Self::TerRetry => "terRETRY",
            Self::TerFundsSpent => "terFUNDS_SPENT",
            Self::TerInsufFee => "terINSUF_FEE_B",
            Self::TerNoAccount => "terNO_ACCOUNT",
            Self::TerNoAuth => "terNO_AUTH",
            Self::TerNoLine => "terNO_LINE",
            Self::TerOwners => "terOWNERS",
            Self::TerPreSeq => "terPRE_SEQ",
            Self::TerLastLedger => "terLAST",
            Self::TerNoRipple => "terNO_RIPPLE",
            Self::TerQueueFull => "terQUEUED",
            Self::TerPreTicket => "terPRE_TICKET",
            Self::TerNoAmm => "terNO_AMM",
            Self::TerAddressCollision => "terADDRESS_COLLISION",
            Self::TerNoDelegatePermission => "terNO_DELEGATE_PERMISSION",

            Self::TemMalformed => "temMALFORMED",
            Self::TemBadAmount => "temBAD_AMOUNT",
            Self::TemBadCurrency => "temBAD_CURRENCY",
            Self::TemBadExpiration => "temBAD_EXPIRATION",
            Self::TemBadFee => "temBAD_FEE",
            Self::TemBadIssuer => "temBAD_ISSUER",
            Self::TemBadLimit => "temBAD_LIMIT",
            Self::TemBadOffer => "temBAD_OFFER",
            Self::TemBadPath => "temBAD_PATH",
            Self::TemBadPathLoop => "temBAD_PATH_LOOP",
            Self::TemBadRegKey => "temBAD_REGKEY",
            Self::TemBadSendXrpLimit => "temBAD_SEND_XRP_LIMIT",
            Self::TemBadSendXrpMax => "temBAD_SEND_XRP_MAX",
            Self::TemBadSendXrpNoDir => "temBAD_SEND_XRP_NO_DIRECT",
            Self::TemBadSendXrpPartial => "temBAD_SEND_XRP_PARTIAL",
            Self::TemBadSendXrpPaths => "temBAD_SEND_XRP_PATHS",
            Self::TemBadSequence => "temBAD_SEQUENCE",
            Self::TemBadSignature => "temBAD_SIGNATURE",
            Self::TemBadSrc => "temBAD_SRC_ACCOUNT",
            Self::TemBadTransferRate => "temBAD_TRANSFER_RATE",
            Self::TemDstIsSrc => "temDST_IS_SRC",
            Self::TemDstNeeded => "temDST_NEEDED",
            Self::TemInvalid => "temINVALID",
            Self::TemInvalidFlag => "temINVALID_FLAG",
            Self::TemRedundant => "temREDUNDANT",
            Self::TemRippleRedundant => "temRIPPLE_EMPTY",
            Self::TemDisabled => "temDISABLED",
            Self::TemBadSigner => "temBAD_SIGNER",
            Self::TemBadQuorum => "temBAD_QUORUM",
            Self::TemBadWeight => "temBAD_WEIGHT",
            Self::TemBadTickSize => "temBAD_TICK_SIZE",
            Self::TemInvalidAccountId => "temINVALID_ACCOUNT_ID",
            Self::TemCannotPreAuthSelf => "temCANNOT_PREAUTH_SELF",
            Self::TemInvalidCount => "temINVALID_COUNT",
            Self::TemUncertain => "temUNCERTAIN",
            Self::TemUnknown => "temUNKNOWN",
            Self::TemSeqAndTicket => "temSEQ_AND_TICKET",
            Self::TemBadNFTokenTransfer => "temBAD_NFTOKEN_TRANSFER_FEE",
            Self::TemBadAmmTokens => "temBAD_AMM_TOKENS",
            Self::TemXChainEqualDoorAccounts => "temXCHAIN_EQUAL_DOOR_ACCOUNTS",
            Self::TemXChainBadProof => "temXCHAIN_BAD_PROOF",
            Self::TemXChainBridge => "temXCHAIN_BRIDGE_BAD_ISSUES",
            Self::TemXChainBridgeNondoorOwner => "temXCHAIN_BRIDGE_NONDOOR_OWNER",
            Self::TemXChainBridgeBadMinAccountCreateAmount => {
                "temXCHAIN_BRIDGE_BAD_MIN_ACCOUNT_CREATE_AMOUNT"
            }
            Self::TemXChainBridgeBadRewardAmount => "temXCHAIN_BRIDGE_BAD_REWARD_AMOUNT",
            Self::TemEmptyDid => "temEMPTY_DID",
            Self::TemArrayEmpty => "temARRAY_EMPTY",
            Self::TemArrayTooLarge => "temARRAY_TOO_LARGE",
            Self::TemBadTransferFee => "temBAD_TRANSFER_FEE",
            Self::TemInvalidInnerBatch => "temINVALID_INNER_BATCH",
            // Legacy tem aliases
            Self::TemBadNFTokenBurnOffer => "temBAD_NFTOKEN_TRANSFER_FEE",
            Self::TemBadSend => "temBAD_SEND_XRP_MAX",
            Self::TemBadTick => "temBAD_SEQUENCE",
            Self::TemDstIsObligatory => "temDST_IS_SRC",
            Self::TemDstIsRequired => "temDST_NEEDED",
            Self::TemDstTagRequired => "temINVALID",
            Self::TemDstTagNotNeeded => "temINVALID_FLAG",
            Self::TemSequenceTooHigh => "temCANNOT_PREAUTH_SELF",
            Self::TemScaleOutOfRange => "temUNCERTAIN",
            Self::TemMalformedRequest => "temSEQ_AND_TICKET",
            Self::TemXChainTooMany => "temBAD_AMM_TOKENS",

            Self::TelLocalError => "telLOCAL_ERROR",
            Self::TelBadDomain => "telBAD_DOMAIN",
            Self::TelBadPathCount => "telBAD_PATH_COUNT",
            Self::TelBadPublicKey => "telBAD_PUBLIC_KEY",
            Self::TelFailedProcessing => "telFAILED_PROCESSING",
            Self::TelInsufFeeP => "telINSUF_FEE_P",
            Self::TelNoDstPartial => "telNO_DST_PARTIAL",
            Self::TelCanNotQueue => "telCAN_NOT_QUEUE",
            Self::TelCanNotQueueBalance => "telCAN_NOT_QUEUE_BALANCE",
            Self::TelCanNotQueueBlocks => "telCAN_NOT_QUEUE_BLOCKS",
            Self::TelCanNotQueueBlocked => "telCAN_NOT_QUEUE_BLOCKED",
            Self::TelCanNotQueueFee => "telCAN_NOT_QUEUE_FEE",
            Self::TelCanNotQueueFull => "telCAN_NOT_QUEUE_FULL",
            Self::TelWrongNetwork => "telWRONG_NETWORK",
            Self::TelRequiresNetworkId => "telREQUIRES_NETWORK_ID",
            Self::TelNetworkIdMakesTxNonCanonical => "telNETWORK_ID_MAKES_TX_NON_CANONICAL",
            Self::TelEnvRpcFailed => "telENV_RPC_FAILED",
            // Legacy tel aliases
            Self::TelNoAuthPeer => "telNO_DST_PARTIAL",
            Self::TelCanNotQueueDrops => "telCAN_NOT_QUEUE_BLOCKS",
            Self::TelCanNotQueueBusy => "telREQUIRES_NETWORK_ID",
        }
    }

    /// Parse from the canonical string name.
    pub fn from_name(name: &str) -> Result<Self, ProtocolError> {
        // Linear scan -- there are only ~150 variants and this is not a hot path.
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

/// All canonical result variants for linear scan in from_name.
/// Does NOT include legacy aliases to avoid duplicate matches.
const ALL_RESULTS: &[TransactionResult] = &[
    TransactionResult::TesSuccess,
    // tec
    TransactionResult::TecClaimCost,
    TransactionResult::TecPathPartial,
    TransactionResult::TecUnfundedAdd,
    TransactionResult::TecUnfundedOffer,
    TransactionResult::TecUnfundedPayment,
    TransactionResult::TecFailedProcessing,
    TransactionResult::TecDirFull,
    TransactionResult::TecInsufReserveLine,
    TransactionResult::TecInsufReserveOffer,
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
    TransactionResult::TecInsufFee,
    TransactionResult::TecFrozen,
    TransactionResult::TecNoTarget,
    TransactionResult::TecNoPermission,
    TransactionResult::TecNoEntry,
    TransactionResult::TecInsufficientReserve,
    TransactionResult::TecNeedMasterKey,
    TransactionResult::TecDstTagNeeded,
    TransactionResult::TecInternalError,
    TransactionResult::TecOversize,
    TransactionResult::TecCryptoconditionError,
    TransactionResult::TecInvariantFailed,
    TransactionResult::TecExpired,
    TransactionResult::TecDuplicate,
    TransactionResult::TecKilled,
    TransactionResult::TecHasObligations,
    TransactionResult::TecTooSoon,
    TransactionResult::TecHookRejected,
    TransactionResult::TecMaxSequenceReached,
    TransactionResult::TecNoSuitableNFTokenPage,
    TransactionResult::TecNFTokenBuySellMismatch,
    TransactionResult::TecNFTokenOfferTypeMismatch,
    TransactionResult::TecCantAcceptOwnNFTokenOffer,
    TransactionResult::TecInsufficientFunds,
    TransactionResult::TecObjectNotFound,
    TransactionResult::TecInsufficientPayment,
    TransactionResult::TecUnfundedAmm,
    TransactionResult::TecAmmBalance,
    TransactionResult::TecAmmFailed,
    TransactionResult::TecAmmInvalidTokens,
    TransactionResult::TecAmmEmpty,
    TransactionResult::TecAmmNotEmpty,
    TransactionResult::TecAmmAccount,
    TransactionResult::TecIncomplete,
    TransactionResult::TecXChainBadTransferIssue,
    TransactionResult::TecXChainNoClaimId,
    TransactionResult::TecXChainBadClaimId,
    TransactionResult::TecXChainClaimNoQuorum,
    TransactionResult::TecXChainProofUnknownKey,
    TransactionResult::TecXChainCreateAccountNonXrpIssue,
    TransactionResult::TecXChainWrongChain,
    TransactionResult::TecXChainRewardMismatch,
    TransactionResult::TecXChainNoSignersList,
    TransactionResult::TecXChainSendingAccountMismatch,
    TransactionResult::TecXChainInsufCreateAmount,
    TransactionResult::TecXChainAccountCreatePastSeq,
    TransactionResult::TecXChainAccountCreateTooMany,
    TransactionResult::TecXChainPaymentFailed,
    TransactionResult::TecXChainSelfCommit,
    TransactionResult::TecXChainBadPublicKeyAccountPair,
    TransactionResult::TecXChainCreateAccountDisabled,
    TransactionResult::TecEmptyDID,
    TransactionResult::TecInvalidUpdateTime,
    TransactionResult::TecTokenPairNotFound,
    TransactionResult::TecArrayEmpty,
    TransactionResult::TecArrayTooLarge,
    TransactionResult::TecLocked,
    TransactionResult::TecBadCredential,
    TransactionResult::TecWrongAsset,
    TransactionResult::TecLimitExceeded,
    TransactionResult::TecPseudoAccount,
    TransactionResult::TecPrecisionLoss,
    TransactionResult::TecNoDelegatePermission,
    // tef
    TransactionResult::TefFailure,
    TransactionResult::TefAlreadyMaster,
    TransactionResult::TefBadAddOrDrop,
    TransactionResult::TefBadAuth,
    TransactionResult::TefBadLedger,
    TransactionResult::TefCreated,
    TransactionResult::TefException,
    TransactionResult::TefInternal,
    TransactionResult::TefNoAuthRequired,
    TransactionResult::TefPastSeq,
    TransactionResult::TefWrongPrior,
    TransactionResult::TefMasterDisabled,
    TransactionResult::TefMaxLedger,
    TransactionResult::TefBadSignature,
    TransactionResult::TefBadQuorum,
    TransactionResult::TefNotMultiSigning,
    TransactionResult::TefBadAuthMaster,
    TransactionResult::TefInvariantFailed,
    TransactionResult::TefTooBig,
    TransactionResult::TefNoTicket,
    TransactionResult::TefNFTokenIsNotTransferable,
    TransactionResult::TefInvalidLedgerFixType,
    // ter
    TransactionResult::TerRetry,
    TransactionResult::TerFundsSpent,
    TransactionResult::TerInsufFee,
    TransactionResult::TerNoAccount,
    TransactionResult::TerNoAuth,
    TransactionResult::TerNoLine,
    TransactionResult::TerOwners,
    TransactionResult::TerPreSeq,
    TransactionResult::TerLastLedger,
    TransactionResult::TerNoRipple,
    TransactionResult::TerQueueFull,
    TransactionResult::TerPreTicket,
    TransactionResult::TerNoAmm,
    TransactionResult::TerAddressCollision,
    TransactionResult::TerNoDelegatePermission,
    // tem
    TransactionResult::TemMalformed,
    TransactionResult::TemBadAmount,
    TransactionResult::TemBadCurrency,
    TransactionResult::TemBadExpiration,
    TransactionResult::TemBadFee,
    TransactionResult::TemBadIssuer,
    TransactionResult::TemBadLimit,
    TransactionResult::TemBadOffer,
    TransactionResult::TemBadPath,
    TransactionResult::TemBadPathLoop,
    TransactionResult::TemBadRegKey,
    TransactionResult::TemBadSendXrpLimit,
    TransactionResult::TemBadSendXrpMax,
    TransactionResult::TemBadSendXrpNoDir,
    TransactionResult::TemBadSendXrpPartial,
    TransactionResult::TemBadSendXrpPaths,
    TransactionResult::TemBadSequence,
    TransactionResult::TemBadSignature,
    TransactionResult::TemBadSrc,
    TransactionResult::TemBadTransferRate,
    TransactionResult::TemDstIsSrc,
    TransactionResult::TemDstNeeded,
    TransactionResult::TemInvalid,
    TransactionResult::TemInvalidFlag,
    TransactionResult::TemRedundant,
    TransactionResult::TemRippleRedundant,
    TransactionResult::TemDisabled,
    TransactionResult::TemBadSigner,
    TransactionResult::TemBadQuorum,
    TransactionResult::TemBadWeight,
    TransactionResult::TemBadTickSize,
    TransactionResult::TemInvalidAccountId,
    TransactionResult::TemCannotPreAuthSelf,
    TransactionResult::TemInvalidCount,
    TransactionResult::TemUncertain,
    TransactionResult::TemUnknown,
    TransactionResult::TemSeqAndTicket,
    TransactionResult::TemBadNFTokenTransfer,
    TransactionResult::TemBadAmmTokens,
    TransactionResult::TemXChainEqualDoorAccounts,
    TransactionResult::TemXChainBadProof,
    TransactionResult::TemXChainBridge,
    TransactionResult::TemXChainBridgeNondoorOwner,
    TransactionResult::TemXChainBridgeBadMinAccountCreateAmount,
    TransactionResult::TemXChainBridgeBadRewardAmount,
    TransactionResult::TemEmptyDid,
    TransactionResult::TemArrayEmpty,
    TransactionResult::TemArrayTooLarge,
    TransactionResult::TemBadTransferFee,
    TransactionResult::TemInvalidInnerBatch,
    // tel
    TransactionResult::TelLocalError,
    TransactionResult::TelBadDomain,
    TransactionResult::TelBadPathCount,
    TransactionResult::TelBadPublicKey,
    TransactionResult::TelFailedProcessing,
    TransactionResult::TelInsufFeeP,
    TransactionResult::TelNoDstPartial,
    TransactionResult::TelCanNotQueue,
    TransactionResult::TelCanNotQueueBalance,
    TransactionResult::TelCanNotQueueBlocks,
    TransactionResult::TelCanNotQueueBlocked,
    TransactionResult::TelCanNotQueueFee,
    TransactionResult::TelCanNotQueueFull,
    TransactionResult::TelWrongNetwork,
    TransactionResult::TelRequiresNetworkId,
    TransactionResult::TelNetworkIdMakesTxNonCanonical,
    TransactionResult::TelEnvRpcFailed,
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
        assert_eq!(r.code(), 124);
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

    #[test]
    fn tec_codes_match_definitions() {
        // Verify key tec codes match rippled definitions.json
        assert_eq!(TransactionResult::TecInsufReserveLine.code(), 122);
        assert_eq!(TransactionResult::TecInsufReserveOffer.code(), 123);
        assert_eq!(TransactionResult::TecInsufFee.code(), 136);
        assert_eq!(TransactionResult::TecFrozen.code(), 137);
        assert_eq!(TransactionResult::TecInternalError.code(), 144);
        assert_eq!(TransactionResult::TecInvariantFailed.code(), 147);
        assert_eq!(TransactionResult::TecObjectNotFound.code(), 160);
        assert_eq!(TransactionResult::TecXChainBadTransferIssue.code(), 170);
        assert_eq!(TransactionResult::TecEmptyDID.code(), 187);
        assert_eq!(TransactionResult::TecNoDelegatePermission.code(), 198);
    }

    #[test]
    fn tef_codes_match_definitions() {
        assert_eq!(TransactionResult::TefBadLedger.code(), -195);
        assert_eq!(TransactionResult::TefCreated.code(), -194);
        assert_eq!(TransactionResult::TefMasterDisabled.code(), -188);
        assert_eq!(TransactionResult::TefBadSignature.code(), -186);
        assert_eq!(TransactionResult::TefBadAuthMaster.code(), -183);
        assert_eq!(TransactionResult::TefInvalidLedgerFixType.code(), -178);
    }
}
