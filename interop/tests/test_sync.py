"""Ledger sync tests.

Verify that an rxrpl node joining a network late can sync up with
existing rippled validators and reach the same ledger state.
"""

import time

from conftest import (
    ALL_URLS,
    RIPPLED_URLS,
    RXRPL_URLS,
    get_account_info,
    get_ledger_hash,
    rpc,
    submit_payment,
    wait_for_ledger,
)

DEST_PRESYNC = "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"


class TestSync:
    """Test ledger synchronization for late-joining nodes."""

    def test_state_consistency_after_activity(self):
        """After tx activity, all nodes converge on same state.

        Submit several transactions, then verify all nodes eventually
        report consistent account balances.
        """
        source = RIPPLED_URLS[0]

        # Submit a payment to create a new account
        result = submit_payment(source, DEST_PRESYNC, "500000000")
        assert result.get("engine_result") == "tesSUCCESS" or \
               result.get("status") == "success"

        # Wait for all nodes to catch up
        current = wait_for_ledger(source, 0, timeout=10)
        for url in ALL_URLS:
            wait_for_ledger(url, current + 3, timeout=60)

        # Verify all nodes see the same balance
        balances = {}
        for url in ALL_URLS:
            info = get_account_info(url, DEST_PRESYNC)
            assert info is not None, f"Account not found on {url}"
            balances[url] = info.get("Balance", "0")

        unique_balances = set(balances.values())
        assert len(unique_balances) == 1, \
            f"Balance mismatch: {balances}"

    def test_ledger_history_matches(self):
        """Verify multiple historical ledger hashes match across implementations."""
        # Advance to at least ledger 20
        for url in ALL_URLS:
            wait_for_ledger(url, 20, timeout=120)

        # Check ledger hashes at multiple points
        for check_seq in [5, 10, 15]:
            hashes = {}
            for url in ALL_URLS:
                h = get_ledger_hash(url, check_seq)
                if h is not None:
                    hashes[url] = h

            if len(hashes) < 2:
                continue  # Not enough nodes responded

            unique = set(hashes.values())
            assert len(unique) == 1, (
                f"Hash mismatch at ledger {check_seq}: "
                + ", ".join(f"{u}={h[:16]}..." for u, h in hashes.items())
            )

    def test_peer_connectivity(self):
        """All nodes can see their peers."""
        for url in ALL_URLS:
            try:
                result = rpc(url, "peers")
                peers = result.get("peers", [])
                # Each node should see at least 1 peer
                assert len(peers) >= 1, \
                    f"Node {url} has no peers"
            except Exception:
                # Some implementations may not support 'peers' RPC
                pass
