#!/usr/bin/env bash
# Run the full interop test suite.
#
# Usage:
#   ./interop/scripts/run_interop.sh [--rippled-image IMAGE] [--suite SUITE]
#       [--quorum N]
#
# Options:
#   --rippled-image   Docker image for rippled (default: rippleci/rippled:3.1.3)
#   --suite           Test suite: all, propagation, consensus, sync, chaos (default: all)
#   --quorum          Validation quorum (default: 4 for the full 5-node topology)
#
# Prerequisites:
#   - Docker and docker compose
#   - Python 3.10+ with requests (for local runs)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INTEROP_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$INTEROP_DIR")"

# Default to 3.1.3 — the 2.3.0 image bootloops on its own startup
# sed-in-place against a read-only mounted rippled.cfg ("Device or
# resource busy") and never reaches the RPC port, so any test against
# it hangs through the entire pytest timeout.
RIPPLED_IMAGE="${RIPPLED_IMAGE:-rippleci/rippled:3.1.3}"
SUITE="all"
QUORUM="${INTEROP_QUORUM:-4}"

while [[ $# -gt 0 ]]; do
    case $1 in
        --rippled-image) RIPPLED_IMAGE="$2"; shift 2 ;;
        --suite) SUITE="$2"; shift 2 ;;
        --quorum) QUORUM="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

echo "=== rxrpl interop test suite ==="
echo "rippled image: $RIPPLED_IMAGE"
echo "test suite:    $SUITE"
echo "validation quorum: $QUORUM/5"
echo ""

# Step 1: Generate configs
echo "--- Generating network configs ---"
python3 "$INTEROP_DIR/scripts/generate_configs.py" --quorum "$QUORUM"

# rippled 2.x normalizes its config with sed -i. A bind-mounted individual
# file cannot be renamed by sed, so give each validator a private temporary
# directory containing the generated config and validators file. Newer images
# also work with this layout, and the checked-in fixtures remain untouched.
RIPPLED_CONFIG_TMP="$(mktemp -d /tmp/rxrpl-rippled-configs.XXXXXX)"
for i in 0 1 2; do
    mkdir -p "$RIPPLED_CONFIG_TMP/$i"
    cp "$INTEROP_DIR/configs/rippled-$i.cfg" "$RIPPLED_CONFIG_TMP/$i/rippled.cfg"
    cp "$INTEROP_DIR/configs/validators.txt" "$RIPPLED_CONFIG_TMP/$i/validators.txt"
    export "RIPPLED_CONFIG_DIR_$i=$RIPPLED_CONFIG_TMP/$i"
done

# Step 2: Build rxrpl image
echo "--- Building rxrpl Docker image ---"
docker build -t rxrpl:interop -f "$PROJECT_ROOT/Dockerfile" "$PROJECT_ROOT"

# Step 3: Start the network
# Only the node services — `test-runner` carries a static IP and is run
# on demand below via `docker compose run`; starting it here too would
# collide with that ephemeral container on the same address.
echo "--- Starting mixed network ---"
export RIPPLED_IMAGE
cd "$INTEROP_DIR"
cleanup() {
    echo "--- Tearing down network ---"
    docker compose -f docker-compose.yml down -v --remove-orphans >/dev/null 2>&1 || true
    rm -r "$RIPPLED_CONFIG_TMP" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker compose -f docker-compose.yml up -d --build rippled-0 rippled-1 rippled-2 rxrpl-0 rxrpl-1

# Step 4: Run tests
echo "--- Running interop tests (suite: $SUITE) ---"
PYTEST_ARGS=()
case "$SUITE" in
    propagation) PYTEST_ARGS=(tests/test_propagation.py) ;;
    consensus)   PYTEST_ARGS=(tests/test_consensus.py) ;;
    sync)        PYTEST_ARGS=(tests/test_sync.py) ;;
    all)
        # Keep destructive crash/restart scenarios last. They intentionally
        # perturb the shared topology; running them before propagation/sync
        # lets a still-recovering validator make unrelated tests flaky.
        PYTEST_ARGS=(
            tests/test_configs.py
            tests/test_consensus.py
            tests/test_consensus_mixed_voter.py
            tests/test_propagation.py
            tests/test_sync.py
            tests/test_flaky_rippled.py
            tests/test_flaky_rxrpl.py
        )
        ;;
    chaos)
        PYTEST_ARGS=(
            tests/test_consensus_mixed_voter.py
            tests/test_flaky_rippled.py
            tests/test_flaky_rxrpl.py
        )
        ;;
    *)           echo "Unknown suite: $SUITE"; exit 1 ;;
esac

# Run tests via the test-runner container. The image entrypoint is
# `python -m pytest`, so pass only the pytest arguments here.
set +e
docker compose -f docker-compose.yml run --build --rm test-runner \
    "${PYTEST_ARGS[@]}" -v --tb=short
TEST_EXIT=$?
set -e

# Step 5: Collect logs on failure
if [ $TEST_EXIT -ne 0 ]; then
    echo ""
    echo "--- Tests FAILED. Collecting logs ---"
    for svc in rippled-0 rippled-1 rippled-2 rxrpl-0 rxrpl-1; do
        echo ""
        echo "=== $svc (last 30 lines) ==="
        docker compose -f docker-compose.yml logs --tail=30 "$svc" 2>/dev/null || true
    done
fi

echo ""
echo "--- Interop run complete; cleanup is handled by the exit trap ---"

exit $TEST_EXIT
