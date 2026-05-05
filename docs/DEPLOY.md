# Deploy rxrpl

This guide walks an operator from "nothing installed" to "node accepting RPC and
peering on a network" in under 10 minutes. Two paths are documented:

1. Docker / docker-compose (recommended for first deploy and most operators).
2. systemd unit on a Debian/Ubuntu host (recommended for long-running validators).

For configuration field reference, see `docs/CONFIG.md`. For day-2 operations,
see `docs/RUNBOOK.md` and `docs/TROUBLESHOOT.md`.

---

## 1. Prerequisites

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| CPU         | 4 cores | 8 cores     |
| RAM         | 8 GB    | 16 GB       |
| Disk        | 100 GB SSD | 500 GB NVMe |
| Network     | 100 Mbps stable | 1 Gbps |
| OS          | Linux x86_64 (kernel >= 5.10) | Debian 12 / Ubuntu 22.04 |

Open inbound TCP `51235` (peer protocol) and, for trusted clients only, TCP
`5005` (JSON-RPC). Never expose RPC to the public internet without a reverse
proxy enforcing auth and rate limits.

---

## 2. Path A — Docker

### 2.1 Build the image

```sh
git clone https://github.com/xrpl-commons/rxrpl.git
cd rxrpl
docker build -t rxrpl:latest .
```

### 2.2 Pick a config template

Copy one of the shipped templates and edit it:

```sh
mkdir -p /srv/rxrpl
cp config/rxrpl-mainnet.toml /srv/rxrpl/rxrpl.toml
$EDITOR /srv/rxrpl/rxrpl.toml
```

Templates available:

- `config/rxrpl-standalone.toml` — single node, no peers, in-memory store. Dev only.
- `config/rxrpl-testnet.toml` — testnet UNL, lower resource footprint.
- `config/rxrpl-mainnet.toml` — production-grade defaults. Always review before use.

### 2.3 Run with docker-compose

```sh
cp examples/docker-compose.yml /srv/rxrpl/docker-compose.yml
cd /srv/rxrpl
docker compose up -d
```

### 2.4 Verify

```sh
docker compose ps
docker compose logs --tail=200 -f rxrpl
curl -fsS -H 'content-type: application/json' \
    --data '{"method":"server_info","params":[{}]}' \
    http://127.0.0.1:5005 | jq .
```

You should see `result.info.server_state` cycle through `connected` → `syncing`
→ `full` (or `validating` for a configured validator).

---

## 3. Path B — systemd

### 3.1 Install the binary

Build from source on the target host (or copy a pre-built binary into
`/usr/local/bin/rxrpl`):

```sh
git clone https://github.com/xrpl-commons/rxrpl.git
cd rxrpl
cargo build --release --bin rxrpl
sudo install -m 0755 target/release/rxrpl /usr/local/bin/rxrpl
```

### 3.2 Create the service user and directories

```sh
sudo useradd --system --home-dir /var/lib/rxrpl --shell /usr/sbin/nologin rxrpl
sudo install -d -m 0750 -o rxrpl -g rxrpl /var/lib/rxrpl
sudo install -d -m 0755 -o root  -g root  /etc/rxrpl
sudo install -m 0640 -o root -g rxrpl config/rxrpl-mainnet.toml /etc/rxrpl/config.toml
```

### 3.3 Install the unit

```sh
sudo install -m 0644 packaging/rxrpl.service /etc/systemd/system/rxrpl.service
sudo systemctl daemon-reload
sudo systemctl enable --now rxrpl
```

### 3.4 Verify

```sh
systemctl status rxrpl
journalctl -u rxrpl -f
```

A clean stop should complete within the unit's `TimeoutStopSec` (default 30s):

```sh
sudo systemctl stop rxrpl
journalctl -u rxrpl -n 50 --no-pager
```

---

## 4. First-deploy checklist

- [ ] Outbound peering established (`server_info.peers > 0`).
- [ ] Ledger advancing (`server_info.complete_ledgers` grows).
- [ ] `/metrics` reachable from your Prometheus scraper.
- [ ] RPC port not exposed to public internet.
- [ ] Disk free space monitored (alert at 80% used).
- [ ] Backups configured for `data-dir` and validator keys (if any).

---

## 5. Upgrades

See `docs/RUNBOOK.md` section "Upgrade procedure" for the full procedure. The
short form for Docker:

```sh
docker compose pull        # or rebuild
docker compose up -d
```

For systemd:

```sh
sudo systemctl stop rxrpl
sudo install -m 0755 target/release/rxrpl /usr/local/bin/rxrpl
sudo systemctl start rxrpl
```

Always verify post-upgrade with `server_info` and tail logs for at least one
ledger close before declaring the upgrade successful.
