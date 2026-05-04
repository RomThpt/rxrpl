# rxrpl Configuration Reference

This document describes every section and field of the rxrpl TOML
configuration, plus three ready-to-use templates under `config/`.

- `config/rxrpl-mainnet.toml` — production mainnet node (RocksDB).
- `config/rxrpl-testnet.toml` — altnet node.
- `config/rxrpl-standalone.toml` — local single-node sandbox.

Load order: rxrpl reads a single TOML file passed via `--config`
(default: `examples/node.toml` when invoked from the repo root). Any
field omitted from the file uses the built-in default listed below.

## Quick Start

```bash
# Mainnet
rxrpl run --config /etc/rxrpl/config.toml

# Testnet
rxrpl run --config config/rxrpl-testnet.toml

# Standalone (no P2P, in-memory)
rxrpl run --config config/rxrpl-standalone.toml --standalone
```

## Schema Overview

| Section       | Purpose                                              |
|---------------|------------------------------------------------------|
| `[server]`    | RPC / WebSocket listener + admin ACL.                |
| `[peer]`      | Peer protocol listen port, seeds, identity, TLS.     |
| `[database]`  | Storage backend, data path, history retention.       |
| `[validators]`| UNL trust + (optional) local validator role.         |
| `[network]`   | Network ID (mainnet/testnet/dev).                    |
| `[genesis]`   | Optional genesis ledger hash override.               |
| `[cluster]`   | Trusted-cluster (TMCluster) membership.              |
| `[reporting]` | Read-only reporting-mode ETL settings.               |

## `[server]`

| Field        | Type           | Default            | Description                                       |
|--------------|----------------|--------------------|---------------------------------------------------|
| `bind`       | `SocketAddr`   | `127.0.0.1:5005`   | RPC + WebSocket listener.                         |
| `admin_ips`  | `[String]`     | `["127.0.0.1"]`    | Source IPs allowed to call admin RPC methods.     |

The `/metrics` Prometheus endpoint and the WebSocket upgrade share the
`bind` address. Front with a reverse proxy if you need TLS termination
or per-route auth.

## `[peer]`

| Field          | Type        | Default                                       | Description                                                                 |
|----------------|-------------|-----------------------------------------------|-----------------------------------------------------------------------------|
| `port`         | `u16`       | `51235`                                       | Public peer-protocol port. Open in firewall when joining mainnet/testnet.   |
| `max_peers`    | `usize`     | `21`                                          | Soft cap on combined inbound + outbound peers.                              |
| `seeds`        | `[String]`  | mainnet + altnet bootstrap nodes              | DNS-resolved bootstrap peers. First contact list.                           |
| `fixed_peers`  | `[String]`  | `[]`                                          | Always-reconnect peers (your own infra, cluster).                           |
| `node_seed`    | `String?`   | `None`                                        | 32-hex deterministic identity. Omit for random.                             |
| `tls_enabled`  | `bool`      | `true`                                        | TLS for the peer protocol. Mainnet/testnet require `true`.                  |

Set `tls_enabled = false` only for isolated local sandboxes.

## `[database]`

| Field             | Type      | Default        | Description                                                                  |
|-------------------|-----------|----------------|------------------------------------------------------------------------------|
| `path`            | `Path`    | `data`         | Data directory. Must be writable by the rxrpl user.                          |
| `backend`         | `String`  | `memory`       | `rocksdb` for production; `memory` for dev/tests.                            |
| `online_delete`   | `u32`     | `2000`         | Number of recent ledgers to retain. `0` disables pruning.                    |
| `advisory_delete` | `bool`    | `false`        | When `true`, pruning only triggers via the `can_delete` admin RPC.           |
| `shard.enabled`   | `bool`    | `false`        | Enable the shard store.                                                      |
| `shard.path`      | `String`  | `data/shards`  | Shard directory.                                                             |
| `shard.max_shards`| `u32`     | `10`           | Cap on locally retained shards.                                              |

Switching backends requires wiping the data directory; rxrpl does not
migrate between `memory` and `rocksdb` on the fly.

## `[validators]`

| Field                        | Type        | Default | Description                                                                              |
|------------------------------|-------------|---------|------------------------------------------------------------------------------------------|
| `enabled`                    | `bool`      | `false` | Set to `true` only when this node signs validations.                                     |
| `trusted`                    | `[String]`  | `[]`    | Hard-coded trusted validator pubkeys. Prefer dynamic UNL via `validator_list_*` fields.  |
| `validator_list_sites`       | `[String]`  | `[]`    | UNL publisher HTTPS endpoints.                                                           |
| `validator_list_keys`        | `[String]`  | `[]`    | UNL publisher signing keys.                                                              |
| `quorum`                     | `usize?`    | auto    | Override quorum size. Leave unset to auto-compute from UNL size.                         |
| `require_trusted_validators` | `bool`      | `true`  | Aggregator only counts trusted validations. Set to `false` for tests with no UNL.        |

## `[network]`

| Field        | Type   | Default | Description                              |
|--------------|--------|---------|------------------------------------------|
| `network_id` | `u32`  | `0`     | `0` = mainnet, `1` = altnet, dev IDs ok. |

## `[genesis]`

| Field         | Type      | Default | Description                                      |
|---------------|-----------|---------|--------------------------------------------------|
| `ledger_hash` | `String?` | `None`  | Override genesis hash (network identification).  |

## `[cluster]`

| Field                     | Type        | Default | Description                                              |
|---------------------------|-------------|---------|----------------------------------------------------------|
| `enabled`                 | `bool`      | `false` | Enable TMCluster broadcasts.                             |
| `node_name`               | `String?`   | `None`  | Human-readable name in cluster status messages.          |
| `members`                 | `[String]`  | `[]`    | Hex-encoded pubkeys of trusted cluster peers.            |
| `broadcast_interval_secs` | `u64`       | `5`     | Cluster status broadcast interval.                       |

## `[reporting]`

| Field         | Type     | Default                  | Description                                       |
|---------------|----------|--------------------------|---------------------------------------------------|
| `enabled`     | `bool`   | `false`                  | Reporting mode (read-only ETL sink).              |
| `etl_source`  | `String` | `ws://127.0.0.1:6006`    | WebSocket URL of the upstream validating node.    |
| `forward_url` | `String` | `http://127.0.0.1:5005`  | Where to forward write requests (submit, etc.).   |

## Validating Your Config

The `rxrpl-config` crate parses every template in `config/` as part of
its test suite (`cargo test -p rxrpl-config`). To check a custom file
manually:

```bash
cargo run -p rxrpl --quiet -- run --config /etc/rxrpl/config.toml --dry-run 2>&1 | head
```

A future `rxrpl config validate` subcommand (batch B3) will provide a
dedicated check.

## Cross-Reference: rippled.cfg → rxrpl

| rippled (`rippled.cfg`)       | rxrpl                              |
|-------------------------------|------------------------------------|
| `[server] / port_rpc_admin`   | `[server] bind`                    |
| `[ips]`                       | `[peer] seeds`                     |
| `[ips_fixed]`                 | `[peer] fixed_peers`               |
| `[node_db] / type`            | `[database] backend`               |
| `[node_db] / path`            | `[database] path`                  |
| `[node_db] / online_delete`   | `[database] online_delete`         |
| `[validator_list_sites]`      | `[validators] validator_list_sites`|
| `[validator_list_keys]`       | `[validators] validator_list_keys` |
| `[validators]`                | `[validators] trusted`             |
| `[network_id]`                | `[network] network_id`             |
