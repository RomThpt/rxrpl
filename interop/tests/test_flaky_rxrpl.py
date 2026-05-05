"""B4: Flaky rxrpl — kill, network continues, rxrpl rejoins.

Scenario: with 2 rippled + 1 rxrpl on a 2-of-3 quorum, killing rxrpl
leaves rippled-0 + rippled-1 as a viable 2-of-3 quorum. The remaining
rippled validators must continue advancing ledgers, and rxrpl must catch
up and re-converge once it returns.
"""

import time

import pytest

from conftest import ALL_URLS, RIPPLED_URLS, RXRPL_URLS, get_ledger_hash, wait_for_ledger
from docker_helpers import (
    RXRPL_CONTAINERS,
    require_docker,
    start_container,
    stop_container,
    wait_until_running,
)


@pytest.mark.network
class TestFlakyRxrpl:
    def test_rxrpl_crash_and_rejoin(self):
        """Kill rxrpl, rippled keeps advancing, restart rxrpl, all reconverge."""
        require_docker()
        if not RXRPL_CONTAINERS:
            pytest.skip("RXRPL_CONTAINERS not set; cannot orchestrate chaos")

        # 1. Bring everyone past a baseline so we have a known good state.
        for url in ALL_URLS:
            wait_for_ledger(url, 8, timeout=180)

        baseline_seq = max(
            wait_for_ledger(url, 8, timeout=10) for url in RIPPLED_URLS
        )

        # 2. Kill rxrpl-0. With 2-of-3 quorum and 2 rippled remaining,
        #    rippled must keep closing ledgers without the rxrpl signature.
        target = RXRPL_CONTAINERS[0]
        stop_container(target)

        # Give rippled time to notice rxrpl is gone and continue. We want
        # to see at least 5 new ledgers without rxrpl participating.
        target_seq = baseline_seq + 5
        for url in RIPPLED_URLS:
            seq = wait_for_ledger(url, target_seq, timeout=180)
            assert seq >= target_seq, (
                f"rippled {url} failed to advance after rxrpl kill: "
                f"reached {seq}, expected >= {target_seq}"
            )

        # 3. Restart rxrpl and let it sync.
        start_container(target)
        wait_until_running(target)
        # Give rxrpl time to reconnect, catch up, and resume validating.
        # Validate it reaches the rippled tip (with some buffer).
        time.sleep(5)
        catchup_seq = target_seq + 2
        for url in RXRPL_URLS:
            seq = wait_for_ledger(url, catchup_seq, timeout=240)
            assert seq >= catchup_seq, (
                f"rxrpl {url} failed to catch up after restart: "
                f"reached {seq}, expected >= {catchup_seq}"
            )

        # 4. All 3 nodes agree on a settled ledger after recovery.
        check_seq = baseline_seq + 1
        hashes = {url: get_ledger_hash(url, check_seq) for url in ALL_URLS}
        for url, h in hashes.items():
            assert h is not None, f"{url} missing hash for ledger {check_seq}"
        unique = set(hashes.values())
        assert len(unique) == 1, (
            f"Post-recovery hash disagreement at seq {check_seq}: "
            + ", ".join(f"{u}={h[:16]}..." for u, h in hashes.items())
        )
