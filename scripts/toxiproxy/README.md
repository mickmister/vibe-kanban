# Websocket Chaos Proxy

This directory contains a `toxiproxy` setup for reproducing websocket
disconnects, resets, hangs, and latency against the local backend.

`toxiproxy` works at the TCP layer, which makes it a better fit than
message-aware proxies for simulating the transport failures that normally break
long-lived websocket streams.

## What it covers

- Local-web `/api` HTTP + websocket traffic
- Vite proxy -> toxiproxy -> backend topology
- Abrupt disconnects, stalled sockets, partial data, and latency

It does not see WebRTC data-channel traffic. For remote-web diagnostics, force
the code path to use relay or direct browser websocket transport instead of the
WebRTC socket shim.

## Why this replaced the previous harness

The previous `mitmproxy` harness was better for websocket-aware message
inspection, but less reliable for simulating raw transport failures such as:

- idle socket drops
- TCP resets
- downstream hangs
- partial transport truncation

`toxiproxy` gives us those failure modes directly through its TCP toxic API.

## Prerequisites

The scripts use the official Docker image by default.

- Docker installed and running

Optional:

- A local `toxiproxy-server` binary if you do not want Docker

## Topology

1. Run the Rust backend on its normal port, for example `3001`.
2. Start `toxiproxy`, which listens on a second port, for example `3002`, and
   forwards to the backend.
3. Start the Vite frontend with `BACKEND_PORT=3002` so all `/api` HTTP and
   websocket traffic flows through the proxy.

## Quick Start

```bash
BACKEND_PORT=3001 \
TOXIPROXY_LISTEN_PORT=3002 \
scripts/toxiproxy/run-local-ws-chaos.sh
```

Then run the frontend against the proxy port:

```bash
BACKEND_PORT=3002 pnpm run local-web:dev
```

Inspect proxy status:

```bash
scripts/toxiproxy/toxic.sh status
```

Clear all toxics:

```bash
scripts/toxiproxy/toxic.sh clear
```

## Environment Variables

- `BACKEND_PORT`
  - Upstream Rust backend port.
  - Default: `3001`
- `TOXIPROXY_LISTEN_PORT`
  - Port exposed to the frontend.
  - Default: `3002`
- `TOXIPROXY_API_PORT`
  - Admin API port.
  - Default: `8474`
- `TOXIPROXY_PROXY_NAME`
  - Proxy resource name.
  - Default: `vk-local-backend`
- `TOXIPROXY_CONTAINER_NAME`
  - Docker container name when using Docker mode.
  - Default: `vk-toxiproxy`
- `TOXIPROXY_IMAGE`
  - Docker image to use.
  - Default: `ghcr.io/shopify/toxiproxy`
- `TOXIPROXY_SERVER_BIN`
  - Optional local server binary. If set, Docker is skipped.

## Common Manual Scenarios

### Abrupt disconnect / TCP reset

```bash
scripts/toxiproxy/toxic.sh reset-peer downstream
```

Use this to verify reconnect logic when the socket is hard-reset by an
intermediary.

### Real clean websocket close

`toxiproxy` cannot synthesize an application-level websocket close frame. For
that case, use one of:

- restart the backend while the page is open
- restart the frontend dev proxy while the page is open
- temporarily disable then re-enable the proxy to force reconnect behavior

### Hanging socket / stalled stream

```bash
scripts/toxiproxy/toxic.sh timeout downstream 5000
```

Use this to simulate the connection appearing open while reads stall.

### Cleanish “connection goes away after some traffic”

```bash
scripts/toxiproxy/toxic.sh limit-data downstream 4096
```

This is a good approximation for “the stream worked briefly, then died”.

### Latency pressure

```bash
scripts/toxiproxy/toxic.sh latency downstream 400 100
```

Use this to check whether views stay populated while updates arrive slowly.

### Entire backend path down

```bash
scripts/toxiproxy/toxic.sh downstream-down
```

This cuts traffic from backend -> browser until cleared.

Restore normal traffic:

```bash
scripts/toxiproxy/toxic.sh clear
```

## Suggested Manual Test Pass

### Scratch durability

1. Start the proxy and frontend through `3002`.
2. Type into:
   - follow-up draft
   - create-workspace prompt
   - issue comment draft
   - workspace notes
3. Apply:

```bash
scripts/toxiproxy/toxic.sh reset-peer downstream
```

or:

```bash
scripts/toxiproxy/toxic.sh limit-data downstream 2048
```

Watch for lost characters after reconnect, remount, or submit.

### Conversation stream interruption

1. Start a running execution process.
2. Apply:

```bash
scripts/toxiproxy/toxic.sh timeout downstream 5000
```

or:

```bash
scripts/toxiproxy/toxic.sh reset-peer downstream
```

Confirm the timeline resumes instead of hanging.

### Workspaces / approvals stale-stream check

Apply:

```bash
scripts/toxiproxy/toxic.sh reset-peer downstream
```

Confirm long-lived streams reconnect instead of silently going stale.

## Notes

- `toxiproxy` works at the TCP layer, so it cannot target websocket routes by
  URL path. It affects the entire backend port routed through it.
- `toxiproxy` is the preferred harness for disconnects, resets, hangs, and
  latency, but not for websocket-frame-aware “send a clean close frame”
  experiments.
- For websocket-path-specific experiments, a message-aware proxy is still
  useful, but for transport failures this setup is the preferred harness.
