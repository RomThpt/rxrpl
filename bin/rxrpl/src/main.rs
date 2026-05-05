use std::path::PathBuf;

use clap::Parser;
use rxrpl::Wallet;
use rxrpl_cli::{
    Cli, Commands, LogLevel, MetricsExport, RunMode, default_metrics_url, prometheus_to_json,
    resolve_run_mode,
};
use serde_json::Value;

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
            if let Some(s) = parsed_starting.as_ref() {
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

        // -- I-B3 operator commands --
        Commands::Peers { json } => {
            return cmd_peers(&cli.url, json).await;
        }
        Commands::Validators { json } => {
            return cmd_validators(&cli.url, json).await;
        }
        Commands::Metrics { endpoint, export } => {
            let target = endpoint.unwrap_or_else(|| default_metrics_url(&cli.url));
            return cmd_metrics(&target, export).await;
        }
        Commands::ConfigValidate { path } => {
            let resolved = path.or(cli.config.clone());
            return cmd_config_validate(resolved.as_ref());
        }

        _ => {}
    }

    // All other commands use HTTP
    let client = rxrpl::ClientBuilder::new(&cli.url).build_http()?;

    let result: Value = match cli.command {
        Commands::ServerInfo { .. } => client.server_info().await?,
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
        | Commands::AccountDelete { .. }
        | Commands::Peers { .. }
        | Commands::Validators { .. }
        | Commands::Metrics { .. }
        | Commands::ConfigValidate { .. } => unreachable!(),
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

// -- I-B3 operator subcommand handlers --

async fn cmd_peers(url: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let client = rxrpl::ClientBuilder::new(url).build_http()?;
    let result: Value = client.request("peers", serde_json::json!({})).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let peers = result["peers"].as_array().cloned().unwrap_or_default();
    let count = result["peer_count"].as_u64().unwrap_or(peers.len() as u64);
    println!("peer_count: {count}");
    if peers.is_empty() {
        println!("(no peers connected)");
        return Ok(());
    }
    println!("{:<40} {:<12} {:<10}", "address", "ledger", "state");
    for p in peers {
        let addr = p["address"].as_str().unwrap_or("-");
        let ledger = p["ledger"].as_u64().unwrap_or(0);
        let state = p["state"].as_str().unwrap_or("-");
        println!("{addr:<40} {ledger:<12} {state:<10}");
    }
    Ok(())
}

async fn cmd_validators(url: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let client = rxrpl::ClientBuilder::new(url).build_http()?;
    let result: Value = client.request("validators", serde_json::json!({})).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let quorum = result["validation_quorum"].as_u64().unwrap_or(0);
    let trusted = result["trusted_validator_keys"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let publishers = result["publisher_lists"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    println!("validation_quorum:        {quorum}");
    println!("trusted_validator_keys:   {trusted}");
    println!("publisher_lists:          {publishers}");
    Ok(())
}

async fn cmd_metrics(url: &str, export: MetricsExport) -> Result<(), Box<dyn std::error::Error>> {
    let body = reqwest::get(url).await?.error_for_status()?.text().await?;
    match export {
        MetricsExport::Prometheus => {
            print!("{body}");
        }
        MetricsExport::Json => {
            let v = prometheus_to_json(&body);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
    }
    Ok(())
}

fn cmd_config_validate(path: Option<&PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let path = path.ok_or("no config path provided (use --config or pass a path)")?;
    match rxrpl_config::load_config(path) {
        Ok(cfg) => {
            println!("OK: {}", path.display());
            println!("  server.bind:       {}", cfg.server.bind);
            println!("  peer.port:         {}", cfg.peer.port);
            println!("  database.backend:  {}", cfg.database.backend);
            println!("  network.id:        {}", cfg.network.network_id);
            Ok(())
        }
        Err(e) => Err(format!("config invalid ({}): {e}", path.display()).into()),
    }
}
