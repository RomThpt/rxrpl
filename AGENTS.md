# rxrpl contributor notes

## Purpose

`rxrpl` is a Rust implementation of an XRP Ledger node and validator. Protocol
behaviour and binary output must match `rippled`; goXRPL and other clients are
useful comparison points but are not protocol authorities.

## Build and test

Use Rust 1.85 or newer from the workspace root.

```sh
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests that contact a full-history node are ignored by default. Run them only
with an explicit oracle endpoint:

```sh
RXRPL_PLAY_FORWARD_RPC=http://host:5005 \
  cargo test -p rxrpl-node --lib play_forward -- --ignored --nocapture
```

The segment replay interface used by `xrpl-state-compare` is:

```sh
rxrpl replay-segment \
  --state checkpoint.pack \
  --ledgers ledger.pack \
  --checkpoint PARENT \
  --start START --end END \
  --output-state resume.pack
```

`PARENT + 1` must equal `START`. It emits one JSON object for every ledger in
the exact inclusive range `START..=END`, followed by one summary object. Logs
and diagnostics must stay on stderr so consumers can parse stdout as JSON Lines.
`--output-state` is optional; it atomically writes the last faithful state as an
XSCP checkpoint. If replay stops on a divergence, that checkpoint is the exact
parent, never the divergent child.

## Protocol conventions

- Treat the matching `rippled` implementation and ledger-era amendments as the
  source of truth for serialization, rounding, metadata, result codes, and
  state transitions.
- Preserve canonical field ordering and exact integer/decimal arithmetic. Do
  not introduce floating-point math into consensus-critical paths.
- Gate historical behaviour by amendments or ledger era; a modern fix must not
  silently change replay of older ledgers.
- A byte-exact claim requires matching account, transaction and ledger hashes,
  total coins, and transaction metadata. Self-consistency tests are not an
  external oracle.
- Never skip an undecodable or failed transaction while reporting a replay as
  faithful.

## Change workflow

- Use short `feature/*`, `fix/*`, `refactor/*`, or `test/*` branches based on
  `main` and keep each protocol cause in an atomic commit.
- Commit subjects follow `type(scope): imperative description` and stay below
  72 characters.
- Add a focused regression test for the first divergent ledger or wire vector,
  then run format, Clippy, and the workspace tests before opening a PR.
- Keep oracle and interoperability fixes in separate repositories and PRs.
- Do not overwrite unrelated local changes or generated analysis directories.

## Important traps

- `cargo test --workspace` does not run live mainnet replay oracles.
- Replay after the first state divergence is contaminated unless the worker
  restarts from a trusted checkpoint or explicitly stops at that divergence.
- The transaction-set SHAMap root controls canonical application order and is
  distinct from the closed ledger transaction root containing metadata.
- Historical fee, reserve, directory ordering, close-time, and arithmetic rules
  must be derived from ledger state and amendments, not current defaults.
- OfferCreate filters its initial direct and XRP-bridge strands with multipath
  AMM quotes before reducing the active strand count; a later single-strand
  pass may use different AMM pricing.
- After an offer is exhausted, the next out-of-quality book page may contain
  offers that became unfunded. Reap those without deleting funded self offers
  whose price is beyond the taker's limit.
- The local Hive and Confluence checkouts may contain unrelated uncommitted
  client adapters; use clean worktrees and never discard those changes.
