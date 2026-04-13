#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

BACKEND_PORT="${BACKEND_PORT:-3001}"
MITMPROXY_PORT="${MITMPROXY_PORT:-3002}"
UPSTREAM_URL="${UPSTREAM_URL:-http://127.0.0.1:${BACKEND_PORT}}"

exec mitmdump \
  --listen-host 127.0.0.1 \
  --listen-port "${MITMPROXY_PORT}" \
  --mode "reverse:${UPSTREAM_URL}" \
  -s "${ROOT_DIR}/scripts/mitmproxy/ws_chaos.py"
