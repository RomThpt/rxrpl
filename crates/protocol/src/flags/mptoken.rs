// MPTokenIssuanceCreate transaction flags
pub const TF_MPT_CAN_LOCK: u32 = 0x0002;
pub const TF_MPT_REQUIRE_AUTH: u32 = 0x0004;
pub const TF_MPT_CAN_ESCROW: u32 = 0x0008;
pub const TF_MPT_CAN_TRADE: u32 = 0x0010;
pub const TF_MPT_CAN_TRANSFER: u32 = 0x0020;
pub const TF_MPT_CAN_CLAWBACK: u32 = 0x0040;

// MPTokenIssuance ledger entry flags
pub const LSFT_MPT_LOCKED: u32 = 0x0001;
pub const LSFT_MPT_CAN_LOCK: u32 = 0x0002;
pub const LSFT_MPT_REQUIRE_AUTH: u32 = 0x0004;
pub const LSFT_MPT_CAN_ESCROW: u32 = 0x0008;
pub const LSFT_MPT_CAN_TRADE: u32 = 0x0010;
pub const LSFT_MPT_CAN_TRANSFER: u32 = 0x0020;
pub const LSFT_MPT_CAN_CLAWBACK: u32 = 0x0040;

// MPToken ledger entry flags
pub const LSFT_MPT_AUTHORIZED: u32 = 0x0002;

// MPTokenAuthorize transaction flags
pub const TF_MPT_UNAUTHORIZE: u32 = 0x0001;
