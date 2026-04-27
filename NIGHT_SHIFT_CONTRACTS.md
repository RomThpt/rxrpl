# Sprint Contracts — rxrpl — 2026-04-27

> Acceptance criteria in Given/When/Then form. Used by **night-qa** to validate features adversarially. Backend-focused: contracts target xrpl-hive sims and cargo test suites instead of Puppeteer DOM assertions.

---

## T24 — xrpl-hive smoke + propagation cross-impl

### Given
- nightly branch `nightly/2026-04-27` with T01-T17 merged
- xrpl-hive checkout at `~/Developer/xrpl-hive` on its current default branch
- Docker daemon running, `rippled_2.3.0` base image already pulled
- rxrpl client image rebuilt once with `--docker.nocache rxrpl`

### When
- the QA agent runs `./bin/xrpl-hive --sim smoke --client rxrpl`
- followed by `./bin/xrpl-hive --sim propagation --client rxrpl,rippled_2.3.0`
- captures `workspace/logs/*.json` and `workspace/logs/details/*.log`

### Then
- smoke sim: `passed=3 failed=0` in `workspace/logs/<run>.json`
- propagation sim: at least one log line `validated_ledger.seq=N` for both `client=rxrpl` and `client=rippled_2.3.0` with the same N (within 1)
- no `details/*.log` line containing `bad signature`, `wrong prev ledger`, or `panic`

### Adversarial probes
- restart the rxrpl container mid-run — must reconnect and resume validating within 30s
- spam the rxrpl peer with 1k duplicate proposals — rxrpl must not OOM (RSS < 1.5 GB)
- send a stale validation (sign_time = now-600s) — must be dropped with `validation_dropped_stale_total` bumped

### Evidence required
- workspace/logs JSON snippet pasted in NIGHT_SHIFT_LOG.md
- output of `docker stats` peak RSS
- counter readout from rxrpl tracing logs for `validation_dropped_stale_total`

---

## T25 — xrpl-hive consensus + sync sims

### Given
- T24 is green
- the same nightly branch with all code tasks merged
- mainnet wallclock-synced (NTP within 200 ms)

### When
- the QA agent runs `./bin/xrpl-hive --sim consensus --client rxrpl,rippled_2.3.0`
- then runs `./bin/xrpl-hive --sim sync --client rxrpl`

### Then
- consensus sim: at least 3 consecutive ledgers closed with rxrpl in the validator set (look for `accepted_validation` lines emitted by rxrpl)
- sync sim: late joiner reaches `validated_ledger.seq >= seed_anchor.seq - 1` within 60s, OR documents that an external typo blocker still trips the test

### Adversarial probes
- stop one rippled validator after 2 ledgers — consensus must continue (>=66% remaining)
- inject a divergent transaction into rxrpl only — dispute must resolve to drop within 4 rounds

### Evidence required
- log excerpt showing consensus rounds + accepted_validation
- gaps.md updated with new score row dated 2026-04-27

---

## T26 — Property tests + fuzz smoke

### Given
- nightly branch with T11, T22, T23 merged
- nightly Rust toolchain installed (`rustup toolchain install nightly`)
- cargo-fuzz installed

### When
- QA agent runs `cargo test -p rxrpl-consensus -p rxrpl-overlay --all-features`
- then `cargo +nightly fuzz run stobject_decode -- -max_total_time=60`
- then `cargo +nightly fuzz run validation_deser -- -max_total_time=60`

### Then
- all tests pass; no `proptest! found shrunken counterexample`
- both fuzzers exit code 0 with no `crash-*` artifact created in `fuzz/artifacts/`
- coverage uplift logged (`fuzz/coverage/` if `cargo fuzz coverage` is run)

### Adversarial probes
- run the fuzzers with `-max_len=131072` to exercise large inputs
- feed the seed corpus from `fuzz/corpus/` if present

### Evidence required
- summary line from `cargo test`
- last 20 lines of each fuzzer stdout (showing exec/s and unique features)
