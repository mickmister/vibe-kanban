# Websocket Reliability Plan

## Scope

This document covers the local app's JSON patch websocket consumers and the
draft-writing flows that depend on them. The main user-visible risk is losing
recently typed state when a websocket drops, the UI remounts, or a debounced
scratch write fails during the outage.

## Stream Inventory

| Area | Client entrypoint | Server entrypoint | Replay source after reconnect | Current risk |
| --- | --- | --- | --- | --- |
| Scratch-backed drafts and notes | `packages/web-core/src/shared/hooks/useScratch.ts` via `useJsonPatchWsStream` | `crates/server/src/routes/scratch.rs` -> `stream_scratch_raw` | DB snapshot + live DB hooks | Draft text is only persisted through debounced HTTP writes; recent local edits are not recoverable if they have not reached the server |
| Session execution processes | `packages/web-core/src/shared/hooks/useExecutionProcesses.ts` | `crates/server/src/routes/execution_processes.rs` -> `stream_execution_processes_for_session_raw` | DB snapshot + live DB hooks | Stream state is cleared during reconnect and long-lived clean closes are treated as terminal |
| Workspaces sidebar | `packages/web-core/src/shared/hooks/useWorkspaces.ts` | `crates/server/src/routes/workspaces/streams.rs` -> `stream_workspaces_raw` | DB snapshot + live DB hooks | Same reconnect semantics as the generic hook |
| Approvals | `packages/web-core/src/shared/hooks/useApprovals.ts` | `crates/server/src/routes/approvals.rs` | In-memory snapshot + live patches | Same reconnect semantics as the generic hook |
| Executor discovery | `packages/web-core/src/shared/hooks/useExecutorDiscovery.ts` | `crates/server/src/routes/config.rs` | Initial snapshot, then short-lived live stream | Lower risk because the stream is meant to finish |
| Workspace diff | `packages/web-core/src/shared/hooks/useDiffStream.ts` | `crates/server/src/routes/workspaces/streams.rs` + `crates/services/src/services/diff_stream.rs` | Fresh diff snapshot from filesystem watcher | UI drops state during reconnect; data is recomputed rather than replayed |
| Conversation history / running logs | `packages/web-core/src/features/workspace-chat/model/hooks/useConversationHistory.ts` via `streamJsonPatchEntries` | `crates/server/src/routes/execution_processes.rs` raw and normalized log streams | `MsgStore::history_plus_stream()` or DB log reload | Unexpected close is not surfaced to callers, so loaders can hang indefinitely |
| Raw log panel | `packages/web-core/src/shared/hooks/useLogStream.ts` | `crates/server/src/routes/execution_processes.rs` raw logs WS | `MsgStore::history_plus_stream()` or DB log reload | Clean close without `finished` stops retries permanently |

## Confirmed Failure Modes

### 1. Generic reconnects clear client state before replacement data arrives

`useJsonPatchWsStream` includes `retryNonce` in its effect dependencies and its
cleanup path always resets `dataRef`, `data`, and `isInitialized`. Every
reconnect attempt therefore wipes the in-memory snapshot before the replacement
socket is ready.

Files:

- `packages/web-core/src/shared/hooks/useJsonPatchWsStream.ts`

Impact:

- Read-only views flicker back to loading on every reconnect.
- Any feature that hydrates local editable state from scratch data has a larger
  window where remounts fall back to stale server data.

### 2. Clean websocket closes are treated as terminal for streams that should be long-lived

`useJsonPatchWsStream` and `useLogStream` do not reconnect when the socket
closes with code `1000` and `wasClean === true`. That is too optimistic for
long-lived streams because proxies, dev servers, and backend restarts can all
produce a clean close even though the stream should resume.

Files:

- `packages/web-core/src/shared/hooks/useJsonPatchWsStream.ts`
- `packages/web-core/src/shared/hooks/useLogStream.ts`

Impact:

- Workspaces, approvals, scratch, diff, and execution process views can become
  silently stale after an intermediary closes the connection cleanly.

### 3. `streamJsonPatchEntries` does not report unexpected close to callers

`streamJsonPatchEntries` only calls `onFinished` for an explicit `finished`
message and only calls `onError` for parse/open errors. An unexpected close
does neither. Callers in `useConversationHistory` wait on promises that resolve
only through those callbacks, so a dropped socket can leave loading stuck
forever and block retries for that execution process.

