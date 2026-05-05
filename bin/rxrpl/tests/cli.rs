//! Parser-level tests for the I-B3 operator subcommands.

use clap::Parser;
use rxrpl_cli::{Cli, Commands, MetricsExport, default_metrics_url, prometheus_to_json};

fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["rxrpl"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).expect("CLI parse failed")
}

// -- subcommand parsing --

#[test]
fn peers_subcommand_default() {
    let cli = parse(&["peers"]);
    match cli.command {
        Commands::Peers { json } => assert!(!json),
        _ => panic!("expected Commands::Peers"),
    }
}

#[test]
fn peers_subcommand_json_flag() {
    let cli = parse(&["peers", "--json"]);
    match cli.command {
        Commands::Peers { json } => assert!(json),
        _ => panic!("expected Commands::Peers"),
    }
}

#[test]
fn server_info_accepts_json_flag() {
    let cli = parse(&["server-info", "--json"]);
    match cli.command {
        Commands::ServerInfo { json } => assert!(json),
        _ => panic!("expected Commands::ServerInfo"),
    }
}

#[test]
fn validators_subcommand_default() {
    let cli = parse(&["validators"]);
    match cli.command {
        Commands::Validators { json } => assert!(!json),
        _ => panic!("expected Commands::Validators"),
    }
}

#[test]
fn metrics_subcommand_default_export() {
    let cli = parse(&["metrics"]);
    match cli.command {
        Commands::Metrics { endpoint, export } => {
            assert!(endpoint.is_none());
            assert_eq!(export, MetricsExport::Prometheus);
        }
        _ => panic!("expected Commands::Metrics"),
    }
}

#[test]
fn metrics_subcommand_export_json() {
    let cli = parse(&["metrics", "--export", "json"]);
    match cli.command {
        Commands::Metrics { export, .. } => assert_eq!(export, MetricsExport::Json),
        _ => panic!("expected Commands::Metrics"),
    }
}

#[test]
fn config_validate_with_path() {
    let cli = parse(&["config-validate", "/tmp/nope.toml"]);
    match cli.command {
        Commands::ConfigValidate { path } => {
            assert_eq!(path.unwrap().to_str().unwrap(), "/tmp/nope.toml");
        }
        _ => panic!("expected Commands::ConfigValidate"),
    }
}

#[test]
fn config_validate_without_path() {
    let cli = parse(&["config-validate"]);
    match cli.command {
        Commands::ConfigValidate { path } => assert!(path.is_none()),
        _ => panic!("expected Commands::ConfigValidate"),
    }
}

// -- helpers --

#[test]
fn default_metrics_url_strips_path_and_appends_metrics() {
    assert_eq!(
        default_metrics_url("http://127.0.0.1:5005"),
        "http://127.0.0.1:5005/metrics"
    );
    assert_eq!(
        default_metrics_url("https://s1.ripple.com:51234/some/path"),
        "https://s1.ripple.com:51234/metrics"
    );
    // No scheme -> just append.
    assert_eq!(default_metrics_url("localhost:5005"), "localhost:5005/metrics");
}

#[test]
fn prometheus_to_json_parses_simple_lines() {
    let body = "\
# HELP foo a counter
# TYPE foo counter
foo 42
bar{a=\"x\",b=\"y\"} 3.14
";
    let v = prometheus_to_json(body);
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "foo");
    assert_eq!(arr[0]["value"], 42.0);
    assert_eq!(arr[1]["name"], "bar");
    assert_eq!(arr[1]["labels"]["a"], "x");
    assert_eq!(arr[1]["labels"]["b"], "y");
    assert_eq!(arr[1]["value"], 3.14);
}

#[test]
fn prometheus_to_json_skips_unparseable_lines() {
    let body = "garbage line\nfoo 7\n";
    let arr = prometheus_to_json(body);
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "foo");
}
