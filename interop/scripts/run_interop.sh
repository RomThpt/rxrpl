#!/usr/bin/env bash
# Run the full interop test suite.
#
# Usage:
#   ./interop/scripts/run_interop.sh [--rippled-image IMAGE] [--suite SUITE]
#
# Options:
#   --rippled-image   Docker image for rippled (default: rippleci/rippled:2.3.0)
#   --suite           Test suite: all, propagation, consensus, sync (default: all)
#
# Prerequisites:
#   - Docker and docker compose
#   - Python 3.10+ with requests (for local runs)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INTEROP_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$INTEROP_DIR")"

RIPPLED_IMAGE="${RIPPLED_IMAGE:-rippleci/rippled:2.3.0}"
SUITE="all"

while [[ $# -gt 0 ]]; do
    case $1 in
        --rippled-image) RIPPLED_IMAGE="$2"; shift 2 ;;
        --suite) SUITE="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

echo "=== rxrpl interop test suite ==="
echo "rippled image: $RIPPLED_IMAGE"
echo "test suite:    $SUITE"
echo ""

# Step 1: Generate configs
echo "--- Generating network configs ---"
python3 "$INTEROP_DIR/scripts/generate_configs.py"

# Step 2: Build rxrpl image
echo "--- Building rxrpl Docker image ---"
docker build -t rxrpl:interop -f "$PROJECT_ROOT/Dockerfile" "$PROJECT_ROOT"

# Step 3: Start the network
echo "--- Starting mixed network ---"
export RIPPLED_IMAGE
cd "$INTEROP_DIR"
docker compose -f docker-compose.yml up -d --build

# Step 4: Run tests
echo "--- Running interop tests (suite: $SUITE) ---"
PYTEST_ARGS=""
case "$SUITE" in
    propagation) PYTEST_ARGS="tests/test_propagation.py" ;;
    consensus)   PYTEST_ARGS="tests/test_consensus.py" ;;
    sync)        PYTEST_ARGS="tests/test_sync.py" ;;
    all)         PYTEST_ARGS="tests/" ;;
    *)           echo "Unknown suite: $SUITE"; exit 1 ;;
esac

# Run tests via the test-runner container, or locally if --local
docker compose -f docker-compose.yml run --rm test-runner \
    python -m pytest "$PYTEST_ARGS" -v --tb=short
TEST_EXIT=$?

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

# Step 6: Teardown
echo ""
echo "--- Tearing down network ---"
docker compose -f docker-compose.yml down -v --remove-orphans

exit $TEST_EXIT
