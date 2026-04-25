use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use rxrpl::Wallet;
use serde_json::Value;

/// Log verbosity levels.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        };
        f.write_str(s)
    }
}

#[derive(Parser)]
#[command(
    name = "rxrpl",
    about = "XRPL node and client toolchain",
    version,
    long_about = "rxrpl -- a Rust implementation of the XRP Ledger protocol.\n\n\
        Run a standalone or networked XRPL node, query ledger data,\n\
        manage wallets, and submit transactions from the command line."
)]
struct Cli {
    /// XRPL node URL for client commands
    #[arg(long, default_value = "https://s1.ripple.com:51234")]
    url: String,

    /// Path to configuration file (TOML)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Log level
    #[arg(short, long, global = true, default_value = "info", value_enum)]
    log_level: LogLevel,

    /// Data directory override
    #[arg(short = 'D', long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

/// Node run modes.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum RunMode {
    /// Standalone single-node mode (no P2P)
    Standalone,
    /// Networked mode with P2P overlay
    Network,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an XRPL node (standalone or networked)
    Run {
        /// Node run mode
        #[arg(short, long, default_value = "standalone", value_enum)]
        mode: RunMode,

        /// Genesis account address
        #[arg(long, default_value = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh")]
        genesis_account: String,

        /// Ledger close interval in seconds
        #[arg(long, default_value = "10")]
        close_interval: u64,

        /// RPC server bind address (standalone mode)
        #[arg(long, default_value = "127.0.0.1:5005")]
        bind: String,

        /// RPC URL of a trusted node to sync from (network mode)
        #[arg(long)]
        sync_rpc: Option<String>,

        /// Path to the persistent node store (RocksDB). Implies
        /// `backend = "rocksdb"` unless explicitly overridden in the config.
        /// If unset, the configured backend (default: memory) is used.
        #[arg(long)]
        db_path: Option<PathBuf>,

        /// Bootstrap from a checkpoint instead of replaying from genesis.
        /// Accepts:
        ///   - a sequence number (decimal, e.g. `90000000`)
        ///   - a 64-char hex ledger hash
        ///   - the literal `recent` to anchor at "tip - 1024"
        /// Requires `validator_list_sites` + `validator_list_keys` to
        /// establish a UNL-quorum trust anchor for the chosen ledger.
        #[arg(long)]
        starting_ledger: Option<String>,

        /// Shorthand for --mode standalone
        #[arg(long, conflicts_with = "network")]
        standalone: bool,

        /// Shorthand for --mode network
        #[arg(long, conflicts_with = "standalone")]
        network: bool,

        /// Run in reporting mode (read-only, no consensus)
        #[arg(long, conflicts_with_all = ["standalone", "network"])]
        reporting: bool,
    },

    /// Print version information and exit
    Version,

    // -- Server Queries --
    /// Get server info from an XRPL node
    ServerInfo,

    /// Get current fee info
    Fee,

    /// Get the latest validated ledger
    LedgerClosed,

    /// Get a ledger by index
    Ledger {
        /// Ledger index or shortcut (validated, current, closed)
        index: String,
    },

    /// Subscribe to streams (WebSocket only)
    Subscribe {
        /// Streams to subscribe to (e.g., ledger, transactions)
        #[arg(value_delimiter = ',')]
        streams: Vec<String>,
    },

    // -- Account Queries --
    /// Get account info
    AccountInfo {
        /// Account address (classic or X-address)
        account: String,
    },

    /// Get account transaction history
    AccountTx {
        /// Account address
        account: String,
        /// Maximum number of transactions
        #[arg(long, default_value = "10")]
        limit: u32,
    },

    /// List NFTs for an account
    AccountNfts {
        /// Account address
        account: String,
    },

    // -- Wallet --
    /// Generate a new wallet keypair locally
    WalletPropose {
        /// Key type: ed25519 or secp256k1
        #[arg(long, default_value = "ed25519")]
        key_type: String,
    },

    // -- Transactions --
    /// Submit a signed transaction blob
    Submit {
        /// Hex-encoded transaction blob
        tx_blob: String,
    },

    /// Look up a transaction by hash
    Tx {
        /// Transaction hash
        hash: String,
    },

    /// Sign a transaction from JSON (inline or @file)
    Sign {
        /// Secret seed (sXXX format)
        #[arg(long)]
        seed: String,
        /// Transaction JSON (inline string or @path/to/file.json)
        #[arg(long)]
        tx: String,
        /// Key type: ed25519 or secp256k1
        #[arg(long, default_value = "ed25519")]
        key_type: String,
    },

