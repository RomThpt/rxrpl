"""Shared fixtures for interop tests."""

import os
import time

import pytest
import requests

RIPPLED_URLS = os.environ.get(
    "RIPPLED_URLS", "http://localhost:15005"
).split(",")

RXRPL_URLS = os.environ.get(
    "RXRPL_URLS", "http://localhost:25005"
).split(",")

ALL_URLS = RIPPLED_URLS + RXRPL_URLS

GENESIS_ACCOUNT = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"
GENESIS_SECRET = "snoPBrXtMeMyMHUVTgbuqAfg1SUTb"

TIMEOUT = 120  # seconds


def rpc(url: str, method: str, params: dict | None = None) -> dict:
    """Send a JSON-RPC request to a node."""
    body = {"method": method, "params": [params or {}]}
    resp = requests.post(url, json=body, timeout=10)
    resp.raise_for_status()
    return resp.json()["result"]


def wait_for_ledger(url: str, min_seq: int, timeout: int = TIMEOUT) -> int:
    """Wait until a node's validated ledger reaches min_seq."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            result = rpc(url, "server_info")
            info = result.get("info", result)
            seq = (
                info.get("validated_ledger", {}).get("seq", 0)
                or info.get("complete_ledgers", "0").split("-")[-1]
            )
            if int(seq) >= min_seq:
                return int(seq)
        except Exception:
            pass
        time.sleep(2)
    pytest.fail(f"Node {url} did not reach ledger {min_seq} within {timeout}s")


def wait_all_nodes_live(min_seq: int = 3, timeout: int = TIMEOUT):
    """Wait until all nodes have reached at least min_seq."""
    for url in ALL_URLS:
        wait_for_ledger(url, min_seq, timeout)


def submit_payment(url: str, destination: str, amount: str = "100000000") -> dict:
    """Submit a Payment tx from genesis account."""
    tx = {
        "TransactionType": "Payment",
        "Account": GENESIS_ACCOUNT,
        "Destination": destination,
        "Amount": amount,
        "Fee": "12",
    }
    return rpc(url, "submit", {
        "tx_json": tx,
        "secret": GENESIS_SECRET,
    })


def get_account_info(url: str, account: str) -> dict | None:
    """Get account info, returns None if account not found."""
    try:
        result = rpc(url, "account_info", {"account": account})
        if "account_data" in result:
            return result["account_data"]
        return None
    except Exception:
        return None


def get_ledger_hash(url: str, seq: int) -> str | None:
    """Get the validated ledger hash at a specific sequence."""
    try:
        result = rpc(url, "ledger", {
            "ledger_index": seq,
            "transactions": False,
        })
        return result.get("ledger", {}).get("ledger_hash")
    except Exception:
        return None


@pytest.fixture(scope="session", autouse=True)
def network_ready():
    """Ensure the network is live before running tests."""
    wait_all_nodes_live(min_seq=3, timeout=TIMEOUT)
