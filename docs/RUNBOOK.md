# rxrpl Operations Runbook

This runbook is for engineers running rxrpl in production. It covers daily
operation, monitoring, common failure modes, upgrades, and key rotation. For
first install see `docs/DEPLOY.md`. For configuration field reference see
`docs/CONFIG.md`. For quick error lookups see `docs/TROUBLESHOOT.md`.

---

## 1. Run modes

| Mode | When to use | Config flag / template |
|------|-------------|------------------------|
| Standalone | Local dev, integration tests | `config/rxrpl-standalone.toml` |
| Network    | Public node tracking a chain (testnet or mainnet) | `config/rxrpl-testnet.toml` / `config/rxrpl-mainnet.toml` |
| Reporting  | Read-only ETL sink for BI / indexers | `--reporting` + a network config |

Switch mode by changing the config and restarting the process. Standalone never
opens a peer port; network and reporting both bind P2P and require outbound
connectivity.

### Start

```sh
# systemd
sudo systemctl start rxrpl

# Docker
docker compose up -d
```

### Stop (graceful)

`SIGTERM` triggers the graceful shutdown path: stop accepting new RPCs, drain
in-flight transactions, close the current ledger, flush the node store, then
exit. Default deadline is 30 seconds (override with `TimeoutStopSec=` in the
unit or `stop_grace_period:` in compose).

```sh
sudo systemctl stop rxrpl                  # systemd
docker compose stop rxrpl                  # compose
kill -TERM <pid>                           # bare process
```

`SIGINT` (Ctrl+C in foreground) is treated identically. `SIGKILL` will leave
the node store consistent (writes are durable) but may discard pending consensus
state for the current round.

### Restart

```sh
sudo systemctl restart rxrpl
```

A restart should not require a resync. If the process died uncleanly, start
the node and watch logs for `recovering` followed by `synced` / `full`.

---

## 2. Monitoring

Scrape `/metrics` from the RPC bind address with Prometheus. The metric names
shipped today are listed in `crates/rpc-server/src/metrics.rs`. The most
operationally relevant ones:

| Metric | Type | What to watch |
|--------|------|---------------|
| `p2p_peers_connected` | gauge | Drops below `min_peers` -> network issue |
| `ledger_sequence` | gauge | Stops increasing -> consensus stalled or syncing lag |
| `ledger_close_duration_seconds` | histogram | p95 climbing -> CPU/disk pressure |
| `txq_size` | gauge | Approaches `max_size` -> rejecting submissions |
| `txq_escalated_fee_drops` | gauge | Escalating -> demand exceeds capacity |
| `consensus_round_duration_seconds` | histogram | p95 > 5s -> peer or DB issue |
| `consensus_stalls_total` | counter | Any non-zero rate -> investigate |
| `rpc_errors_total{error_type}` | counter | Spike -> client misuse or backend down |
| `nodestore_cache_misses_total` | counter | Sustained miss rate -> resize cache |

Suggested SLOs for a public mainnet tracking node:

- `p2p_peers_connected >= 8` for 99% of 5-minute windows.
- Ledger advancing every 4-5 seconds (`rate(ledger_sequence[5m]) > 0.15`).
- `consensus_stalls_total` rate of change == 0 for any 1-hour window.
- `histogram_quantile(0.95, sum by (le) (rate(rpc_request_duration_seconds_bucket[5m]))) < 0.5s`.

Alerting rules and a starter Grafana dashboard ship under `examples/`:

- `examples/prometheus/alert-rules.yaml`
- `examples/grafana/dashboard.json`

---

## 3. Troubleshooting decision tree

Always start by capturing:

```sh
# Last 500 log lines with timestamps
journalctl -u rxrpl -n 500 --no-pager > rxrpl-incident.log

# Live RPC snapshot
curl -fsS -H 'content-type: application/json' \
    --data '{"method":"server_info","params":[{}]}' \
    http://127.0.0.1:5005 | tee server-info.json
```

### 3.1 Symptom: `peers == 0`

1. Check outbound: `ss -tnp | grep rxrpl` should list ESTABLISHED sockets to
   port 51235 of remote hosts.
2. Check inbound: `ss -tlnp | grep 51235` confirms the port is bound.
3. Firewall: confirm 51235/tcp is open inbound; outbound should be unrestricted.
4. Config: in `[peer]`, validate `peer_list` (or bootstrap UNL) is non-empty
   and resolves. `dig +short <hostname>` should return an A/AAAA record.
5. Network ID: `network_id` mismatch silently rejects peers. Cross-check
   against the network you intend to track.
6. Time skew: NTP must be configured. Skews > 60s cause peers to drop us.

### 3.2 Symptom: ledger not advancing

1. Confirm peers are present (above).
2. `server_info.server_state` values: `disconnected` -> peers missing;
   `connected` -> have peers, syncing; `syncing` -> applying historical
   ledgers; `full` -> caught up; `validating` -> caught up + signing.
3. If stuck at `syncing` for > 10 minutes:
   - Inspect `nodestore_*` metric ratios. High miss rate with low IOPS may
     indicate disk saturation.
   - Tail logs for `unable to fetch ledger` / `acquiring ledger` patterns.
4. If stuck at `full` but `ledger_sequence` is flat:
   - Check `consensus_stalls_total` counter.
   - Compare the local ledger hash for the last sequence with a known-good
     peer (e.g. `xrpl-cluster.ripple.com:51234`); a divergence requires a
     resync (see 3.5).

