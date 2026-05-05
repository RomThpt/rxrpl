"""B5: Flaky rippled — kill one rippled, rxrpl + remaining rippled defer
quorum without panic, then resume once rippled comes back.

In a 2-of-3 quorum with 2 rippled + 1 rxrpl, killing one rippled leaves
1 rippled + 1 rxrpl = 2 votes available. Whether quorum still forms or
not depends on the exact UNL/quorum configuration: the important
invariant for this test is that **no node panics** and that the network
fully reconverges once the missing rippled returns.
"""

import time

import pytest

from conftest import ALL_URLS, RIPPLED_URLS, RXRPL_URLS, get_ledger_hash, wait_for_ledger
from docker_helpers import (
    RIPPLED_CONTAINERS,
    container_logs,
    require_docker,
    start_container,
    stop_container,
    wait_until_running,
)


@pytest.mark.network
class TestFlakyRippled:
    def test_rippled_crash_and_recover(self):
        """Kill rippled-0, network does not panic, restart, all reconverge."""
        require_docker()
        if not RIPPLED_CONTAINERS:
            pytest.skip("RIPPLED_CONTAINERS not set; cannot orchestrate chaos")

        # 1. Baseline: everyone past ledger 8.
        for url in ALL_URLS:
            wait_for_ledger(url, 8, timeout=180)
        baseline_seq = wait_for_ledger(RIPPLED_URLS[0], 8, timeout=10)

        # 2. Kill rippled-0 (the first rippled validator).
        target = RIPPLED_CONTAINERS[0]
        stop_container(target)

        # 3. Give the network up to ~30s of partition time. The remaining
        #    rxrpl + rippled-1 may or may not form quorum depending on the
        #    quorum threshold; either way, no node should panic, and we
        #    expect the rxrpl yield-to-peer logic to defer rather than
        #    solo-close at a divergent hash.
        time.sleep(30)

        # 4. Surviving nodes must still respond to RPC and not have panicked.
        survivors = [RIPPLED_URLS[1]] + RXRPL_URLS
        for url in survivors:
            # wait_for_ledger short timeout: we just want the node alive.
            wait_for_ledger(url, baseline_seq, timeout=15)

        # 5. Restart rippled-0. Quorum must reform and all 3 must advance.
        start_container(target)
        wait_until_running(target)
        time.sleep(5)

        recovery_seq = baseline_seq + 5
        for url in ALL_URLS:
            seq = wait_for_ledger(url, recovery_seq, timeout=240)
            assert seq >= recovery_seq, (
                f"node {url} failed to advance after rippled recovery: "
                f"reached {seq}, expected >= {recovery_seq}"
            )

        # 6. Hash agreement at a ledger closed *after* recovery.
        check_seq = baseline_seq + 2
        hashes = {url: get_ledger_hash(url, check_seq) for url in ALL_URLS}
        for url, h in hashes.items():
            assert h is not None, f"{url} missing hash for ledger {check_seq}"
        unique = set(hashes.values())
        assert len(unique) == 1, (
            f"Post-recovery hash disagreement at seq {check_seq}: "
            + ", ".join(f"{u}={h[:16]}..." for u, h in hashes.items())
        )

    def test_rxrpl_did_not_panic_during_partition(self):
        """rxrpl logs must not contain panic/abort markers during the run.

        Runs after the chaos test above so the run window includes the
        partition+recovery cycle.
        """
        require_docker()
        from docker_helpers import RXRPL_CONTAINERS
        if not RXRPL_CONTAINERS:
            pytest.skip("RXRPL_CONTAINERS not set")
        for name in RXRPL_CONTAINERS:
            logs = container_logs(name, tail=4000)
            forbidden = ["panicked at", "fatal runtime error", "thread '"]
            # `thread 'main' panicked` is the canonical Rust panic marker;
            # we match the loose prefix to also catch worker-thread panics.
            for marker in forbidden:
                if marker == "thread '":
                    # Only flag if actually followed by ` panicked`.
                    assert " panicked" not in logs, (
                        f"rxrpl container {name} reports a panic in logs"
                    )
                else:
                    assert marker not in logs, (
                        f"rxrpl container {name} log contains forbidden "
                        f"marker {marker!r}"
                    )
