#!/usr/bin/env python3
"""Generate interop test network configurations.

Creates configs for a mixed rippled + rxrpl private validator network.
All nodes share the same UNL (trusted validator list) and connect
to each other as fixed peers.

Usage:
    python scripts/generate_configs.py [--rippled N] [--rxrpl N]
"""

import argparse
import hashlib
import os
import secrets

CONFIGS_DIR = os.path.join(os.path.dirname(__file__), "..", "configs")


def generate_node_seed():
    """Generate a random 32-byte hex seed for a validator node."""
    return secrets.token_hex(32)


def seed_to_public_key_stub(seed: str, index: int) -> str:
    """Derive a deterministic fake public key from seed for UNL.

    In production, this would use Ed25519 key derivation.
    For test configs, we use a deterministic hash as placeholder.
    """
    h = hashlib.sha256(f"{seed}:{index}".encode()).hexdigest()
    return f"n9{h[:50]}"


def write_rippled_config(path: str, index: int, seed: str, peer_port: int,
                         rpc_port: int, fixed_peers: list[str]):
    """Write a rippled.cfg for a validator node."""
    peers_section = "\n".join(fixed_peers)

    config = f"""[server]
port_rpc_admin_local
port_peer

[port_rpc_admin_local]
port = {rpc_port}
ip = 0.0.0.0
admin = 0.0.0.0
protocol = http

[port_peer]
port = {peer_port}
ip = 0.0.0.0
protocol = peer

[node_size]
tiny

[node_db]
type=NuDB
path=/var/lib/rippled/db/nudb
advisory_delete=0
online_delete=256
earliest_seq=1

[database_path]
/var/lib/rippled/db

[debug_logfile]
/var/log/rippled/debug.log

[sntp_servers]
time.windows.com
time.apple.com
time.nist.gov
pool.ntp.org

[ips_fixed]
{peers_section}

[validators_file]
validators.txt

[rpc_startup]
{{ "command": "log_level", "severity": "warning" }}

[validation_seed]
{seed}

[validator_token]

[network_id]
99

[ledger_history]
256

# Allow standalone-like fast close for testing
[consensus]
minimum_duration_ms=200
"""
    with open(path, "w") as f:
        f.write(config)


def write_rxrpl_config(path: str, index: int, seed: str, rpc_bind: str,
                       peer_port: int, fixed_peers: list[str],
                       trusted_keys: list[str]):
    """Write a rxrpl TOML config for a validator node."""
    peers_toml = ", ".join(f'"{p}"' for p in fixed_peers)
    trusted_toml = ", ".join(f'"{k}"' for k in trusted_keys)

    config = f"""[server]
bind = "{rpc_bind}"
admin_ips = ["0.0.0.0"]

[peer]
port = {peer_port}
max_peers = 21
seeds = []
fixed_peers = [{peers_toml}]
node_seed = "{seed}"
tls_enabled = false

[database]
path = "/tmp/rxrpl-{index}"
backend = "memory"
online_delete = 256

[validators]
enabled = true
trusted = [{trusted_toml}]
quorum = 3

[network]
network_id = 99

[genesis]
"""
    with open(path, "w") as f:
        f.write(config)


def write_validators_txt(path: str, public_keys: list[str]):
    """Write the shared validators.txt for rippled nodes."""
    lines = ["[validators]"]
    for key in public_keys:
        lines.append(f"    {key}")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description="Generate interop test configs")
    parser.add_argument("--rippled", type=int, default=3, help="Number of rippled nodes")
    parser.add_argument("--rxrpl", type=int, default=2, help="Number of rxrpl nodes")
    args = parser.parse_args()

    os.makedirs(CONFIGS_DIR, exist_ok=True)

    # Generate seeds and keys for all validators
    seeds = []
    public_keys = []
    for i in range(args.rippled + args.rxrpl):
        seed = generate_node_seed()
        seeds.append(seed)
        public_keys.append(seed_to_public_key_stub(seed, i))

    # Build peer addresses
    rippled_peers = [f"172.30.0.{10 + i}:51235" for i in range(args.rippled)]
    rxrpl_peers = [f"172.30.0.{20 + i}:51235" for i in range(args.rxrpl)]
    all_peers = rippled_peers + rxrpl_peers

    # Write rippled configs
    for i in range(args.rippled):
        peers = [p for p in all_peers if p != rippled_peers[i]]
        write_rippled_config(
            os.path.join(CONFIGS_DIR, f"rippled-{i}.cfg"),
            index=i,
            seed=seeds[i],
            peer_port=51235,
            rpc_port=5005,
            fixed_peers=peers,
        )

    # Write rxrpl configs
    for i in range(args.rxrpl):
        peers = [p for p in all_peers if p != rxrpl_peers[i]]
        write_rxrpl_config(
            os.path.join(CONFIGS_DIR, f"rxrpl-{i}.toml"),
            index=i,
            seed=seeds[args.rippled + i],
            rpc_bind=f"0.0.0.0:5005",
            peer_port=51235,
            fixed_peers=peers,
            trusted_keys=public_keys,
        )

    # Write shared validators.txt for rippled
    write_validators_txt(
        os.path.join(CONFIGS_DIR, "validators.txt"),
        public_keys,
    )

    print(f"Generated configs for {args.rippled} rippled + {args.rxrpl} rxrpl nodes")
    print(f"  Configs: {CONFIGS_DIR}/")
    print(f"  Validators: {len(public_keys)} total")


if __name__ == "__main__":
    main()
