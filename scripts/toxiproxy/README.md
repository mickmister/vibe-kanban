# Websocket Chaos Proxy

This directory contains a small `toxiproxy` setup for reproducing websocket
disconnects, stalls, and slow delivery against the local backend.

## What it covers

- Local-web `/api/...` traffic, including websocket upgrades
- Vite proxy -> toxiproxy -> backend topology

It does not see WebRTC data-channel traffic. For remote-web diagnostics, force
the code path to use relay or direct browser websocket transport instead of the
WebRTC socket shim.

## Why Toxiproxy

`toxiproxy` is a better fit than `mitmproxy` for frontend reconnect work in
this repo because the main need is repeatable transport failures:

- bring the backend path down and back up
- keep it stalled for a while
- add latency or bandwidth pressure
- repeat the same outage while iterating on frontend state handling

## Prerequisites

Install `toxiproxy-server` locally, or run the official container image.

Examples from the official project:

- macOS/Homebrew: `brew tap shopify/shopify && brew install toxiproxy`
- Docker image: `ghcr.io/shopify/toxiproxy`

## Topology

1. Run the Rust backend on its normal port, for example `3001`.
2. Run `toxiproxy-server`.
3. Create a proxy on a second port, for example `3002`, upstreaming to the
   backend.
4. Start the Vite frontend with `BACKEND_PORT=3002` so all `/api` HTTP and
   websocket traffic flows through `toxiproxy`.

## Quick Start

Start the toxiproxy server:

```bash
scripts/toxiproxy/run-server.sh
```

Configure a proxy from `3002` to the real backend on `3001`:

```bash
BACKEND_PORT=3001 \
TOXIPROXY_PORT=3002 \
scripts/toxiproxy/configure-local-ws-chaos.sh
```

Then run the frontend against the proxy port:

```bash
cd packages/local-web
BACKEND_PORT=3002 npx vite --host 0.0.0.0 --port 3028
```

## Common Workflows

### Hard outage

Disable the proxy entirely:

```bash
scripts/toxiproxy/set-enabled.sh down
```

Bring it back:

```bash
scripts/toxiproxy/set-enabled.sh up
```

This is the cleanest way to test reconnect behavior after a sustained outage.

### Infinite hang

Drop all downstream data without closing the connection:

```bash
VK_TOXIC=timeout VK_TIMEOUT_MS=0 scripts/toxiproxy/configure-local-ws-chaos.sh
```

Remove the toxic and restore traffic:

```bash
scripts/toxiproxy/reset.sh
```

### Fixed-duration timeout

Close connections after traffic is blocked for a while:

```bash
VK_TOXIC=timeout VK_TIMEOUT_MS=5000 scripts/toxiproxy/configure-local-ws-chaos.sh
```

### Abrupt disconnect after some traffic

Close the TCP connection once a byte threshold is crossed:

```bash
VK_TOXIC=limit_data VK_LIMIT_BYTES=4096 scripts/toxiproxy/configure-local-ws-chaos.sh
```

### Slow patch delivery

Delay downstream traffic:

```bash
VK_TOXIC=latency VK_LATENCY_MS=400 scripts/toxiproxy/configure-local-ws-chaos.sh
```

### Narrow bandwidth

Throttle response throughput:

```bash
VK_TOXIC=bandwidth VK_RATE_KBPS=8 scripts/toxiproxy/configure-local-ws-chaos.sh
```

## Environment Variables

- `BACKEND_PORT`
  - Upstream Rust backend port.
  - Default: `3001`
- `BACKEND_HOST`
  - Upstream Rust backend host.
  - Default: `127.0.0.1`
- `TOXIPROXY_PORT`
  - Port exposed by the data proxy.
  - Default: `3002`
- `TOXIPROXY_API_URL`
  - Toxiproxy control API base URL.
  - Default: `http://127.0.0.1:8474`
- `TOXIPROXY_PROXY_NAME`
  - Proxy name to create/update.
  - Default: `vk_local_ws`
- `TOXIPROXY_LISTEN_HOST`
  - Listen host for the proxy.
  - Default: `127.0.0.1`
  - Use `0.0.0.0` if the server is running inside Docker with host port
    publishing.
- `VK_TOXIC`
  - Optional toxic to add after the proxy is created.
  - Supported by the helper script:
    - `latency`
    - `timeout`
    - `slow_close`
    - `bandwidth`
    - `limit_data`
    - `reset_peer`
- `VK_STREAM`
  - Toxic direction.
  - Default: `downstream`
- `VK_TOXICITY`
  - Probability the toxic is applied.
  - Default: `1.0`

### Toxic-specific variables

- `VK_LATENCY_MS`
  - Used by `latency`
  - Default: `0`
- `VK_JITTER_MS`
  - Used by `latency`
  - Default: `0`
- `VK_TIMEOUT_MS`
  - Used by `timeout` and `reset_peer`
  - Default: `0`
- `VK_SLOW_CLOSE_MS`
  - Used by `slow_close`
  - Default: `0`
- `VK_RATE_KBPS`
  - Used by `bandwidth`
  - Default: `1`
- `VK_LIMIT_BYTES`
  - Used by `limit_data`
  - Default: `1024`

## Suggested Scenarios

### Scratch durability

Use a full outage or hang while typing into:

- follow-up draft
- create-workspace prompt
- issue comment draft
- workspace notes

Recommended commands:

```bash
scripts/toxiproxy/set-enabled.sh down
```

or

```bash
VK_TOXIC=timeout VK_TIMEOUT_MS=0 scripts/toxiproxy/configure-local-ws-chaos.sh
```

Watch for lost characters after reconnect, remount, or submit.

### Conversation stream interruption

Use an abrupt TCP reset:

```bash
VK_TOXIC=reset_peer VK_TIMEOUT_MS=0 scripts/toxiproxy/configure-local-ws-chaos.sh
```

Start a running execution process and confirm the timeline resumes instead of
hanging.

### Long outage recovery

Disable the proxy for a while, then re-enable it:

```bash
scripts/toxiproxy/set-enabled.sh down
sleep 5
scripts/toxiproxy/set-enabled.sh up
```

Use this to verify that long-lived streams reconnect and keep their last known
state visible while the transport is unavailable.

### Latency pressure

```bash
VK_TOXIC=latency VK_LATENCY_MS=400 scripts/toxiproxy/configure-local-ws-chaos.sh
```

Check whether views stay populated while patches arrive slowly.
