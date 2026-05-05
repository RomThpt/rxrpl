# Validator Seed File with Strict Unix Permissions (mode 0600)

## Audit: Current Validator Seed Loading Flow

### Current State
1. **No file-based validator seed loader exists** — validation keys are currently:
   - Generated ad-hoc via RPC handlers (`validation_create`, `validation_seed`)
   - Not loaded from disk at node startup
   - Not persistent across restarts

2. **Node identity (P2P) seed handling** (exists in `crates/node/src/node.rs:616-622`):
   - Loaded from `config.peer.node_seed` (optional string)
   - Accepts 32-char hex OR base58 family seed format
   - Parsed by `parse_node_seed()` → 16-byte entropy
   - No persistent storage; must be provided each boot via config or env

3. **Config loading** (`crates/config/src/loader.rs`):
   - Simple `fs::read_to_string()` + TOML parse
   - No permission validation; assumes config file is operator-owned

4. **Crypto primitives** (`crates/crypto/src/seed.rs`):
   - `Seed::from_passphrase()` — SHA-512(passphrase)[:16]
   - `Seed::from_bytes()` — raw 16-byte constructor
   - `Seed::random()` — cryptographically random seed
   - Zeroized on drop ✓

5. **Error types**:
   - `ConfigError` — IO, Parse, Invalid
   - `NodeError` — Config, Storage, Server, etc.
   - Both use `thiserror`; can be extended

### Dependencies Available
- **Standard library only** — no `cap-std`, `nix`, or `rustix` in workspace
- Will need to add: `std::fs::metadata()` + `#[cfg(unix)]` for mode checks
- Windows: use `std::fs::File::open()` with ACL validation or graceful skip

---

## Identified Target Crates & Files

### Crate: `rxrpl-config`
Files to modify/create:
- `crates/config/src/types.rs` — add `ValidatorConfig::seed_file` field (Optional<PathBuf>)
- `crates/config/src/loader.rs` — extend to load & validate seed file
- `crates/config/src/error.rs` — add `SeedFile` variant
- (NEW) `crates/config/src/seed_loader.rs` — dedicated seed file I/O module

### Crate: `rxrpl-node`
Files to modify:
- `crates/node/src/node.rs` — integrate seed file load at startup (line ~601 `run_networked`)
- `crates/node/src/error.rs` — extend with `SeedFilePermission` variant

### Crate: `rxrpl-crypto`
No changes needed — existing `Seed` API is sufficient.

### RPC Server (validation_seed handler)
- `crates/rpc-server/src/handlers/validation_seed.rs` — already works; no changes
- (NOTE) Handlers for *generating* seeds; validator *loading* is separate concern

### Tests
- (NEW) `crates/config/tests/seed_file_permission.rs` — Unix mode 0600 validation
- (NEW) Integration test in `crates/node/tests/` — load & validate seed at startup

---

## Spec: Implementation Batches (B1–B5)

### **B1: Config Type & Permission Error Types**
**Goal:** Add seed file path to ValidatorConfig; extend error enums.

**Files:**
- `crates/config/src/types.rs` — add to `ValidatorConfig`:
  ```toml
  [validators]
  enabled = true
  seed_file = "/etc/rxrpl/validator-seed"  # optional
  ```
- `crates/config/src/error.rs` — add variants:
  - `SeedFileNotFound(PathBuf)`
  - `SeedFilePermissionDenied(PathBuf, mode u32)` — Unix mode too loose
  - `SeedFileUnreadable(PathBuf, String)` — generic I/O error
  - `SeedFileWindowsNotSupported` — Windows doesn't store Unix mode
- `crates/node/src/error.rs` — add: `SeedFileError(String)` variant

**Failing test:** Config with `seed_file` set parses without error (no loader yet).

**Verify:**
```bash
cargo test --package rxrpl-config config_types
```

---

### **B2: Seed File Loader Module (Unix Mode Check)**
**Goal:** Implement `crates/config/src/seed_loader.rs` — read seed file, enforce 0600 on Unix.

**Files:**
- (NEW) `crates/config/src/seed_loader.rs`:
  ```rust
  pub fn load_seed_file(path: &Path) -> Result<Seed, ConfigError>
  ```
  - Read file bytes (must be 16 or 32 hex chars)
  - On Unix: call `std::fs::metadata()`, check mode == 0o600
  - On Windows: warn in log (ACL not portable; skip check)
  - Return `Seed` on success

- `crates/config/src/lib.rs` — export module

**Failing test:**
- File with mode 0644 → `SeedFilePermissionDenied`
- File with mode 0600 → success
- Missing file → `SeedFileNotFound`

**Verify:**
```bash
cargo test --package rxrpl-config seed_file --lib
```

---

### **B3: Config Loader Integration**
**Goal:** Wire seed file loader into config parsing at load time.

**Files:**
- `crates/config/src/loader.rs`:
  ```rust
  pub fn load_config(path: impl AsRef<Path>) -> Result<(NodeConfig, Option<Seed>), ConfigError>
  ```
  - Load NodeConfig as before
  - If `config.validators.seed_file` is Some, call `load_seed_file()`
  - Return tuple: (config, seed)

**Failing test:**
- Config without `seed_file` → returns None seed ✓
- Config with valid seed file → returns seed bytes ✓
- Config with loose-perm seed file → Error

