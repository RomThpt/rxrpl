#!/usr/bin/env python3
"""Generate interop test network configurations.

Creates configs for a mixed rippled + rxrpl private validator network.
All nodes share the same UNL (trusted validator list) and connect
to each other as fixed peers.

Usage:
    python scripts/generate_configs.py [--rippled N] [--rxrpl N]
"""

import argparse
import base64
import hashlib
import json
import os
import secrets

CONFIGS_DIR = os.path.join(os.path.dirname(__file__), "..", "configs")


def _try_import_secp256k1_signer():
    """Return a callable (priv_hex, msg_bytes) -> (sig_hex, pub_hex) or None.

    Prefers the `ecdsa` library which is broadly available; falls back to None
    if no secp256k1 implementation is reachable.
    """
    try:
        from ecdsa import SigningKey, SECP256k1
        from ecdsa.util import sigencode_der_canonize

        def sign(priv_hex: str, msg: bytes) -> tuple[str, str]:
            sk = SigningKey.from_string(bytes.fromhex(priv_hex), curve=SECP256k1)
            vk = sk.get_verifying_key()
            # Compressed pubkey (33 bytes) per SEC1.
            pub_pt = vk.pubkey.point
            x = pub_pt.x().to_bytes(32, "big")
            prefix = b"\x02" if pub_pt.y() % 2 == 0 else b"\x03"
            compressed = prefix + x
            sig = sk.sign_deterministic(
                msg,
                hashfunc=hashlib.sha256,
                sigencode=sigencode_der_canonize,
            )
            return sig.hex().upper(), compressed.hex().upper()

        return sign
    except Exception:
        return None


def generate_publisher_key() -> dict:
    """Create a fresh test publisher secp256k1 keypair.

    Returns dict with hex-encoded `secret_key` (32 bytes) and `public_key`
    (33-byte compressed). If no secp256k1 library is available, falls back
    to a deterministic stub (still 33 bytes, but not a real curve point).
    """
    secret = secrets.token_hex(32)
    signer = _try_import_secp256k1_signer()
    if signer is None:
        # Deterministic stub pubkey for environments without ecdsa.
        h = hashlib.sha256(secret.encode()).hexdigest()
        return {
            "secret_key": secret,
            "public_key": ("02" + h)[:66].upper(),
            "_stub": True,
        }
    _, pub_hex = signer(secret, b"probe")
    return {"secret_key": secret, "public_key": pub_hex}


def write_publisher(path: str, key: dict):
    with open(path, "w") as f:
        json.dump(key, f, indent=2)


def write_manifest(path: str, publisher: dict, validators: list[dict],
                   sequence: int = 1):
    """Sign and write the shared validator-list manifest.

    The signed payload is the canonical JSON of `validators + sequence`.
    Format mirrors rippled's UTE envelope at a JSON layer (B1 scope: parse
    + presence; full STValidatorList byte-encoding deferred to B3).
    """
    blob = {
        "sequence": sequence,
        "validators": validators,
    }
    blob_bytes = json.dumps(blob, sort_keys=True, separators=(",", ":")).encode()
    blob_b64 = base64.b64encode(blob_bytes).decode()

    signer = _try_import_secp256k1_signer()
    if signer is None:
        # Stub signature: SHA-256 of (priv || blob).  Deterministic, parse-able.
        h = hashlib.sha256(
            (publisher["secret_key"] + blob_b64).encode()
        ).hexdigest()
        sig_hex = h.upper()
        signing_pubkey = publisher["public_key"]
    else:
        sig_hex, signing_pubkey = signer(publisher["secret_key"], blob_bytes)

    manifest = {
        "version": 2,
        "sequence": sequence,
        "validators": validators,
        "blob": blob_b64,
        "signature": sig_hex,
        "signing_pubkey": signing_pubkey,
    }
    with open(path, "w") as f:
        json.dump(manifest, f, indent=2)


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

    # Generate test publisher key + signed manifest binding all validators.
    publisher = generate_publisher_key()
    write_publisher(os.path.join(CONFIGS_DIR, "publisher.json"), publisher)

    validator_entries = []
    for i, key in enumerate(public_keys):
        role = "rippled" if i < args.rippled else "rxrpl"
        validator_entries.append({"public_key": key, "role": role})
    write_manifest(
        os.path.join(CONFIGS_DIR, "manifest.json"),
        publisher,
        validator_entries,
    )

    print(f"Generated configs for {args.rippled} rippled + {args.rxrpl} rxrpl nodes")
    print(f"  Configs: {CONFIGS_DIR}/")
    print(f"  Validators: {len(public_keys)} total")
    print(f"  Publisher: {publisher['public_key'][:16]}...")


if __name__ == "__main__":
    main()
