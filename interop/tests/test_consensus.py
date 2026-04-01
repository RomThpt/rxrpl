"""Consensus tests.

Verify that a mixed validator set (rippled + rxrpl) reaches consensus
and all nodes agree on the same validated ledger hashes.
"""

from conftest import (
    ALL_URLS,
    RIPPLED_URLS,
    RXRPL_URLS,
    get_ledger_hash,
    rpc,
    wait_for_ledger,
)


class TestConsensus:
    """Test consensus across mixed rippled/rxrpl validator sets."""

    def test_mixed_validators_advance(self):
        """All nodes in the mixed network advance past ledger 10."""
        for url in ALL_URLS:
            seq = wait_for_ledger(url, 10, timeout=120)
            assert seq >= 10, f"Node {url} stuck at ledger {seq}"

    def test_ledger_hash_agreement(self):
        """All nodes agree on the same validated ledger hash.

        Advance to ledger 15, then check that all nodes report the
        same hash for ledger 10 (well behind the tip to ensure validation).
        """
        # Advance all nodes past ledger 15
        for url in ALL_URLS:
            wait_for_ledger(url, 15, timeout=120)

        # Compare ledger 10 hash across all nodes
        check_seq = 10
        hashes = {}
        for url in ALL_URLS:
            h = get_ledger_hash(url, check_seq)
            assert h is not None, f"Node {url} has no hash for ledger {check_seq}"
            hashes[url] = h

        unique_hashes = set(hashes.values())
        assert len(unique_hashes) == 1, (
            f"Ledger hash disagreement at seq {check_seq}: "
            + ", ".join(f"{url}={h[:16]}..." for url, h in hashes.items())
        )

    def test_server_info_consistent(self):
        """All nodes report similar server state."""
        infos = {}
        for url in ALL_URLS:
            result = rpc(url, "server_info")
            info = result.get("info", result)
            infos[url] = info

        # All nodes should report a validated ledger
        for url, info in infos.items():
            validated = info.get("validated_ledger")
            assert validated is not None, f"Node {url} has no validated ledger"

        # Network IDs should match
        network_ids = set()
        for url, info in infos.items():
            nid = info.get("network_id", info.get("networkID"))
            if nid is not None:
                network_ids.add(nid)

        # All should be on the same network (99 = test)
        if network_ids:
            assert len(network_ids) == 1, \
                f"Network ID mismatch: {network_ids}"
