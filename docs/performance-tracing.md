# Performance Tracing

Use the local server's structured tracing when investigating function-level,
HTTP-level, SQL query-level, or WebSocket send-path bottlenecks.

## Quick start

```bash
VK_PERF_TRACING=1 RUST_LOG=info pnpm run backend:dev:watch
```

`VK_PERF_TRACING=1` keeps the normal application log level but also enables:

- `tower_http=debug` for HTTP request spans and response latency;
- `sqlx::query=debug` for SQLx query timings;
- `server::middleware::signed_ws=trace` for signed/plain WebSocket upgrade,
  send, receive, flush, and close paths;
- `ws_bridge=trace` for proxied WebSocket bridge send paths.

For deeper function-level traces, add explicit module directives:

```bash
VK_PERF_TRACING=1 \
RUST_LOG='server=trace,services=debug,db=debug,sqlx::query=debug,tower_http=debug,ws_bridge=trace' \
pnpm run backend:dev:watch
```

## WebSocket notes

HTTP tracing records the upgrade request/response only. After a connection is
upgraded, WebSocket sends and receives no longer pass through HTTP middleware,
so the server instruments those paths explicitly. Look for spans/events named:

- `ws.upgrade`
- `ws.send`
- `ws.recv`
- `ws.sink.start_send`
- `ws.sink.poll_flush`
- `ws.bridge.send`

Message logs include the message kind, byte length, and whether a close frame
was present. Payload contents are intentionally not logged.

## SQL query notes

SQLx query logs are emitted by SQLx itself under the `sqlx::query` target. Keep
this at `debug` while profiling and avoid leaving very verbose tracing enabled
in normal development sessions unless you need the data.
