#!/usr/bin/env bash

set -euo pipefail

TOXIPROXY_API_URL="${TOXIPROXY_API_URL:-http://127.0.0.1:8474}"
TOXIPROXY_PROXY_NAME="${TOXIPROXY_PROXY_NAME:-vk_local_ws}"

usage() {
  cat <<'EOF'
Usage:
  scripts/toxiproxy/toxic.sh status
  scripts/toxiproxy/toxic.sh clear
  scripts/toxiproxy/toxic.sh enable
  scripts/toxiproxy/toxic.sh disable
  scripts/toxiproxy/toxic.sh reset-peer <upstream|downstream> [toxicity]
  scripts/toxiproxy/toxic.sh timeout <upstream|downstream> <ms> [toxicity]
  scripts/toxiproxy/toxic.sh latency <upstream|downstream> <ms> [jitter_ms] [toxicity]
  scripts/toxiproxy/toxic.sh bandwidth <upstream|downstream> <kbps> [toxicity]
  scripts/toxiproxy/toxic.sh slow-close <upstream|downstream> <ms> [toxicity]
  scripts/toxiproxy/toxic.sh limit-data <upstream|downstream> <bytes> [toxicity]
  scripts/toxiproxy/toxic.sh downstream-down
  scripts/toxiproxy/toxic.sh upstream-down
EOF
}

require_proxy() {
  curl -fsS "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}" >/dev/null
}

add_toxic() {
  local name="$1"
  local type="$2"
  local stream="$3"
  local toxicity="$4"
  local attributes="$5"

  curl -fsS -X POST \
    -H 'Content-Type: application/json' \
    -d "{\"name\":\"${name}\",\"type\":\"${type}\",\"stream\":\"${stream}\",\"toxicity\":${toxicity},\"attributes\":${attributes}}" \
    "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}/toxics"
}

cmd="${1:-}"
if [[ -z "${cmd}" ]]; then
  usage
  exit 1
fi
shift || true

case "${cmd}" in
  status)
    require_proxy
    curl -fsS "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}"
    ;;
  clear)
    require_proxy
    curl -fsS -X DELETE \
      "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}/toxics"
    ;;
  enable)
    require_proxy
    curl -fsS -X POST \
      -H 'Content-Type: application/json' \
      -d '{"enabled":true}' \
      "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}"
    ;;
  disable)
    require_proxy
    curl -fsS -X POST \
      -H 'Content-Type: application/json' \
      -d '{"enabled":false}' \
      "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}"
    ;;
  reset-peer)
    stream="${1:?stream required}"
    toxicity="${2:-1.0}"
    require_proxy
    add_toxic "vk-reset-peer-${stream}" "reset_peer" "${stream}" "${toxicity}" '{}'
    ;;
  timeout)
    stream="${1:?stream required}"
    timeout_ms="${2:?timeout ms required}"
    toxicity="${3:-1.0}"
    require_proxy
    add_toxic "vk-timeout-${stream}" "timeout" "${stream}" "${toxicity}" "{\"timeout\":${timeout_ms}}"
    ;;
  latency)
    stream="${1:?stream required}"
    latency_ms="${2:?latency ms required}"
    jitter_ms="${3:-0}"
    toxicity="${4:-1.0}"
    require_proxy
    add_toxic "vk-latency-${stream}" "latency" "${stream}" "${toxicity}" "{\"latency\":${latency_ms},\"jitter\":${jitter_ms}}"
    ;;
  bandwidth)
    stream="${1:?stream required}"
    rate_kbps="${2:?rate kbps required}"
    toxicity="${3:-1.0}"
    require_proxy
    add_toxic "vk-bandwidth-${stream}" "bandwidth" "${stream}" "${toxicity}" "{\"rate\":${rate_kbps}}"
    ;;
  slow-close)
    stream="${1:?stream required}"
    delay_ms="${2:?delay ms required}"
    toxicity="${3:-1.0}"
    require_proxy
    add_toxic "vk-slow-close-${stream}" "slow_close" "${stream}" "${toxicity}" "{\"delay\":${delay_ms}}"
    ;;
  limit-data)
    stream="${1:?stream required}"
    bytes="${2:?bytes required}"
    toxicity="${3:-1.0}"
    require_proxy
    add_toxic "vk-limit-data-${stream}" "limit_data" "${stream}" "${toxicity}" "{\"bytes\":${bytes}}"
    ;;
  downstream-down)
    require_proxy
    add_toxic "vk-downstream-down" "timeout" "downstream" "1.0" '{"timeout":600000}'
    ;;
  upstream-down)
    require_proxy
    add_toxic "vk-upstream-down" "timeout" "upstream" "1.0" '{"timeout":600000}'
    ;;
  *)
    usage
    exit 1
    ;;
esac
