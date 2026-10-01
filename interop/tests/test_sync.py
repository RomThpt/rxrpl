"""Ledger sync tests.

Verify that an rxrpl node joining a network late can sync up with
existing rippled validators and reach the same ledger state.
"""

import subprocess
import time

import pytest

from conftest import (
    ALL_URLS,
    RIPPLED_URLS,
    RXRPL_URLS,
    get_account_info,
    rpc,
    submit_payment,
    wait_for_account_info,
    wait_for_ledger,
    wait_for_ledger_hash,
)
from docker_helpers import (
    RXRPL_CONTAINERS,
    require_docker,
    start_container,
    stop_container,
    wait_until_running,
)

DEST_PRESYNC = "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"
# Reuse the known-good account fixture so the late-join test exercises ledger
# activity rather than an address-validation failure.
DEST_LATE_JOIN = DEST_PRESYNC


@pytest.mark.network
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
            info = wait_for_account_info(url, DEST_PRESYNC, timeout=60)
            assert info is not None, f"Account not found on {url}"
            balances[url] = info.get("Balance", "0")

        unique_balances = set(balances.values())
        assert len(unique_balances) == 1, \
            f"Balance mismatch: {balances}"

    def test_late_join_catches_up_after_activity(self):
        """A stopped validator rejoins after new ledgers and catches up."""
        require_docker()
        if not RXRPL_CONTAINERS:
            pytest.skip("RXRPL_CONTAINERS not set; cannot run late-join test")

        target_name = RXRPL_CONTAINERS[-1]
        target_url = RXRPL_URLS[-1]
        for url in ALL_URLS:
            wait_for_ledger(url, 12, timeout=180)
        baseline_seq = max(
            wait_for_ledger(url, 12, timeout=30) for url in ALL_URLS
        )

        stopped = False
        try:
            stop_container(target_name)
            stopped = True

            payment = submit_payment(RIPPLED_URLS[0], DEST_LATE_JOIN, "700000000")
            assert payment.get("engine_result") == "tesSUCCESS" or \
                payment.get("status") == "success"

            target_seq = baseline_seq + 5
            survivors = [url for url in ALL_URLS if url != target_url]
            for url in survivors:
                seq = wait_for_ledger(url, target_seq, timeout=240)
                assert seq >= target_seq, (
                    f"surviving node {url} did not advance while late joiner "
                    f"was stopped: reached {seq}, expected >= {target_seq}"
                )

            start_container(target_name)
            stopped = False
            wait_until_running(target_name)
            joined_seq = wait_for_ledger(target_url, target_seq, timeout=300)
            assert joined_seq >= target_seq, (
                f"late joiner {target_url} failed to catch up: "
                f"reached {joined_seq}, expected >= {target_seq}"
            )

            hashes = {
                url: wait_for_ledger_hash(url, target_seq, timeout=120)
                for url in ALL_URLS
            }
            assert all(hashes.values()), f"missing late-join hashes: {hashes}"
            assert len(set(hashes.values())) == 1, (
                "late-join hash mismatch: "
                + ", ".join(f"{url}={value[:16]}..." for url, value in hashes.items())
            )

            account_data = {
                url: wait_for_account_info(url, DEST_LATE_JOIN, timeout=120)
                for url in ALL_URLS
            }
            assert all(account_data.values()), (
                f"late-join account state missing: {account_data}"
            )
            balances = {data["Balance"] for data in account_data.values()}
            assert len(balances) == 1, (
                f"late-join account state mismatch: {account_data}"
            )
        finally:
            if stopped:
                running = subprocess.run(
                    ["docker", "inspect", "-f", "{{.State.Running}}", target_name],
                    capture_output=True,
                    timeout=5,
                )
                if running.returncode == 0 and running.stdout.strip() != b"true":
                    start_container(target_name)
                    wait_until_running(target_name)

    def test_ledger_history_matches(self):
        """Verify multiple historical ledger hashes match across implementations."""
        # Advance to at least ledger 20
        for url in ALL_URLS:
            wait_for_ledger(url, 20, timeout=120)

        # Check ledger hashes at multiple points
        for check_seq in [5, 10, 15]:
            hashes = {}
            for url in ALL_URLS:
                h = wait_for_ledger_hash(url, check_seq, timeout=60)
                assert h is not None, (
                    f"Node {url} did not serve historical ledger {check_seq}"
                )
                hashes[url] = h

            unique = set(hashes.values())
            assert len(unique) == 1, (
                f"Hash mismatch at ledger {check_seq}: "
                + ", ".join(f"{u}={h[:16]}..." for u, h in hashes.items())
            )

    def test_peer_connectivity(self):
        """All nodes can see their peers."""
        for url in ALL_URLS:
            deadline = time.time() + 60
            last_error = None
            while time.time() < deadline:
                try:
                    result = rpc(url, "peers")
                    peers = result.get("peers", [])
                    if peers:
                        break
                    last_error = "RPC returned zero peers"
                except Exception as exc:
                    last_error = repr(exc)
                time.sleep(2)
            else:
                pytest.fail(f"Node {url} has no peers after 60s: {last_error}")
