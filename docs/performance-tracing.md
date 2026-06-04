# Performance Tracing

Use the local server's structured tracing when investigating function-level,
HTTP-level, SQL query-level, or WebSocket send-path bottlenecks.

## Quick start

```bash
VK_PERF_TRACING=1 RUST_LOG=info pnpm run backend:dev:watch
```

`VK_PERF_TRACING=1` keeps the normal application log level but also installs
the HTTP tracing middleware and enables:

- `tower_http=debug` for HTTP request spans and response latency;
- `sqlx::query=debug` for SQLx query timings;
- `server::middleware::signed_ws=trace` for signed/plain WebSocket upgrade,
  send, receive, flush, and close paths;
- `ws_bridge=trace` for proxied WebSocket bridge send paths.

For deeper targeted function-level traces, add explicit module directives:

```bash
VK_PERF_TRACING=1 \
RUST_LOG='server=trace,services=debug,db=debug,sqlx::query=debug,tower_http=debug,ws_bridge=trace' \
pnpm run backend:dev:watch
```

## Sending traces to Sentry

The backend already initializes Sentry when `SENTRY_DSN` is configured. To send
performance spans to Sentry, explicitly set a Sentry trace sample rate for the
profiling run.

Use a lower sample rate for longer or higher-volume runs:

```bash
SENTRY_DSN='https://public-key@o0.ingest.sentry.io/project-id' \
VK_PERF_TRACING=1 \
VK_SENTRY_TRACES_SAMPLE_RATE=0.2 \
RUST_LOG=info \
pnpm run backend:dev:watch
```

`VK_SENTRY_TRACES_SAMPLE_RATE` takes precedence over `SENTRY_TRACES_SAMPLE_RATE`.
Both must be valid Sentry-supported values from `0.0` to `1.0`; invalid values
disable Sentry Performance export instead of falling back to a higher sample
rate. Prefer short, sampled profiling windows for noisy WebSocket sessions to
avoid excessive span volume.

### Sentry smoke-test checklist

For a profiling run that sends data to Sentry:

1. Start the backend with `SENTRY_DSN`, `VK_PERF_TRACING=1`, and an explicit
   `VK_SENTRY_TRACES_SAMPLE_RATE`.
2. Load a session in the browser and wait for the spinner to resolve.
3. Trigger or observe an agent message stream.
4. In Sentry Performance, confirm a backend transaction includes nested spans
   such as `http.request`, `sessions.find_by_workspace_id`,
   `events.stream_execution_processes.initial_snapshot`,
   `normalized_logs.*`, and `ws.send`.
5. Confirm HTTP span data uses route templates rather than raw query strings or
   full request URIs.

## WebSocket notes

HTTP tracing records the upgrade request/response only. After a connection is
upgraded, WebSocket sends and receives no longer pass through HTTP middleware,
so the server instruments those paths explicitly. Look for spans/events named:

- `ws.upgrade`
- `ws.send`
- `ws.recv`
- `ws.bridge.send`

Message logs include the message kind, byte length, and whether a close frame
was present. Payload contents are intentionally not logged.

Low-level sink polling events such as `ws.sink.start_send`,
`ws.sink.poll_ready`, and `ws.sink.poll_flush` are intentionally gated behind an
additional flag because they can be very noisy:

```bash
VK_PERF_TRACING=1 VK_WS_POLL_TRACING=1 RUST_LOG=info pnpm run backend:dev:watch
```

## SQL query notes

SQLx query logs are emitted by SQLx itself under the `sqlx::query` target. Keep
this at `debug` while profiling and avoid leaving very verbose tracing enabled
in normal development sessions unless you need the data.
