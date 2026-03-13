# rxrpl

A modular, type-safe Rust SDK for the XRP Ledger.

## Quick start

```toml
[dependencies]
rxrpl = "0.1"
```

```rust
use rxrpl::{Wallet, KeyType};

let wallet = Wallet::generate(KeyType::Ed25519);
println!("Address: {}", wallet.address);
```

With RPC client:

```toml
[dependencies]
rxrpl = { version = "0.1", features = ["client", "autofill"] }
```

## Crates

| Crate | Description |
|-------|-------------|
| `rxrpl` | Facade crate -- single dependency with feature flags |
| `rxrpl-primitives` | Core types: `AccountId`, `Amount`, `Hash256`, `PublicKey` |
| `rxrpl-crypto` | Ed25519/secp256k1 key derivation, signing, verification |
| `rxrpl-codec` | Binary codec, classic/X-address encoding, seed encoding |
| `rxrpl-protocol` | Transactions, wallet, signing, multisig |
| `rxrpl-rpc-api` | JSON-RPC request/response type definitions |
| `rxrpl-rpc-client` | Async RPC client (HTTP + WebSocket) |

## Feature flags

| Feature | Enables | Default |
|---------|---------|---------|
| `crypto` | Key generation, signing primitives | yes |
| `codec` | Binary codec, address encoding | yes |
| `protocol` | Transactions, wallet, signing | yes |
| `rpc` | RPC API type definitions | no |
| `client` | Async RPC client (HTTP + WebSocket) | no |
| `autofill` | Transaction autofill via RPC | no |
| `full` | All of the above | no |

## Examples

```sh
cargo run --example generate_wallet -p rxrpl --features protocol
cargo run --example decode_address -p rxrpl --features codec
cargo run --example send_payment  -p rxrpl --features "client,autofill"
cargo run --example subscribe_ledger -p rxrpl --features client
```

## CLI

```sh
cargo install --path bin/rxrpl
rxrpl --help
```

## MSRV

Rust 1.85+

## License

MIT OR Apache-2.0
