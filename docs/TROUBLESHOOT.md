# rxrpl Troubleshooting Quick Reference

Use this page for fast lookup. For deeper context and recovery procedures see
`docs/RUNBOOK.md`.

---

## 1. Diagnostic one-liners

```sh
# Node state, ledger, peer count
curl -fsS -H 'content-type: application/json' \
    --data '{"method":"server_info","params":[{}]}' \
    http://127.0.0.1:5005 | jq '.result.info | {server_state, peers, complete_ledgers, validated_ledger}'

# Live metrics
curl -fsS http://127.0.0.1:5005/metrics | grep -E '^(p2p_peers|ledger_sequence|txq_size|consensus_stalls)'

# Tail logs (systemd)
journalctl -u rxrpl -n 200 --no-pager
journalctl -u rxrpl -f

# Tail logs (docker)
docker logs --tail 200 -f rxrpl
```

---

## 2. Symptom -> action

| Symptom | First check | Likely cause | Fix |
|---------|-------------|--------------|-----|
| `server_state == disconnected` | `peers` count | Outbound blocked, empty peer list | Open 51235/tcp; populate `peer.peer_list` |
| `server_state == syncing` for > 10 min | `ledger_sequence` rate | Slow peers or disk | See RUNBOOK 3.2 |
| `server_state == full`, ledger flat | `consensus_stalls_total` | Stalled consensus, divergence | See RUNBOOK 3.5 |
| RPC 5xx spike | `rpc_errors_total` by type | Backend regression / overload | Tail logs, scale RPC reverse proxy |
| RPC `insufficient_fee` rejections | `txq_escalated_fee_drops` | Demand-driven escalation | Inform clients; raise minimum fee on submitters |
| Submission rejections, queue full | `txq_size` vs `max_size` | Saturation | See RUNBOOK 3.3 |
| OOM kills | `MemoryHigh` warnings, RSS | Cache + queue too large | Lower `database.cache_mb` or `txq.max_size` |
| Slow ledger close (> 5s p95) | `iostat`, CPU saturation | Disk or CPU bottleneck | See RUNBOOK 6 |
| Container unhealthy | Healthcheck CMD output | RPC bind unreachable | Check `[server]` bind address; ensure port published |

---

## 3. Log grep patterns

```sh
# Peer churn
journalctl -u rxrpl --since '1 hour ago' | grep -E 'peer (connect|disconnect)' | wc -l

# Ledger close issues
journalctl -u rxrpl --since '15 min ago' | grep -E 'ledger close|consensus stall|ledger acquired'

# RPC 5xx
journalctl -u rxrpl --since '15 min ago' | grep -E 'rpc.*error|internal_error'

# Storage / nodestore
journalctl -u rxrpl --since '1 hour ago' | grep -iE 'rocksdb|nodestore|fetch.*node'
```

---

## 4. Common configuration mistakes

| Mistake | Effect | Fix |
|---------|--------|-----|
| `network_id` mismatch with peer set | Silent peer rejection, `peers == 0` | Match the chain's id (mainnet = 0) |
| RPC bind on `0.0.0.0` without proxy | Public abuse, rate limits exhausted | Bind on `127.0.0.1`; front with TLS proxy |
| Missing `data-dir` (or wrong perms) | Crash loop on start | `chown -R rxrpl:rxrpl <dir>`; mode 0750 |
| Validator key file world-readable | Critical security incident | `chmod 0400`; rotate key |
| `cluster.nodes` populated on standalone | Confusing log noise, no harm | Remove `[cluster]` for single-node setups |
| `RUST_LOG=trace` in production | Disk fills, latency rises | Set to `info` |

---

## 5. When to escalate

Open a maintainer ticket (or page on-call) if any of the following hold for
more than 15 minutes despite the actions above:

- `consensus_stalls_total` rate > 0.
- `ledger_sequence` flat while `p2p_peers_connected >= 8`.
- Repeated panics in the journal (search `panic` keyword).
- Suspected ledger divergence (hash mismatch with peers at a known sequence).
- Storage corruption errors from RocksDB.

Always attach: `server-info.json`, last 500 log lines, the running config (with
secrets redacted), the relevant `/metrics` snapshot.
