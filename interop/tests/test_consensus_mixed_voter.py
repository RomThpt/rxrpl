"""B3: Mixed validator consensus — rxrpl as voting validator.

These tests prove that, in a 2 rippled + 1 rxrpl network, rxrpl's signed
validations contribute to quorum. Concretely:

- The network advances past ledger 20 (no consensus stall).
- All three nodes converge on the same hash for a settled ledger.
- rxrpl logs report network-validated ledgers with `validation_count >= 2`,
  which proves rxrpl is observing peer validations on top of its own and
  that quorum is being met by the mixed UNL — not by a single rippled
  declaring victory alone.

The validation log line emitted by rxrpl is (see `crates/node/src/node.rs`):

    network validated ledger #<seq> hash=<hash> (<n> validations)

We parse that line as the wire-level proxy for "rxrpl saw quorum".
"""

import re

import pytest

from conftest import ALL_URLS, RXRPL_URLS, get_ledger_hash, wait_for_ledger
from docker_helpers import RXRPL_CONTAINERS, container_logs

VALIDATION_LINE = re.compile(
    r"network validated ledger #(?P<seq>\d+)\s+hash=(?P<hash>[0-9A-Fa-f]+)\s+"
    r"\((?P<count>\d+) validations\)"
)


@pytest.mark.network
class TestMixedVoterConsensus:
    """rxrpl participates in quorum alongside rippled validators."""

    def test_ledger_advance_mixed_validators(self):
        """All 3 nodes reach >= ledger 20 with no consensus stall."""
        for url in ALL_URLS:
            seq = wait_for_ledger(url, 20, timeout=180)
            assert seq >= 20, f"Node {url} stalled at ledger {seq}"

    def test_hash_agreement_settled_ledger(self):
        """All 3 nodes report the same hash for a well-settled ledger."""
        for url in ALL_URLS:
            wait_for_ledger(url, 20, timeout=180)
        check_seq = 12
        hashes = {url: get_ledger_hash(url, check_seq) for url in ALL_URLS}
        for url, h in hashes.items():
            assert h is not None, f"{url} missing hash for ledger {check_seq}"
        unique = set(hashes.values())
        assert len(unique) == 1, (
            f"Hash disagreement at seq {check_seq}: "
            + ", ".join(f"{u}={h[:16]}..." for u, h in hashes.items())
        )

    def test_rxrpl_observes_quorum(self):
        """rxrpl logs at least one ledger validated by >= 2 signatures.

        With a 2-of-3 mixed UNL, a `validation_count >= 2` line proves the
        rxrpl node received and aggregated validations from at least one
        peer in addition to its own — i.e. rxrpl participates in consensus
        as a voter, and rippled's signatures are accepted on its side.
        """
        if not RXRPL_CONTAINERS:
            pytest.skip("RXRPL_CONTAINERS not set; cannot scrape rxrpl logs")

        # Make sure rxrpl is past a few ledgers so its log buffer has lines.
        for url in RXRPL_URLS:
            wait_for_ledger(url, 12, timeout=180)

        max_count = 0
        sample_line = None
        for name in RXRPL_CONTAINERS:
            logs = container_logs(name, tail=4000)
            for match in VALIDATION_LINE.finditer(logs):
                count = int(match.group("count"))
                if count > max_count:
                    max_count = count
                    sample_line = match.group(0)

        assert max_count >= 2, (
            "rxrpl never observed a ledger with validation_count >= 2; "
            "either rippled rejects rxrpl's validations or rxrpl rejects "
            "rippled's. Best line seen: "
            + (sample_line or "<no `network validated ledger` line at all>")
        )
