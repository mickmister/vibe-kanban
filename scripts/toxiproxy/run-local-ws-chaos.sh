#!/usr/bin/env bash

set -euo pipefail

BACKEND_PORT="${BACKEND_PORT:-3001}"
TOXIPROXY_LISTEN_PORT="${TOXIPROXY_LISTEN_PORT:-3002}"
TOXIPROXY_API_PORT="${TOXIPROXY_API_PORT:-8474}"
TOXIPROXY_PROXY_NAME="${TOXIPROXY_PROXY_NAME:-vk-local-backend}"
TOXIPROXY_CONTAINER_NAME="${TOXIPROXY_CONTAINER_NAME:-vk-toxiproxy}"
TOXIPROXY_IMAGE="${TOXIPROXY_IMAGE:-ghcr.io/shopify/toxiproxy}"
TOXIPROXY_SERVER_BIN="${TOXIPROXY_SERVER_BIN:-}"
TOXIPROXY_API_URL="http://127.0.0.1:${TOXIPROXY_API_PORT}"

wait_for_api() {
  for _ in $(seq 1 40); do
    if curl -fsS "${TOXIPROXY_API_URL}/version" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done

  echo "Timed out waiting for toxiproxy API at ${TOXIPROXY_API_URL}" >&2
  exit 1
}

ensure_server() {
  if [[ -n "${TOXIPROXY_SERVER_BIN}" ]]; then
    if ! curl -fsS "${TOXIPROXY_API_URL}/version" >/dev/null 2>&1; then
      echo "Starting toxiproxy-server from TOXIPROXY_SERVER_BIN=${TOXIPROXY_SERVER_BIN}"
      "${TOXIPROXY_SERVER_BIN}" >/tmp/vk-toxiproxy.log 2>&1 &
    fi
    wait_for_api
    return
  fi

  if docker ps --format '{{.Names}}' | grep -Fx "${TOXIPROXY_CONTAINER_NAME}" >/dev/null; then
    wait_for_api
    return
  fi

  if docker ps -a --format '{{.Names}}' | grep -Fx "${TOXIPROXY_CONTAINER_NAME}" >/dev/null; then
    docker rm -f "${TOXIPROXY_CONTAINER_NAME}" >/dev/null
  fi

  echo "Starting toxiproxy container ${TOXIPROXY_CONTAINER_NAME}"
  docker run -d \
    --name "${TOXIPROXY_CONTAINER_NAME}" \
    --network host \
    "${TOXIPROXY_IMAGE}" >/dev/null

  wait_for_api
}

ensure_proxy() {
  local payload
  payload="$(
    cat <<JSON
[{"name":"${TOXIPROXY_PROXY_NAME}","listen":"127.0.0.1:${TOXIPROXY_LISTEN_PORT}","upstream":"127.0.0.1:${BACKEND_PORT}","enabled":true}]
JSON
  )"

  curl -fsS -X POST \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${TOXIPROXY_API_URL}/populate" >/dev/null
}

clear_toxics() {
  curl -fsS -X DELETE \
    "${TOXIPROXY_API_URL}/proxies/${TOXIPROXY_PROXY_NAME}/toxics" >/dev/null \
    || true
}

ensure_server
ensure_proxy
clear_toxics

cat <<EOF
toxiproxy ready

Admin API:  ${TOXIPROXY_API_URL}
Proxy name: ${TOXIPROXY_PROXY_NAME}
Listen:     127.0.0.1:${TOXIPROXY_LISTEN_PORT}
Upstream:   127.0.0.1:${BACKEND_PORT}

Run the frontend through the proxy:
  BACKEND_PORT=${TOXIPROXY_LISTEN_PORT} pnpm run local-web:dev

Useful commands:
  scripts/toxiproxy/toxic.sh status
  scripts/toxiproxy/toxic.sh reset-peer downstream
  scripts/toxiproxy/toxic.sh timeout downstream 5000
  scripts/toxiproxy/toxic.sh latency downstream 400 100
  scripts/toxiproxy/toxic.sh clear
EOF
