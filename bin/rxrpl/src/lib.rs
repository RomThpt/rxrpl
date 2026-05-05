//! Library entry point for the `rxrpl` CLI.
//!
//! The full command implementations live in `main.rs`; this lib exposes
//! the clap parser types plus a few pure helpers so they can be unit
//! tested without spawning the binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Log verbosity levels.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LogLevel {
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

/// Node run modes.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RunMode {
    /// Standalone single-node mode (no P2P)
    Standalone,
    /// Networked mode with P2P overlay
    Network,
}

/// Output format for `metrics`.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum MetricsExport {
    /// Native Prometheus text exposition format (default).
    Prometheus,
    /// Convert each `name{labels} value` line into a JSON object.
    Json,
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
pub struct Cli {
    /// XRPL node URL for client commands
    #[arg(long, default_value = "https://s1.ripple.com:51234")]
    pub url: String,

    /// Path to configuration file (TOML)
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Log level
    #[arg(short, long, global = true, default_value = "info", value_enum)]
    pub log_level: LogLevel,

    /// Data directory override
    #[arg(short = 'D', long, global = true)]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
        /// Path to the persistent node store (RocksDB).
        #[arg(long)]
        db_path: Option<PathBuf>,
        /// Bootstrap from a checkpoint instead of replaying from genesis.
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
    ServerInfo {
        /// Pretty-print JSON only (no human header).
        #[arg(long, short = 'j')]
        json: bool,
    },

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
        #[arg(value_delimiter = ',')]
        streams: Vec<String>,
    },

    // -- Account Queries --
    AccountInfo {
        account: String,
    },
    AccountTx {
        account: String,
        #[arg(long, default_value = "10")]
        limit: u32,
    },
    AccountNfts {
        account: String,
    },

    /// Generate a new wallet keypair locally
    WalletPropose {
        #[arg(long, default_value = "ed25519")]
        key_type: String,
    },

    Submit {
        tx_blob: String,
    },
    Tx {
        hash: String,
    },
    Sign {
        #[arg(long)]
        seed: String,
        #[arg(long)]
        tx: String,
        #[arg(long, default_value = "ed25519")]
        key_type: String,
    },
    Pay {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        fee: Option<String>,
        #[arg(long, default_value = "ed25519")]
        key_type: String,
    },
    TrustSet {
        #[arg(long)]
        from: String,
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        currency: String,
        #[arg(long)]
        limit: String,
        #[arg(long)]
        fee: Option<String>,
    },
    OfferCreate {
        #[arg(long)]
        from: String,
        #[arg(long)]
        taker_gets: String,
        #[arg(long)]
        taker_pays: String,
        #[arg(long)]
        fee: Option<String>,
    },
    AccountDelete {
        #[arg(long)]
        from: String,
        #[arg(long)]
        destination: String,
        #[arg(long)]
        fee: Option<String>,
    },

    // -- Operator (I-B3) --
    /// List currently connected peers (calls `peers` RPC).
    Peers {
        /// Output raw JSON instead of a table.
        #[arg(long, short = 'j')]
        json: bool,
    },

    /// Show validator quorum status (calls `validators` RPC).
    Validators {
        /// Output raw JSON instead of a summary.
        #[arg(long, short = 'j')]
        json: bool,
    },

    /// Fetch the Prometheus metrics endpoint.
    Metrics {
        /// Override the metrics URL (defaults to `<--url host>/metrics`).
        #[arg(long)]
        endpoint: Option<String>,
        /// Output format.
        #[arg(long, default_value = "prometheus", value_enum)]
        export: MetricsExport,
    },

    /// Validate a TOML configuration file.
    ConfigValidate {
        /// Path to the TOML config (defaults to the global --config path).
        path: Option<PathBuf>,
    },
}

/// Resolve the effective run mode from the `--mode`, `--standalone`, and `--network` flags.
pub fn resolve_run_mode(mode: RunMode, standalone: bool, network: bool) -> RunMode {
    if standalone {
        RunMode::Standalone
    } else if network {
        RunMode::Network
    } else {
        mode
    }
}

/// Convert a Prometheus exposition body into a JSON array of
/// `{ name, labels, value }` records. Comment lines (`# ...`) and
/// blank lines are dropped. Lines that don't parse are skipped.
pub fn prometheus_to_json(body: &str) -> serde_json::Value {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `name{label="v",..} value` or `name value`
        let (left, value) = match line.rsplit_once(' ') {
            Some(t) => t,
            None => continue,
        };
        let value: f64 = match value.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let (name, labels) = if let Some(idx) = left.find('{') {
            let name = &left[..idx];
            let label_blob = left[idx..].trim_start_matches('{').trim_end_matches('}');
            let mut labels = serde_json::Map::new();
            for kv in label_blob.split(',') {
                if let Some((k, v)) = kv.split_once('=') {
                    let k = k.trim();
                    let v = v.trim().trim_matches('"');
                    if !k.is_empty() {
                        labels.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                    }
                }
            }
            (name.to_string(), serde_json::Value::Object(labels))
        } else {
            (
                left.to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            )
        };
        out.push(serde_json::json!({
            "name": name,
            "labels": labels,
            "value": value,
        }));
    }
    serde_json::Value::Array(out)
}

/// Compute the default `/metrics` URL from a JSON-RPC base URL.
///
/// Strips the path, keeps scheme + host (+ optional port), and appends `/metrics`.
pub fn default_metrics_url(rpc_url: &str) -> String {
    // Naïve but predictable: cut at the third `/` if scheme is present.
    if let Some(rest) = rpc_url
        .strip_prefix("http://")
        .or_else(|| rpc_url.strip_prefix("https://"))
    {
        let host = rest.split('/').next().unwrap_or(rest);
        let scheme = if rpc_url.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{host}/metrics")
    } else {
        format!("{}/metrics", rpc_url.trim_end_matches('/'))
    }
}
