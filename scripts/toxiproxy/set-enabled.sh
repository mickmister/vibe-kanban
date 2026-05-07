#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 up|down" >&2
  exit 1
fi

case "$1" in
  up)
    enabled=true
    ;;
  down)
    enabled=false
    ;;
  *)
    echo "Usage: $0 up|down" >&2
    exit 1
    ;;
esac

TOXIPROXY_API_URL="${TOXIPROXY_API_URL:-http://127.0.0.1:8474}"
TOXIPROXY_PROXY_NAME="${TOXIPROXY_PROXY_NAME:-vk_local_ws}"

curl -fsS \
  -H 'Content-Type: application/json' \
  -X POST \
  -d "{\"enabled\":${enabled}}" \
  "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}" \
  >/dev/null

echo "Set proxy '${TOXIPROXY_PROXY_NAME}' enabled=${enabled}"
