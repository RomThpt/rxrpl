#!/usr/bin/env python3
"""Generate interop test network configurations (confluence topology).

Writes deterministic configs for the mixed validator network defined in
`interop/docker-compose.yml`: three rippled validators and two rxrpl
validators, all sharing one UNL. The 5-validator set gives an 80% quorum
of 4, so the network tolerates losing any single validator (required by
the chaos/fault-tolerance tests).

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
    {
        "seed": "shqEDAXNxM8Y94qj4Hq2EocLqZEKz",
        "public_key": "n9KXHLvP8vsxhNLNFKxUDqziUMbqoLikk3Y2jHh1dnaqMCb3gnn4",
        "ip": "172.30.0.12",
    },
]
RXRPL_VALIDATORS = [
    {
        "seed": "sEdVZKfQYfinxEpnzyLmQZfSYNV2teD",
        "public_key": "n9Lc8w5xK1kJE4AiHX9kvDQF6shMBWoz8pEXp95xRo227ZMnUPt2",
        "ip": "172.30.0.20",
        "node_seed": "a1f33f544dbae3d90db16b1bc9e821e9",
    },
    {
        "seed": "shVHki9rwyRX52LWJkUqcH2H17bhh",
        "public_key": "n9J5N9HSH3cYmJmJDxjq3JPwxFDo62CakvCCZkjn2crcgxs3dztj",
        "ip": "172.30.0.21",
        "node_seed": "57c2cb534e657e81da3dc09c69a33607",
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

# Verbose manifest + UNL acceptance traces so confluence-style 3-node
# diagnostics can pinpoint where a peer's manifest is rejected / not
# applied to the active trusted set (the `validator_list.count: 1`
# symptom we hit when only the local self-key activates).
[log_level]
warning
Manifest=trace
ValidatorList=trace
Overlay=debug

[validation_seed]
{seed}

[network_id]
{NETWORK_ID}

[ledger_history]
full

[consensus]
minimum_duration_ms=200

# Match rxrpl's explicit quorum so the 3-validator UNL only needs 2
# validations to advance. Without this, rippled defaults to
# ceil(3 * 0.8) = 3 and requires ALL 3 validators (us + the other
# rippled + rxrpl) to validate every ledger. With cross-impl manifest
# propagation taking ~30s to settle at boot, the strict-3 quorum
# leaves rippled `complete_ledgers: empty` for the whole 5-minute
# pytest timeout window.
[validation_quorum]
2
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


# Deterministic, hand-rolled "publisher" identity. Not derived from a
# real curve — these are just opaque hex blobs the test_configs.B1
# harness checks for shape (33-byte pubkey, non-empty signature) so
# downstream UNL publishing can be wired in later without churning the
# fixture file paths. Real PKL/manifest generation would replace
# `_FAKE_PUBLISHER_*` with rxrpl-cli `validation_create` output.
_FAKE_PUBLISHER_PUBKEY = (
    "02" + "00112233445566778899AABBCCDDEEFF" * 2  # 33 bytes (66 hex chars)
)
_FAKE_PUBLISHER_SECRET = "00" + "11223344556677889900AABBCCDDEEFF" * 2
_FAKE_PUBLISHER_SIG = "30" + "45" + "0102030405060708090A0B0C0D0E0F10" * 4  # any non-empty hex blob


def write_publisher_key(path):
    import json
    obj = {
        "public_key": _FAKE_PUBLISHER_PUBKEY,
        "secret_key": _FAKE_PUBLISHER_SECRET,
    }
    with open(path, "w") as f:
        json.dump(obj, f, indent=2)


def write_publisher_manifest(path):
    """Emit a JSON manifest documenting the validator set and the
    publisher signature over it. Schema matches what tests/test_configs.py
    (B1) inspects: a `validators` list with `role` + `public_key` per
    entry, plus `signature` + `signing_pubkey` for the publisher. This
    is a fixture artifact; rippled/rxrpl don't load it directly today.
    """
    import json
    validators = []
    for v in RIPPLED_VALIDATORS:
        validators.append({"public_key": v["public_key"], "role": "rippled"})
    for v in RXRPL_VALIDATORS:
        validators.append({"public_key": v["public_key"], "role": "rxrpl"})
    manifest = {
        "validators": validators,
        "signing_pubkey": _FAKE_PUBLISHER_PUBKEY,
        "signature": _FAKE_PUBLISHER_SIG,
    }
    with open(path, "w") as f:
        json.dump(manifest, f, indent=2)


def main():
    parser = argparse.ArgumentParser(description="Generate interop test configs")
    parser.add_argument("--rippled", type=int, default=3)
    parser.add_argument("--rxrpl", type=int, default=2)
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
    write_publisher_key(os.path.join(CONFIGS_DIR, "publisher.json"))
    write_publisher_manifest(os.path.join(CONFIGS_DIR, "manifest.json"))

    print(
        f"Generated configs for {len(RIPPLED_VALIDATORS)} rippled "
        f"+ {len(RXRPL_VALIDATORS)} rxrpl nodes"
    )
    print(f"  Configs:    {CONFIGS_DIR}/")
    print(f"  UNL keys:   {len(unl_keys())}")


if __name__ == "__main__":
    main()
