use serde::{Deserialize, Serialize};

/// All 78 RPC methods supported by the XRPL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    // Account methods
    AccountInfo,
    AccountTx,
    AccountLines,
    AccountChannels,
    AccountObjects,
    AccountOffers,
    AccountCurrencies,
    AccountNfts,
    AccountNftsByIssuer,
    GatewayBalances,
    NorippleCheck,

    // Ledger methods
    Ledger,
    LedgerAccept,
    LedgerEntry,
    LedgerData,
    LedgerClosed,
    LedgerCurrent,

    // Transaction methods
    Tx,
    Submit,
    SubmitMultisigned,
    Sign,
    SignFor,
    TransactionEntry,
    Simulate,

    // Trading / AMM methods
    BookOffers,
    PathFind,
    RipplePathFind,
    AmmInfo,

    // Server methods
    ServerInfo,
    ServerState,
    ServerDefinitions,
    ServerSubscribe,
    Fee,
    Feature,
    Manifest,
    Ping,
    Random,

    // NFT methods
    NftBuyOffers,
    NftSellOffers,
    NftInfo,
    NftHistory,

    // Subscription methods
    Subscribe,
    Unsubscribe,

    // Channel methods
    ChannelAuthorize,
    ChannelVerify,

    // Utility methods
    WalletPropose,
    DepositAuthorized,
    GetAggregatePrice,

    // Admin methods
    Stop,
    Peers,
    Connect,
    LogLevel,
    Logrotate,
    Validators,
    ConsensusInfo,
    ValidationCreate,
    ValidationSeed,
    ValidatorInfo,
    PeerReservationsAdd,
    PeerReservationsDel,
    PeerReservationsList,
    ValidatorListSites,
    FetchInfo,
    Print,
    CanDelete,
    Crawl,
    TxReduceRelay,

    // Ledger management
    LedgerHeader,
    LedgerRequest,
    LedgerCleaner,
    LedgerDiff,
    LedgerRange,

    // Misc
    TxHistory,
    BookChanges,
    Json,
    Version,
    VaultInfo,
    Batch,
}

impl Method {
    /// Return the method name as used in JSON-RPC requests.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AccountInfo => "account_info",
            Self::AccountTx => "account_tx",
            Self::AccountLines => "account_lines",
            Self::AccountChannels => "account_channels",
            Self::AccountObjects => "account_objects",
            Self::AccountOffers => "account_offers",
            Self::AccountCurrencies => "account_currencies",
            Self::AccountNfts => "account_nfts",
            Self::AccountNftsByIssuer => "account_nfts_by_issuer",
            Self::GatewayBalances => "gateway_balances",
            Self::NorippleCheck => "noripple_check",
            Self::Ledger => "ledger",
            Self::LedgerAccept => "ledger_accept",
            Self::LedgerEntry => "ledger_entry",
            Self::LedgerData => "ledger_data",
            Self::LedgerClosed => "ledger_closed",
            Self::LedgerCurrent => "ledger_current",
            Self::Tx => "tx",
            Self::Submit => "submit",
            Self::SubmitMultisigned => "submit_multisigned",
            Self::Sign => "sign",
            Self::SignFor => "sign_for",
            Self::TransactionEntry => "transaction_entry",
            Self::Simulate => "simulate",
            Self::BookOffers => "book_offers",
            Self::PathFind => "path_find",
            Self::RipplePathFind => "ripple_path_find",
            Self::AmmInfo => "amm_info",
            Self::ServerInfo => "server_info",
            Self::ServerState => "server_state",
            Self::ServerDefinitions => "server_definitions",
            Self::ServerSubscribe => "server_subscribe",
            Self::Fee => "fee",
            Self::Feature => "feature",
            Self::Manifest => "manifest",
            Self::Ping => "ping",
            Self::Random => "random",
            Self::NftBuyOffers => "nft_buy_offers",
            Self::NftSellOffers => "nft_sell_offers",
            Self::NftInfo => "nft_info",
            Self::NftHistory => "nft_history",
            Self::Subscribe => "subscribe",
            Self::Unsubscribe => "unsubscribe",
            Self::ChannelAuthorize => "channel_authorize",
            Self::ChannelVerify => "channel_verify",
            Self::WalletPropose => "wallet_propose",
            Self::DepositAuthorized => "deposit_authorized",
            Self::GetAggregatePrice => "get_aggregate_price",
            Self::Stop => "stop",
            Self::Peers => "peers",
            Self::Connect => "connect",
            Self::LogLevel => "log_level",
            Self::Logrotate => "logrotate",
            Self::Validators => "validators",
            Self::ConsensusInfo => "consensus_info",
            Self::ValidationCreate => "validation_create",
            Self::ValidationSeed => "validation_seed",
            Self::ValidatorInfo => "validator_info",
            Self::PeerReservationsAdd => "peer_reservations_add",
            Self::PeerReservationsDel => "peer_reservations_del",
            Self::PeerReservationsList => "peer_reservations_list",
            Self::ValidatorListSites => "validator_list_sites",
            Self::FetchInfo => "fetch_info",
            Self::Print => "print",
            Self::CanDelete => "can_delete",
            Self::Crawl => "crawl",
            Self::TxReduceRelay => "tx_reduce_relay",
            Self::LedgerHeader => "ledger_header",
            Self::LedgerRequest => "ledger_request",
            Self::LedgerCleaner => "ledger_cleaner",
            Self::LedgerDiff => "ledger_diff",
            Self::LedgerRange => "ledger_range",
            Self::TxHistory => "tx_history",
            Self::BookChanges => "book_changes",
            Self::Json => "json",
            Self::Version => "version",
            Self::VaultInfo => "vault_info",
            Self::Batch => "batch",
        }
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