    /// Send an XRP payment (build, autofill, sign, submit)
    Pay {
        /// Sender secret seed (sXXX format)
        #[arg(long)]
        from: String,
        /// Destination address (rXXX format)
        #[arg(long)]
        to: String,
        /// Amount in drops
        #[arg(long)]
        amount: u64,
        /// Fee in drops (auto-filled if omitted)
        #[arg(long)]
        fee: Option<String>,
        /// Key type: ed25519 or secp256k1
        #[arg(long, default_value = "ed25519")]
        key_type: String,
    },

    /// Set a trust line (autofill, sign, submit)
    TrustSet {
        /// Sender secret seed (sXXX format)
        #[arg(long)]
        from: String,
        /// Issuer address (rXXX format)
        #[arg(long)]
        issuer: String,
        /// Currency code (e.g., USD)
        #[arg(long)]
        currency: String,
        /// Trust line limit
        #[arg(long)]
        limit: String,
        /// Fee in drops (auto-filled if omitted)
        #[arg(long)]
        fee: Option<String>,
    },

    /// Create an offer (autofill, sign, submit)
    OfferCreate {
        /// Sender secret seed (sXXX format)
        #[arg(long)]
        from: String,
        /// What the taker gets: drops or value/currency/issuer
        #[arg(long)]
        taker_gets: String,
        /// What the taker pays: drops or value/currency/issuer
        #[arg(long)]
        taker_pays: String,
        /// Fee in drops (auto-filled if omitted)
        #[arg(long)]
        fee: Option<String>,
    },

    /// Delete an account (autofill, sign, submit)
    AccountDelete {
        /// Sender secret seed (sXXX format)
        #[arg(long)]
        from: String,
        /// Destination for remaining XRP (rXXX format)
        #[arg(long)]
        destination: String,
        /// Fee in drops (auto-filled if omitted)
        #[arg(long)]
        fee: Option<String>,
    },
}

fn setup_logging(level: LogLevel) {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.to_string()));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    setup_logging(cli.log_level);

    if let Err(e) = run(cli).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn parse_key_type(s: &str) -> Result<rxrpl::KeyType, Box<dyn std::error::Error>> {
    match s {
        "ed25519" => Ok(rxrpl::KeyType::Ed25519),
        "secp256k1" => Ok(rxrpl::KeyType::Secp256k1),
        other => Err(format!("unknown key type: {other}").into()),
    }
}

/// Parse an amount string: plain integer = XRP drops, `value/currency/issuer` = IOU.
fn parse_amount(s: &str) -> Result<Value, Box<dyn std::error::Error>> {
    if let Some((value, rest)) = s.split_once('/') {
        let (currency, issuer) = rest
            .split_once('/')
            .ok_or_else(|| format!("invalid IOU format, expected value/currency/issuer: {s}"))?;
        Ok(serde_json::json!({
            "value": value,
            "currency": currency,
            "issuer": issuer
        }))
    } else {
        Ok(Value::String(s.to_string()))
    }
}

