#!/usr/bin/env bash

set -euo pipefail

TOXIPROXY_API_URL="${TOXIPROXY_API_URL:-http://127.0.0.1:8474}"
TOXIPROXY_PROXY_NAME="${TOXIPROXY_PROXY_NAME:-vk_local_ws}"
TOXIPROXY_LISTEN_HOST="${TOXIPROXY_LISTEN_HOST:-127.0.0.1}"
TOXIPROXY_PORT="${TOXIPROXY_PORT:-3002}"

BACKEND_HOST="${BACKEND_HOST:-127.0.0.1}"
BACKEND_PORT="${BACKEND_PORT:-3001}"

VK_TOXIC="${VK_TOXIC:-}"
VK_STREAM="${VK_STREAM:-downstream}"
VK_TOXICITY="${VK_TOXICITY:-1.0}"

api_post() {
  local path="$1"
  local body="$2"
  curl -fsS \
    -H 'Content-Type: application/json' \
    -X POST \
    -d "$body" \
    "${TOXIPROXY_API_URL}${path}"
}

api_delete() {
  local path="$1"
  curl -fsS -X DELETE "${TOXIPROXY_API_URL}${path}"
}

ensure_server() {
  curl -fsS "${TOXIPROXY_API_URL}/version" >/dev/null
}

toxic_attributes_json() {
  case "$VK_TOXIC" in
    latency)
      printf '{"latency":%s,"jitter":%s}' \
        "${VK_LATENCY_MS:-0}" \
        "${VK_JITTER_MS:-0}"
      ;;
    timeout)
      printf '{"timeout":%s}' "${VK_TIMEOUT_MS:-0}"
      ;;
    slow_close)
      printf '{"delay":%s}' "${VK_SLOW_CLOSE_MS:-0}"
      ;;
    bandwidth)
      printf '{"rate":%s}' "${VK_RATE_KBPS:-1}"
      ;;
    limit_data)
      printf '{"bytes":%s}' "${VK_LIMIT_BYTES:-1024}"
      ;;
    reset_peer)
      printf '{"timeout":%s}' "${VK_TIMEOUT_MS:-0}"
      ;;
    '')
      printf '{}'
      ;;
    *)
      echo "Unsupported VK_TOXIC: ${VK_TOXIC}" >&2
      exit 1
      ;;
  esac
}

main() {
  ensure_server

  api_delete "/proxies/${TOXIPROXY_PROXY_NAME}" >/dev/null 2>&1 || true

  api_post \
    "/populate" \
    "[{\"name\":\"${TOXIPROXY_PROXY_NAME}\",\"listen\":\"${TOXIPROXY_LISTEN_HOST}:${TOXIPROXY_PORT}\",\"upstream\":\"${BACKEND_HOST}:${BACKEND_PORT}\",\"enabled\":true}]" \
    >/dev/null

  if [[ -n "$VK_TOXIC" ]]; then
    api_post \
      "/proxies/${TOXIPROXY_PROXY_NAME}/toxics" \
      "{\"name\":\"${VK_TOXIC}_${VK_STREAM}\",\"type\":\"${VK_TOXIC}\",\"stream\":\"${VK_STREAM}\",\"toxicity\":${VK_TOXICITY},\"attributes\":$(toxic_attributes_json)}" \
      >/dev/null
  fi

  echo "Configured toxiproxy proxy '${TOXIPROXY_PROXY_NAME}' on ${TOXIPROXY_LISTEN_HOST}:${TOXIPROXY_PORT} -> ${BACKEND_HOST}:${BACKEND_PORT}"
  if [[ -n "$VK_TOXIC" ]]; then
    echo "Applied toxic '${VK_TOXIC}' on stream '${VK_STREAM}'"
  fi
}

main "$@"