Files:

- `packages/web-core/src/shared/lib/streamJsonPatchEntries.ts`
- `packages/web-core/src/features/workspace-chat/model/hooks/useConversationHistory.ts`

Impact:

- Historic log hydration can hang.
- Running log streams can stop permanently for a process until the page state is
  rebuilt some other way.

### 4. Draft persistence has no local write-ahead buffer or retry queue

Draft editors keep the latest text in component state and persist through
debounced `scratchApi.update` calls. Those writes are cancelled on unmount and
write failures are logged but not retried. If the websocket drops at the same
time the UI remounts, navigates, or the save request fails, the most recent
typing is lost because the server snapshot only contains the last successful
save.

Files:

- `packages/web-core/src/shared/hooks/useDebouncedCallback.ts`
- `packages/web-core/src/features/workspace-chat/model/hooks/useSessionMessageEditor.ts`
- `packages/web-core/src/features/create-mode/model/useCreateModeState.ts`
- `packages/web-core/src/features/workspace/model/hooks/useWorkspaceNotes.ts`
- `packages/web-core/src/pages/kanban/IssueCommentsSectionContainer.tsx`

Impact:

- This is the main "where the user is typing" data-loss path.

## Remediation Plan

### Phase 1: Make transports reconnect predictably

1. Split stream lifecycle into two modes:
   - Reset state only when the endpoint identity changes or the consumer is
     disabled.
   - Preserve the last applied snapshot while reconnecting the same endpoint.
2. Treat only explicit terminal signals as terminal:
   - `finished` messages for finite streams.
   - Manual teardown from the component.
3. Reconnect long-lived streams after clean closes unless the stream is known to
   be finite.
4. Surface reconnect state separately from initialization state so the UI can
   show "reconnecting" without dropping cached data.

Acceptance criteria:

- Workspaces, approvals, scratch, diff, and execution process views keep their
  last snapshot visible during reconnect.
- A clean proxy close causes automatic resubscription.

### Phase 2: Make log-stream consumers close-aware

1. Update `streamJsonPatchEntries` to emit a terminal error on unexpected close.
2. Allow optional auto-reconnect for streams that can replay history safely.
3. In `useConversationHistory`, retry unexpected closes with the same backoff
   logic currently used for open failures.
4. In `useLogStream`, reconnect unless a real `finished` message was observed.

Acceptance criteria:

- Pulling the socket mid-stream does not leave conversation history stuck.
- Reopened raw or normalized log streams reconstruct the same visible history.

### Phase 3: Protect user input with local durability

1. Introduce a small local draft cache keyed by scratch type and id.
   - Write locally immediately on every keystroke.
   - Mark entries dirty until the server acknowledges the same payload.
2. Add flush semantics to the debounce helper:
   - `flush()` on blur, submit, route transition, and unmount.
   - `cancel()` remains for intentional discard.
3. Queue failed scratch writes for retry when connectivity returns.
4. Hydrate editors from local dirty state first, then reconcile with the server
   snapshot.

Suggested first targets:

- `DRAFT_FOLLOW_UP`
- `DRAFT_WORKSPACE`
- `DRAFT_TASK`
- `WORKSPACE_NOTES`

Acceptance criteria:

- Closing and reopening the websocket without leaving the page does not drop the
  latest typed characters.
- Unmounting within the debounce window still preserves the latest draft.

### Phase 4: Add observability

1. Add structured client logging for:
   - endpoint
   - close code
   - clean vs unclean close
   - reconnect attempt
   - time to ready
2. Add counters in the server for websocket opens, closes, and explicit
   `finished` completions per route.
3. Keep the instrumentation behind a debug flag if noisy.

## Test Matrix

Run the same checks on at least:

- Scratch-backed follow-up editor
- Create-workspace draft
- Issue comment draft
- Workspace notes
- Workspaces sidebar
- Execution process list
- Conversation history while a process is actively streaming

For each surface:

1. Kill the websocket before the first `Ready` message.
2. Kill it after the initial snapshot.
3. Clean-close it mid-stream.
4. Delay frames enough to force backpressure and out-of-order user actions.
5. Switch tabs or navigate while a debounced save is pending.

Success means the UI reconnects, stays populated, and preserves the last typed
state once the connection returns.
