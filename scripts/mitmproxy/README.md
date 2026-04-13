# Websocket Chaos Proxy

This directory contains a small `mitmproxy` setup for reproducing websocket
disconnects and slow patch delivery against the local backend.

## What it covers

- Local-web `/api/.../ws` traffic
- Vite proxy -> mitmproxy -> backend topology

It does not see WebRTC data-channel traffic. For remote-web diagnostics, force
the code path to use relay or direct browser websocket transport instead of the
WebRTC socket shim.

## Prerequisites

Install `mitmproxy` locally before using the scripts. Example:

```bash
pipx install mitmproxy
```

## Topology

1. Run the Rust backend on its normal port, for example `3001`.
2. Run `mitmdump` on a second port, for example `3002`, in reverse-proxy mode
   to the backend.
3. Start the Vite frontend with `BACKEND_PORT=3002` so all `/api` HTTP and
   websocket traffic flows through `mitmproxy`.

## Quick Start

```bash
BACKEND_PORT=3001 \
MITMPROXY_PORT=3002 \
VK_WS_MATCH='/api/scratch/,/api/execution-processes/' \
VK_WS_KILL_AFTER_MESSAGES=5 \
scripts/mitmproxy/run-local-ws-chaos.sh
```

Then run the frontend against the proxy port:

```bash
BACKEND_PORT=3002 pnpm run local-web:dev
```

## Environment Variables

- `BACKEND_PORT`
  - Upstream Rust backend port.
  - Default: `3001`
- `MITMPROXY_PORT`
  - Port exposed by `mitmproxy`.
  - Default: `3002`
- `VK_WS_MATCH`
  - Comma-separated path substrings. Only matching websocket requests are
    manipulated.
  - Default: empty, which matches every websocket request.
- `VK_WS_KILL_AFTER_MESSAGES`
  - Kill a matched websocket after this many websocket messages have passed
    through the proxy.
  - Default: `0` (disabled)
- `VK_WS_KILL_AFTER_SECONDS`
  - Kill a matched websocket once it has been open for at least this many
    seconds.
  - Default: `0` (disabled)
- `VK_WS_DELAY_MS`
  - Sleep before forwarding each websocket message on matched flows.
  - Default: `0`
- `VK_WS_CLOSE_MODE`
  - `kill` for an abrupt disconnect, or `clean` for a best-effort websocket
    close frame before fallback to a hard kill.
  - Default: `kill`
- `VK_WS_LOG`
  - Set to `0` to reduce addon logging.
  - Default: `1`

## Suggested Scenarios

### Scratch durability

```bash
VK_WS_MATCH='/api/scratch/' VK_WS_KILL_AFTER_MESSAGES=2 scripts/mitmproxy/run-local-ws-chaos.sh
```

Type into:

- follow-up draft
- create-workspace prompt
- issue comment draft
- workspace notes

Watch for lost characters after reconnect, remount, or submit.

### Conversation stream interruption

```bash
VK_WS_MATCH='/normalized-logs/ws,/raw-logs/ws' VK_WS_KILL_AFTER_SECONDS=3 scripts/mitmproxy/run-local-ws-chaos.sh
```

Start a running execution process and confirm the timeline resumes instead of
hanging.

### Clean close handling

```bash
VK_WS_MATCH='/api/workspaces/,/api/approvals/' VK_WS_CLOSE_MODE=clean VK_WS_KILL_AFTER_SECONDS=5 scripts/mitmproxy/run-local-ws-chaos.sh
```

Use this to verify that long-lived streams reconnect even when the intermediary
closes them cleanly.

### Clean latency pressure

```bash
VK_WS_MATCH='/api/workspaces/,/api/execution-processes/' VK_WS_DELAY_MS=400 scripts/mitmproxy/run-local-ws-chaos.sh
```

Check whether views stay populated while patches arrive slowly.
