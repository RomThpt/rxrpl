"""Transaction propagation tests.

Verify that transactions submitted to one implementation are visible
on nodes of the other implementation after validation.
"""

import time

from conftest import (
    RIPPLED_URLS,
    RXRPL_URLS,
    get_account_info,
    submit_payment,
    wait_for_ledger,
)

# Deterministic test destination addresses
DEST_RIPPLED_TO_RXRPL = "rPMh7Pi9ct699iZUTWzJaUPfRhPgenRpBk"
DEST_RXRPL_TO_RIPPLED = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV"


class TestPropagation:
    """Test transaction propagation between rippled and rxrpl nodes."""

    def test_rippled_to_rxrpl(self):
        """Submit a payment via rippled, verify it appears on rxrpl."""
        source = RIPPLED_URLS[0]
        target = RXRPL_URLS[0]

        # Submit payment through rippled
        result = submit_payment(source, DEST_RIPPLED_TO_RXRPL, "200000000")
        assert result.get("engine_result") == "tesSUCCESS" or \
               result.get("status") == "success", \
               f"Submit failed: {result}"

        # Wait for a few ledgers to close
        current = wait_for_ledger(source, 0, timeout=10)
        wait_for_ledger(target, current + 2, timeout=60)

        # Verify the account exists on the rxrpl node
        info = get_account_info(target, DEST_RIPPLED_TO_RXRPL)
        assert info is not None, \
            f"Account {DEST_RIPPLED_TO_RXRPL} not found on rxrpl after propagation"
        assert int(info.get("Balance", "0")) > 0

    def test_rxrpl_to_rippled(self):
        """Submit a payment via rxrpl, verify it appears on rippled."""
        source = RXRPL_URLS[0]
        target = RIPPLED_URLS[0]

        # Submit payment through rxrpl
        result = submit_payment(source, DEST_RXRPL_TO_RIPPLED, "200000000")
        assert result.get("engine_result") == "tesSUCCESS" or \
               result.get("status") == "success", \
               f"Submit failed: {result}"

        # Wait for propagation
        current = wait_for_ledger(source, 0, timeout=10)
        wait_for_ledger(target, current + 2, timeout=60)

        # Verify the account exists on the rippled node
        info = get_account_info(target, DEST_RXRPL_TO_RIPPLED)
        assert info is not None, \
            f"Account {DEST_RXRPL_TO_RIPPLED} not found on rippled after propagation"
        assert int(info.get("Balance", "0")) > 0

    def test_bidirectional_visibility(self):
        """Verify all nodes see the same accounts after propagation."""
        # By now both destinations should exist on all nodes
        time.sleep(5)

        from conftest import ALL_URLS
        for url in ALL_URLS:
            for dest in [DEST_RIPPLED_TO_RXRPL, DEST_RXRPL_TO_RIPPLED]:
                info = get_account_info(url, dest)
                assert info is not None, \
                    f"Account {dest} not found on {url}"
