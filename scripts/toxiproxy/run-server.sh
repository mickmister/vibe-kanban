#!/usr/bin/env bash

set -euo pipefail

TOXIPROXY_PORT="${TOXIPROXY_PORT:-3002}"
TOXIPROXY_API_URL="${TOXIPROXY_API_URL:-http://127.0.0.1:8474}"
TOXIPROXY_API_PORT="${TOXIPROXY_API_URL##*:}"
TOXIPROXY_IMAGE="${TOXIPROXY_IMAGE:-ghcr.io/shopify/toxiproxy}"

if command -v toxiproxy-server >/dev/null 2>&1; then
  exec toxiproxy-server
fi

if command -v docker >/dev/null 2>&1; then
  exec docker run --rm -it \
    -p "127.0.0.1:${TOXIPROXY_API_PORT}:8474" \
    -p "127.0.0.1:${TOXIPROXY_PORT}:${TOXIPROXY_PORT}" \
    "${TOXIPROXY_IMAGE}"
fi

echo "Neither toxiproxy-server nor docker is available." >&2
echo "Install toxiproxy locally or run the official ghcr.io/shopify/toxiproxy image." >&2
exit 1
