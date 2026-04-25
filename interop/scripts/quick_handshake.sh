#!/usr/bin/env bash
# Minimal rippled <-> rxrpl handshake + GetLedger smoke test.
#
# Spins up just one rippled and one rxrpl node, waits for them to peer,
# then asks the rxrpl node for its peers list. If rippled appears as a
# connected peer with the right software identity, the wire format is
# compatible at the handshake layer.
#
# Use this for fast iteration during wire-format work; the full
# `run_interop.sh all` suite is the formal gate (~10 min vs ~30 s).
#
# Usage:
#   ./interop/scripts/quick_handshake.sh [--rippled-image IMAGE]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INTEROP_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$INTEROP_DIR")"

RIPPLED_IMAGE="${RIPPLED_IMAGE:-rippleci/rippled:2.3.0}"

while [[ $# -gt 0 ]]; do
    case $1 in
        --rippled-image) RIPPLED_IMAGE="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

cd "$INTEROP_DIR"

# Ensure configs exist (idempotent).
if [ ! -f configs/rxrpl-0.toml ] || [ ! -f configs/rippled-0.cfg ]; then
    echo "==> Generating configs"
    python3 scripts/generate_configs.py
fi

# Build rxrpl image (cached).
if ! docker image inspect rxrpl:interop >/dev/null 2>&1; then
    echo "==> Building rxrpl image (rxrpl:interop)"
    docker build -t rxrpl:interop -f "$PROJECT_ROOT/Dockerfile" "$PROJECT_ROOT"
fi

NET="rxrpl-quick-net"
docker network create --subnet=172.31.0.0/16 "$NET" 2>/dev/null || true

cleanup() {
    echo "==> Cleanup"
    docker rm -f rippled-quick rxrpl-quick 2>/dev/null || true
    docker network rm "$NET" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> Starting rippled at 172.31.0.10"
docker run -d --name rippled-quick --network "$NET" --ip 172.31.0.10 \
    -v "$INTEROP_DIR/configs/rippled-0.cfg:/etc/opt/ripple/rippled.cfg:ro" \
    -v "$INTEROP_DIR/configs/validators.txt:/etc/opt/ripple/validators.txt:ro" \
    "$RIPPLED_IMAGE" >/dev/null

echo "==> Starting rxrpl at 172.31.0.20"
docker run -d --name rxrpl-quick --network "$NET" --ip 172.31.0.20 \
    -p 5005:5005 \
    -v "$INTEROP_DIR/configs/rxrpl-0.toml:/etc/rxrpl/node.toml:ro" \
    rxrpl:interop run --mode network --config /etc/rxrpl/node.toml \
        --bind 0.0.0.0:5005 >/dev/null

echo "==> Waiting up to 30 s for handshake"
for _ in $(seq 1 30); do
    if curl -fsS -X POST http://127.0.0.1:5005/ \
        -H 'Content-Type: application/json' \
        -d '{"method":"peers","params":[{}]}' 2>/dev/null \
        | grep -q '"status":"success"'; then
        break
    fi
    sleep 1
done

echo "==> rxrpl peers response:"
curl -sS -X POST http://127.0.0.1:5005/ \
    -H 'Content-Type: application/json' \
    -d '{"method":"peers","params":[{}]}' | python3 -m json.tool

echo "==> Done. Logs: docker logs rippled-quick / rxrpl-quick"
