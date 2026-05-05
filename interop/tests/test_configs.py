"""B1: Harness + config generation tests.

Verifies that generate_configs.py produces:
  - A test publisher key (for signing the shared VL)
  - A custom validator list (manifest.json) that includes both rippled
    and rxrpl master public keys
  - validators.txt + per-node configs that reference the publisher

These tests are filesystem-only; no network or Docker required.
"""

import json
import os
import subprocess
import sys

import pytest

INTEROP_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SCRIPTS_DIR = os.path.join(INTEROP_DIR, "scripts")
CONFIGS_DIR = os.path.join(INTEROP_DIR, "configs")
GENERATE = os.path.join(SCRIPTS_DIR, "generate_configs.py")


@pytest.fixture(scope="module")
def regenerate_configs():
    """Run generate_configs.py with a known node count before each test module."""
    subprocess.run(
        [sys.executable, GENERATE, "--rippled", "2", "--rxrpl", "1"],
        check=True,
        cwd=INTEROP_DIR,
    )
    yield


class TestConfigsB1:
    """B1 — VL generation + harness wiring."""

    def test_publisher_key_generated(self, regenerate_configs):
        """A publisher key file exists with a 33-byte secp256k1 pubkey + private seed."""
        path = os.path.join(CONFIGS_DIR, "publisher.json")
        assert os.path.isfile(path), f"missing publisher key file: {path}"
        with open(path) as f:
            pub = json.load(f)
        assert "public_key" in pub and "secret_key" in pub
        # secp256k1 compressed pubkey = 33 bytes = 66 hex chars
        assert len(pub["public_key"]) == 66, "publisher pubkey must be 33-byte compressed"

    def test_manifest_contains_mixed_validators(self, regenerate_configs):
        """The signed manifest lists exactly 2 rippled + 1 rxrpl entries."""
        path = os.path.join(CONFIGS_DIR, "manifest.json")
        assert os.path.isfile(path), f"missing manifest: {path}"
        with open(path) as f:
            manifest = json.load(f)
        assert "validators" in manifest
        validators = manifest["validators"]
        assert len(validators) == 3, f"expected 3 validators, got {len(validators)}"
        # Each entry must carry pubkey + role tag
        roles = sorted(v["role"] for v in validators)
        assert roles == ["rippled", "rippled", "rxrpl"], f"role mix: {roles}"

    def test_manifest_signature_present(self, regenerate_configs):
        """The manifest carries a publisher signature over the validator blob."""
        path = os.path.join(CONFIGS_DIR, "manifest.json")
        with open(path) as f:
            manifest = json.load(f)
        assert "signature" in manifest and len(manifest["signature"]) > 0
        assert "signing_pubkey" in manifest

    def test_validators_txt_lists_all_keys(self, regenerate_configs):
        """validators.txt contains all 3 master public keys."""
        path = os.path.join(CONFIGS_DIR, "validators.txt")
        with open(path) as f:
            content = f.read()
        # Count non-empty, non-section lines
        keys = [
            ln.strip() for ln in content.splitlines()
            if ln.strip() and not ln.strip().startswith("[")
        ]
        assert len(keys) == 3, f"expected 3 validator keys, got {len(keys)}: {keys}"

    def test_rxrpl_config_trusts_rippled_keys(self, regenerate_configs):
        """rxrpl-0.toml trusted set contains all 3 keys (self + 2 peers)."""
        path = os.path.join(CONFIGS_DIR, "rxrpl-0.toml")
        with open(path) as f:
            content = f.read()
        assert "trusted" in content
        # Trusted list is a TOML array; count quoted entries
        import re
        m = re.search(r"trusted\s*=\s*\[([^\]]*)\]", content)
        assert m, "trusted = [...] block not found"
        entries = [s for s in re.findall(r'"([^"]+)"', m.group(1))]
        assert len(entries) == 3, f"expected 3 trusted keys, got {len(entries)}"