### 3.3 Symptom: `txq_size` near `max_size`, submissions rejected

1. Inspect `txq_escalated_fee_drops`. If escalation is engaging, this is
   demand-driven and the node is behaving correctly.
2. Look for clients submitting with too-low fees. `rpc_errors_total{error_type="insufficient_fee"}`
   will be elevated.
3. Tighten per-IP rate limits if a single client is monopolizing the queue.
4. If `txq_dequeued_total` rate is near zero while `txq_size` stays high, the
   node has stopped closing ledgers -> see 3.2.

### 3.4 Symptom: high RPC latency

1. Check `histogram_quantile(0.95, rate(rpc_request_duration_seconds_bucket[5m]))`.
2. Heavy methods (`ledger_data`, `account_tx` with wide ranges) dominate.
   Apply per-method limits at the reverse proxy.
3. Confirm disk latency: `iostat -x 1` p95 await should be < 5 ms on NVMe.
4. Check `nodestore_cache_misses_total / (hits + misses)` ratio. Sustained
   > 30% suggests cache undersized.

### 3.5 Symptom: ledger divergence / suspected corruption

This is the most invasive recovery. Last resort.

```sh
sudo systemctl stop rxrpl
sudo -u rxrpl rm -rf /var/lib/rxrpl/*    # or the configured data-dir
sudo systemctl start rxrpl
```

If a checkpoint is available:

```sh
sudo -u rxrpl rxrpl run --config /etc/rxrpl/config.toml \
    --data-dir /var/lib/rxrpl \
    --starting-ledger <known_good_seq>
```

After resync, verify the node converges to `full` and matches a peer's tip
hash before reopening RPC to clients.

---

## 4. Upgrade procedure

1. Read the release notes; check for amendment activations or config changes.
2. Snapshot:
   ```sh
   sudo systemctl stop rxrpl
   sudo tar -C /var/lib/rxrpl -czf /backup/rxrpl-$(date +%F).tgz .
   ```
3. Install the new binary:
   ```sh
   sudo install -m 0755 target/release/rxrpl /usr/local/bin/rxrpl
   /usr/local/bin/rxrpl version
   ```
4. Start and watch:
   ```sh
   sudo systemctl start rxrpl
   journalctl -u rxrpl -f
   ```
5. Verify (within 5 minutes):
   - `server_state` reaches `full` (or `validating`).
   - `ledger_sequence` matches a peer.
   - No new `consensus_stalls_total` increments.

For Docker, replace step 3 with `docker compose pull` (or rebuild) and
`docker compose up -d`. The healthcheck will mark the container unhealthy if
the upgrade fails to reach a serving state.

### Rollback

If the new version misbehaves within the first ledger close:

```sh
sudo systemctl stop rxrpl
sudo install -m 0755 /backup/rxrpl-prev /usr/local/bin/rxrpl
sudo systemctl start rxrpl
```

If state corruption is suspected after the upgrade, restore the snapshot from
step 2 before starting.

---

## 5. Validator key rotation

The full validator-keys subcommand will land in initiative I-B4. Until then,
operators using ad-hoc keys should:

1. Generate a new validator key pair offline (air-gapped host preferred).
2. Issue a manifest signed by the master key authorizing the new validation
   key with a higher sequence number.
3. Publish the manifest to the UNL publisher / token site.
4. Update the running node's `[validator]` config to point at the new
   ephemeral key file.
5. Restart with `systemctl reload rxrpl` if reload is supported, otherwise
   `restart`. Watch for one full ledger close before declaring success.
6. Keep the previous ephemeral key on disk (read-only) for 24 hours so peers
   that have not seen the new manifest can still verify our prior validations.

A complete decision tree (including amendments preservation) lands with B4.

---

## 6. Performance tuning

| Lever | When to adjust | How |
|-------|----------------|-----|
| `peer.max_peers` | More CPU/bandwidth available | Raise to 50-80 on validators |
| `database.cache_mb` | High `nodestore_cache_misses_total` rate | Increase by 25% increments |
| RocksDB block cache | Same as above on rocks-backed nodes | Set in TOML `[database.rocks]` |
| `txq.max_size` | Sustained queue saturation under demand | Raise cautiously; watch RAM |
| RPC threads | RPC p95 latency increasing | Bind RPC behind a reverse proxy with worker pool |

Always change one lever at a time and observe at least one full ledger close
window before drawing conclusions.

---

## 7. Logs and metrics ingestion

- Logs are written to stderr in tracing format. systemd captures them in
  the journal. Configure `[Service] StandardOutput=journal` (default).
- For ELK/Loki, run a sidecar (Vector / Promtail) tailing the journal unit.
- Prometheus scrape config example:

  ```yaml
  - job_name: rxrpl
    metrics_path: /metrics
    static_configs:
      - targets: ['rxrpl-host:5005']
        labels:
          network: mainnet
          role: tracking
  ```

- Disable `RUST_LOG=trace` in production; it can produce > 1 GB/hour.

---

## 8. Pre-flight checklist for a new validator

- [ ] Validator keys generated on an isolated host.
- [ ] Manifest published to the UNL signer.
- [ ] Node config has `[validator]` populated and points at the ephemeral key.
- [ ] Time sync (chrony or systemd-timesyncd) operational, drift < 50 ms.
- [ ] Filesystem journal commit interval acceptable for ledger close cadence.
- [ ] Backups of `data-dir` and key files automated.
- [ ] Alerting wired for the metrics in section 2.
- [ ] Runbook accessible to on-call engineers.