**Verify:**
```bash
cargo test --package rxrpl-config loader
```

---

### **B4: Node Startup Integration (Pass-Through)**
**Goal:** Load validator seed at `Node::run_networked()` boot; store in `Node` struct.

**Files:**
- `crates/node/src/node.rs`:
  - Add field to `struct Node`: `validation_seed: Option<Seed>`
  - Modify `Node::new(config)` → load seed if config specifies path
  - At startup, if enabled: initialize validator signing key (placeholder for later)
  - Log: "validator seed loaded from X" or warning if seed_file specified but disabled

- `crates/node/src/error.rs` — ensure `SeedFileError` propagates from config

**Failing test:**
- Create node with validator enabled + seed file set → seed loaded ✓
- Create node with validator disabled + seed file set → warning logged, no panic
- Create node, seed file has wrong mode → startup fails cleanly

**Verify:**
```bash
cargo test --package rxrpl-node node -- --nocapture
```

---

### **B5: Write Path + Umask (File Creation)**
**Goal:** Add CLI helper or utility fn to create seed file with 0600 atomically.

**Files:**
- (NEW) `crates/config/src/seed_writer.rs`:
  ```rust
  pub fn write_seed_file(path: &Path, seed: &Seed) -> Result<(), ConfigError>
  ```
  - Create file with O_EXCL (fail if exists)
  - On Unix: set umask(0o077) before create, then restore
  - Write 16 bytes (hex) or raw binary
  - Sync to disk
  - Verify mode == 0o600 after write

- (NEW) CLI subcommand (e.g., `rxrpl init-validator-seed --path /etc/rxrpl/seed`):
  ```bash
  rxrpl init-validator-seed \
    --path /etc/rxrpl/validator-seed \
    [--generate | --from-passphrase "..."]
  ```
  - Generate random or from passphrase
  - Write with 0600
  - Print validation key (public) for UNL registration

**Failing test:**
- Write file, check mode == 0o600 ✓
- Write to existing file → fails (O_EXCL)
- Write then read back → identical seed ✓

**Verify:**
```bash
cargo test --package rxrpl-config seed_writer
```

---

## Risk Analysis

### High
1. **TOCTOU (Time-of-Check-Time-of-Use):**
   - Attacker could change mode between our check and read
   - *Mitigation:* Atomic check+read in single `open()` call; use `fcntl(F_GETFL)` after open to verify mode still 0o600
   - *Acceptance:* Not exploitable in typical setups; Docker/container environments may need special guidance

2. **Windows ACL story:**
   - Unix mode 0600 is meaningless on Windows
   - No portable ACL equivalent
   - *Solution:* Log warning on Windows; require operator to manually secure via NTFS ACLs or GPO
   - *Or:* Use `rustls` already in dependencies, embed seed in encrypted config + password prompt at startup (defer to future iteration)

### Medium
3. **Symlink attack:**
   - If seed_file points to symlink, attacker could redirect to world-readable file
   - *Mitigation:* Check that seed_file path is not a symlink; use `fs::symlink_metadata()` → `is_symlink()`
   - *If symlink:* Error or warning (decide per ops preference)

4. **Docker/container ergonomics:**
   - 0600 file in volume mount may not be preservable across container restart
   - Docker run: `--cap-drop=DAC_OVERRIDE` helps; volume mount may lose perms
   - *Guidance:* Document use of tmpfs or secrets manager integration (future)

5. **Backward compat for users with seed in `rxrpl.cfg`:**
   - Some users may have stored `[validators] seed = "hex..."` in config
   - *Approach:* Keep that field unsupported (or with a loud deprecation warning)
   - Migration: provide one-time CLI tool to extract from cfg → write to 0600 file

### Low
6. **Parent directory mode:**
   - If `/etc/rxrpl/` is world-writable, attacker could mv/swap file
   - *Mitigation:* Check parent dir mode in `load_seed_file()` (warn if world-writable)
   - *Or:* Document that parent must be 0750 or stricter (operator responsibility)

---

## Out of Scope

1. **Key rotation:** Only load once at startup; rotations require node restart
2. **Hardware security modules (HSM):** Direct file I/O only; no PKCS#11 integration
3. **Encrypted seed storage:** Seed is stored plaintext at 0600; if encryption needed, wrap at filesystem level (LUKS, dm-crypt)
4. **Multi-signature validator keys:** Single-key support; multisig is separate protocol concern
5. **Audit logging for seed access:** OS-level audit (`auditctl`, `seaudit`) is operator responsibility
6. **Seed rotation on file modification:** No active monitor; requires manual restart
7. **gRPC server integration:** Currently RPC-only; gRPC handlers can be added in parallel

---

## Progress Summary

**% Done:** 5% (audit complete; ready for B1)

**# Batches:** 5 (B1 config types, B2 loader, B3 config integration, B4 node startup, B5 write path)

**Biggest Risk:** Windows ACL story + TOCTOU attack window. Solved by: Windows → warn + document, TOCTOU → use atomic open+verify post-open.

**Effort:** ~2–3 days (B1-B2: 4h, B3: 2h, B4: 3h, B5: 3h + testing 2h)

**Next:** Start B1 — add `seed_file: Option<PathBuf>` to `ValidatorConfig`.