/// Resolve the effective run mode from the `--mode`, `--standalone`, and `--network` flags.
fn resolve_run_mode(mode: RunMode, standalone: bool, network: bool) -> RunMode {
    if standalone {
        RunMode::Standalone
    } else if network {
        RunMode::Network
    } else {
        mode
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Version => {
            println!(
                "{} {}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }

        Commands::Run {
            mode,
            genesis_account,
            close_interval,
            bind,
            sync_rpc,
            db_path,
            starting_ledger,
            standalone,
            network,
            reporting,
        } => {
            // Validate (without resolving — actual lookup happens on the
            // running node once the UNL has produced a trust anchor).
            let parsed_starting = starting_ledger
                .as_deref()
                .map(rxrpl_node::StartingLedger::parse)
                .transpose()
                .map_err(|e| {
                    eprintln!("Error: --starting-ledger: {e}");
                    std::process::exit(2)
                })
                .unwrap();
            if let Some(s) = parsed_starting {
                tracing::info!("checkpoint bootstrap requested: {:?}", s);
            }
            let mut config = if let Some(ref config_path) = cli.config {
                rxrpl_config::load_config(config_path)?
            } else {
                rxrpl_config::NodeConfig::default()
            };

            if let Some(ref dir) = cli.data_dir {
                config.database.path = dir.clone();
            }

            // --db-path implies a persistent backend unless the config
            // already set one explicitly to something other than the default.
            if let Some(ref path) = db_path {
                config.database.path = path.clone();
                if config.database.backend == "memory" {
                    config.database.backend = "rocksdb".into();
                }
            }

            if reporting {
                config.reporting.enabled = true;
                config.server.bind = bind.parse()?;
                return cmd_reporting_run(config).await;
            }

            let effective_mode = resolve_run_mode(mode, standalone, network);

            match effective_mode {
                RunMode::Standalone => {
                    config.server.bind = bind.parse()?;
                    return cmd_node_run(config, &genesis_account, close_interval).await;
                }
                RunMode::Network => {
                    // Only bootstrap from an external RPC when explicitly
                    // requested via --sync-rpc. The default --url is for
                    // client commands; nodes with fixed_peers can sync via
                    // P2P alone (private/test networks like xrpl-hive).
                    return cmd_network_run(
                        config,
                        &genesis_account,
                        close_interval,
                        sync_rpc.as_deref(),
                        parsed_starting,
                    )
                    .await;
                }
            }
        }

        Commands::WalletPropose { key_type } => {
            let kt = parse_key_type(&key_type)?;
            let wallet = Wallet::generate(kt);
            let seed_encoded = wallet.seed_encoded()?;

            let result = serde_json::json!({
                "account_id": wallet.address,
                "key_type": key_type,
                "master_seed": seed_encoded,
                "public_key_hex": hex::encode_upper(wallet.public_key.as_bytes()),
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }

        Commands::Subscribe { streams } => {
            let ws_url = if cli.url.starts_with("http") {
                cli.url
                    .replace("https://", "wss://")
                    .replace("http://", "ws://")
            } else {
                cli.url.clone()
            };

            let client = rxrpl::ClientBuilder::new(&ws_url).build_ws().await?;

            let result = client.subscribe(streams.clone()).await?;
            println!("Subscribed: {}", serde_json::to_string_pretty(&result)?);

            if let Some(mut stream) = client.subscription_stream() {
                loop {
                    match stream.next().await {
                        Ok(event) => {
                            println!("{}", serde_json::to_string_pretty(&event)?);
                        }
                        Err(e) => {
                            eprintln!("Stream error: {e}");
                            break;
                        }
                    }
                }
            }
            return Ok(());
        }

        Commands::Sign { seed, tx, key_type } => {
            return cmd_sign(&seed, &tx, &key_type);
        }

        Commands::Pay {
            from,
            to,
            amount,
            fee,
            key_type,
        } => {
            return cmd_pay(&cli.url, &from, &to, amount, fee.as_deref(), &key_type).await;
        }

        Commands::TrustSet {
            from,
            issuer,
            currency,
            limit,
            fee,
        } => {
            return cmd_trust_set(&cli.url, &from, &issuer, &currency, &limit, fee.as_deref())
                .await;
        }

        Commands::OfferCreate {
            from,
            taker_gets,
            taker_pays,
            fee,
        } => {
            return cmd_offer_create(&cli.url, &from, &taker_gets, &taker_pays, fee.as_deref())
                .await;
        }

        Commands::AccountDelete {
            from,
            destination,
            fee,
        } => {
            return cmd_account_delete(&cli.url, &from, &destination, fee.as_deref()).await;
        }

        _ => {}
    }

    // All other commands use HTTP
    let client = rxrpl::ClientBuilder::new(&cli.url).build_http()?;

    let result: Value = match cli.command {
        Commands::ServerInfo => client.server_info().await?,
        Commands::AccountInfo { account } => client.account_info(&account).await?,
        Commands::AccountTx { account, limit } => client.account_tx(&account, Some(limit)).await?,
        Commands::Submit { tx_blob } => client.submit(&tx_blob).await?,
        Commands::Tx { hash } => client.tx(&hash).await?,
        Commands::Fee => client.fee().await?,
        Commands::LedgerClosed => client.ledger_closed().await?,
        Commands::Ledger { index } => client.ledger(&index).await?,
        Commands::AccountNfts { account } => client.account_nfts(&account).await?,
        Commands::Version
        | Commands::Run { .. }
        | Commands::WalletPropose { .. }
        | Commands::Subscribe { .. }
        | Commands::Sign { .. }
        | Commands::Pay { .. }
        | Commands::TrustSet { .. }
        | Commands::OfferCreate { .. }
        | Commands::AccountDelete { .. } => unreachable!(),
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn cmd_sign(
    seed_str: &str,
    tx_input: &str,
    key_type_str: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let kt = parse_key_type(key_type_str)?;
    let wallet = Wallet::from_seed_with_type(seed_str, kt)?;

    let tx_json: Value = if let Some(path) = tx_input.strip_prefix('@') {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::from_str(tx_input)?
    };

    let (blob, hash) = wallet.sign_and_serialize(&tx_json)?;

    let result = serde_json::json!({
        "tx_blob": blob,
        "hash": hash.to_string(),
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn cmd_pay(
    url: &str,
    seed_str: &str,
    to: &str,
    amount: u64,
    fee: Option<&str>,
    key_type_str: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let kt = parse_key_type(key_type_str)?;
    let wallet = Wallet::from_seed_with_type(seed_str, kt)?;

    let mut tx_json = serde_json::json!({
        "TransactionType": "Payment",
        "Account": wallet.address,
        "Destination": to,
        "Amount": amount.to_string(),
    });

    if let Some(f) = fee {
        tx_json["Fee"] = Value::String(f.to_string());
    }

    autofill_sign_submit(url, &wallet, &mut tx_json).await
}

async fn cmd_trust_set(
    url: &str,
    seed_str: &str,
    issuer: &str,
    currency: &str,
    limit: &str,
    fee: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::from_seed(seed_str)?;

    let mut tx_json = serde_json::json!({
        "TransactionType": "TrustSet",
        "Account": wallet.address,
        "LimitAmount": {
            "currency": currency,
            "issuer": issuer,
            "value": limit
        }
    });

    if let Some(f) = fee {
        tx_json["Fee"] = Value::String(f.to_string());
    }

    autofill_sign_submit(url, &wallet, &mut tx_json).await
}

async fn cmd_offer_create(
    url: &str,
    seed_str: &str,
    taker_gets: &str,
    taker_pays: &str,
    fee: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::from_seed(seed_str)?;

    let mut tx_json = serde_json::json!({
        "TransactionType": "OfferCreate",
        "Account": wallet.address,
        "TakerGets": parse_amount(taker_gets)?,
        "TakerPays": parse_amount(taker_pays)?,
    });

    if let Some(f) = fee {
        tx_json["Fee"] = Value::String(f.to_string());
    }

    autofill_sign_submit(url, &wallet, &mut tx_json).await
}

async fn cmd_account_delete(
    url: &str,
    seed_str: &str,
    destination: &str,
    fee: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::from_seed(seed_str)?;

    let mut tx_json = serde_json::json!({
        "TransactionType": "AccountDelete",
        "Account": wallet.address,
        "Destination": destination,
    });

    if let Some(f) = fee {
        tx_json["Fee"] = Value::String(f.to_string());
    }

    autofill_sign_submit(url, &wallet, &mut tx_json).await
}

/// Shared autofill -> sign -> submit -> wait pipeline for transaction commands.
async fn autofill_sign_submit(
    url: &str,
    wallet: &Wallet,
    tx_json: &mut Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = rxrpl::ClientBuilder::new(url).build_http()?;
    rxrpl::protocol::tx::autofill::autofill(tx_json, &client).await?;

    let (blob, hash) = wallet.sign_and_serialize(tx_json)?;
    let hash_hex = hash.to_string();

    eprintln!("Submitting tx {hash_hex}...");

    let result = client.submit_and_wait(&blob, &hash_hex, 30).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn cmd_node_run(
    config: rxrpl_config::NodeConfig,
    genesis_account: &str,
    close_interval: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind = config.server.bind;
    let node = rxrpl_node::Node::new_standalone(config, genesis_account)?;

    eprintln!("Starting standalone node...");
    eprintln!("  Genesis account: {genesis_account}");
    eprintln!("  RPC server: http://{bind}");
    eprintln!("  Close interval: {close_interval}s");

    node.run_standalone(close_interval).await?;
    Ok(())
}

async fn cmd_network_run(
    config: rxrpl_config::NodeConfig,
    genesis_account: &str,
    close_interval: u64,
    sync_rpc_url: Option<&str>,
    starting_ledger: Option<rxrpl_node::StartingLedger>,
) -> Result<(), Box<dyn std::error::Error>> {
    let node = rxrpl_node::Node::new_standalone(config, genesis_account)?;

    eprintln!("Starting networked node...");
    eprintln!("  Genesis account: {genesis_account}");
    eprintln!("  Close interval: {close_interval}s");
    match sync_rpc_url {
        Some(url) => eprintln!("  Sync RPC: {url}"),
        None => eprintln!("  Sync RPC: <none — discover via P2P>"),
    }
    if let Some(s) = starting_ledger {
        eprintln!("  Starting ledger: {s:?}");
    }

    node.run_networked(close_interval, sync_rpc_url, starting_ledger)
        .await?;
    Ok(())
}

async fn cmd_reporting_run(
    config: rxrpl_config::NodeConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind = config.server.bind;
    let etl_source = config.reporting.etl_source.clone();
    let forward_url = config.reporting.forward_url.clone();

    let node = rxrpl_node::Node::new(config)?;

    eprintln!("Starting reporting-mode node...");
    eprintln!("  RPC server: http://{bind}");
    eprintln!("  ETL source: {etl_source}");
    eprintln!("  Forward URL: {forward_url}");

    node.run_reporting().await?;
    Ok(())
}
