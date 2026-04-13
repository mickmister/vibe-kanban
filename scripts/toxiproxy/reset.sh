#!/usr/bin/env bash

set -euo pipefail

TOXIPROXY_API_URL="${TOXIPROXY_API_URL:-http://127.0.0.1:8474}"

curl -fsS \
  -H 'Content-Type: application/json' \
  -X POST \
  "${TOXIPROXY_API_URL}/reset" \
  >/dev/null

echo "Reset toxiproxy: enabled all proxies and removed all toxics"
