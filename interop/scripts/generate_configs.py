#!/usr/bin/env python3
"""Generate interop test network configurations (confluence topology).

Writes deterministic configs for the mixed validator network defined in
`interop/docker-compose.yml`: two rippled validators and one rxrpl
validator, all sharing one UNL.

The validator keys below are real, pre-harvested fixtures — not stubs:

  * Each rippled `[validation_seed]` is a base58 family seed; the matching
    `n...` NodePublic was read back from `server_info.pubkey_validator`
    of a rippled started with that seed.
  * The rxrpl seed feeds `[validator_identity]`; its `n...` key was read
    from the running node's loaded validator identity.

Because the keys are fixed, the generated configs are reproducible and
can be committed as fixtures. To rotate them, re-harvest (start each node
with a fresh seed, read back its validator public key) and update the
table below.
"""

import argparse
import os

CONFIGS_DIR = os.path.join(os.path.dirname(__file__), "..", "configs")

# Pre-harvested validator fixtures. Order matches docker-compose.yml.
RIPPLED_VALIDATORS = [
    {
        "seed": "ssnxjNzfejgYQUSJFXabJxTmueR8b",
        "public_key": "n9KXX4mAWwFeEBemh5Kg4e4o41pcmmhrCGFycMSdLx3yFSXZyvyf",
        "ip": "172.30.0.10",
    },
    {
        "seed": "shq5A3uBkbJsHyeeNZMGdgYwakQEK",
        "public_key": "n9Ki1vBTxo26iszNVRoGK1nFMsrfx68xnYcrBEUY62XsgHM7FCM5",
        "ip": "172.30.0.11",
    },
]
RXRPL_VALIDATORS = [
    {
        "seed": "sEdVZKfQYfinxEpnzyLmQZfSYNV2teD",
        "public_key": "n9Lc8w5xK1kJE4AiHX9kvDQF6shMBWoz8pEXp95xRo227ZMnUPt2",
        "ip": "172.30.0.20",
        "node_seed": "a1f33f544dbae3d90db16b1bc9e821e9",
    },
]

PEER_PORT = 51235
RPC_PORT = 5005
NETWORK_ID = 99


def all_peers():
    return [f"{v['ip']}:{PEER_PORT}" for v in RIPPLED_VALIDATORS + RXRPL_VALIDATORS]


def unl_keys():
    return [v["public_key"] for v in RIPPLED_VALIDATORS + RXRPL_VALIDATORS]


def write_rippled_config(path, seed, fixed_peers):
    peers_section = "\n".join(fixed_peers)
    config = f"""[server]
port_rpc_admin_local
port_peer

[port_rpc_admin_local]
port = {RPC_PORT}
ip = 0.0.0.0
admin = 0.0.0.0
protocol = http

[port_peer]
port = {PEER_PORT}
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

[ips_fixed]
{peers_section}

[validators_file]
validators.txt

[rpc_startup]
{{ "command": "log_level", "severity": "warning" }}

[validation_seed]
{seed}

[network_id]
{NETWORK_ID}

[ledger_history]
256

[consensus]
minimum_duration_ms=200
"""
    with open(path, "w") as f:
        f.write(config)


def write_rxrpl_config(path, validator, fixed_peers):
    peers_toml = ", ".join(f'"{p}"' for p in fixed_peers)
    trusted_toml = ", ".join(f'"{k}"' for k in unl_keys())
    config = f"""[server]
bind = "0.0.0.0:{RPC_PORT}"
admin_ips = ["0.0.0.0"]

[peer]
port = {PEER_PORT}
max_peers = 21
seeds = []
fixed_peers = [{peers_toml}]
node_seed = "{validator['node_seed']}"
tls_enabled = false

[database]
path = "/var/lib/rxrpl/data"
backend = "memory"
online_delete = 256

[validators]
enabled = true
trusted = [{trusted_toml}]
quorum = 2

[validator_identity]
master_secret = "{validator['seed']}"
ephemeral_seed = "{validator['seed']}"

[network]
network_id = {NETWORK_ID}
# Confluence topology: match rippled's fresh-network genesis, which holds
# only the master AccountRoot (no FeeSettings / Amendments SLEs).
genesis_amendments_disabled = true
"""
    with open(path, "w") as f:
        f.write(config)


def write_validators_txt(path):
    lines = ["[validators]"]
    for key in unl_keys():
        lines.append(f"    {key}")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description="Generate interop test configs")
    parser.add_argument("--rippled", type=int, default=2)
    parser.add_argument("--rxrpl", type=int, default=1)
    args = parser.parse_args()
    if args.rippled != len(RIPPLED_VALIDATORS) or args.rxrpl != len(RXRPL_VALIDATORS):
        parser.error(
            f"fixed-fixture topology is "
            f"{len(RIPPLED_VALIDATORS)} rippled + {len(RXRPL_VALIDATORS)} rxrpl; "
            f"re-harvest keys to change it"
        )

    os.makedirs(CONFIGS_DIR, exist_ok=True)
    peers = all_peers()

    for i, v in enumerate(RIPPLED_VALIDATORS):
        own = f"{v['ip']}:{PEER_PORT}"
        write_rippled_config(
            os.path.join(CONFIGS_DIR, f"rippled-{i}.cfg"),
            v["seed"],
            [p for p in peers if p != own],
        )

    for i, v in enumerate(RXRPL_VALIDATORS):
        own = f"{v['ip']}:{PEER_PORT}"
        write_rxrpl_config(
            os.path.join(CONFIGS_DIR, f"rxrpl-{i}.toml"),
            v,
            [p for p in peers if p != own],
        )

    write_validators_txt(os.path.join(CONFIGS_DIR, "validators.txt"))

    print(
        f"Generated configs for {len(RIPPLED_VALIDATORS)} rippled "
        f"+ {len(RXRPL_VALIDATORS)} rxrpl nodes"
    )
    print(f"  Configs:    {CONFIGS_DIR}/")
    print(f"  UNL keys:   {len(unl_keys())}")


if __name__ == "__main__":
    main()
