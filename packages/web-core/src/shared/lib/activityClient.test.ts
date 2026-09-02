import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ActivityV1Snapshot, ActivityV1WsEvent } from 'shared/types';
import { ActivityClient, buildActivityV1WsUrl } from './activityClient';

class FakeWebSocket {
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  close = vi.fn(() => {
    this.onclose?.({} as CloseEvent);
  });

  open() {
    this.onopen?.({} as Event);
  }

  message(value: unknown) {
    this.onmessage?.({ data: JSON.stringify(value) } as MessageEvent);
  }

  fail() {
    this.onerror?.({} as Event);
    this.onclose?.({} as CloseEvent);
  }
}

const generatedAt = '2026-08-28T12:00:00.000Z';

function snapshot(
  overrides: Partial<ActivityV1Snapshot> = {}
): ActivityV1Snapshot {
  return {
    schema_version: 'activity.v1',
    generated_at: generatedAt,
    scope: {
      workspace_id: null,
      session_id: null,
      user_id: null,
    },
    summary: {
      active_turn_count: 0,
      pending_turn_count: 0,
      callback_waiting_count: 0,
      recent_callback_count: 0,
    },
    workspaces: [],
    ...overrides,
  };
}

function snapshotEvent(
  eventId: string,
  value: ActivityV1Snapshot
): ActivityV1WsEvent {
  return {
    schema_version: 'activity.v1.ws',
    event_id: eventId,
    cursor: `${eventId}:cursor`,
    event_type: 'snapshot',
    generated_at: generatedAt,
    snapshot: value,
    reason: null,
  };
}

function heartbeatEvent(eventId = 'heartbeat-1'): ActivityV1WsEvent {
  return {
    schema_version: 'activity.v1.ws',
    event_id: eventId,
    cursor: `${eventId}:cursor`,
    event_type: 'heartbeat',
    generated_at: generatedAt,
    snapshot: null,
    reason: null,
  };
}

function refreshEvent(): ActivityV1WsEvent {
  return {
    schema_version: 'activity.v1.ws',
    event_id: 'refresh-1',
    cursor: 'refresh-1:cursor',
    event_type: 'refresh_snapshot',
    generated_at: generatedAt,
    snapshot: null,
    reason: 'Replay unavailable',
  };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('ActivityClient', () => {
  it('builds scoped websocket URLs with reconnect cursor', () => {
    expect(
      buildActivityV1WsUrl(
        { workspaceId: 'workspace-1', sessionId: 'session-1' },
        'snapshot:1'
      )
    ).toBe(
      '/api/activity/v1/ws?workspace_id=workspace-1&session_id=session-1&cursor=snapshot%3A1'
    );
  });

  it('applies websocket snapshot events and exposes foreground summaries', async () => {
    const ws = new FakeWebSocket();
    const openWebSocket = vi.fn(() => ws as unknown as WebSocket);
    const client = new ActivityClient({ openWebSocket, now: () => 1000 });
    const states: string[] = [];
    client.subscribe((state) => states.push(state.connection.status));

    client.start();
    await flushMicrotasks();
    expect(openWebSocket).toHaveBeenCalledWith('/api/activity/v1/ws');
    ws.open();
    ws.message(
      snapshotEvent(
        'snapshot-1',
        snapshot({
          summary: {
            active_turn_count: 2,
            pending_turn_count: 1,
            callback_waiting_count: 1,
            recent_callback_count: 3,
          },
        })
      )
    );

    const state = client.getSnapshot();
    expect(states).toContain('connected');
    expect(state.summary).toEqual({
      activeTurnCount: 2,
      pendingTurnCount: 1,
      callbackWaitingCount: 1,
      recentCallbackCount: 3,
    });
    expect(state.connection.lastCursor).toBe('snapshot-1:cursor');
    client.stop();
  });

  it('polls as fallback when websocket connect fails', async () => {
    const fetched = snapshot({
      summary: {
        active_turn_count: 0,
        pending_turn_count: 4,
        callback_waiting_count: 0,
        recent_callback_count: 0,
      },
    });
    const fetchSnapshot = vi.fn(async () => fetched);
    const client = new ActivityClient({
      openWebSocket: vi.fn(async () => {
        throw new Error('webhook /Users/me failed');
      }),
      fetchSnapshot,
      reconnectBaseMs: 10,
      pollIntervalMs: 10,
    });

    client.start();
    await flushMicrotasks();

    const state = client.getSnapshot();
    expect(fetchSnapshot).toHaveBeenCalled();
    expect(state.connection.usingPollingFallback).toBe(true);
    expect(state.summary.pendingTurnCount).toBe(4);
    expect(JSON.stringify(state).toLowerCase()).not.toContain('webhook');
    expect(JSON.stringify(state).toLowerCase()).not.toContain('/users/');
    client.stop();
  });

  it('fetches a fresh snapshot on refresh-snapshot events', async () => {
    const ws = new FakeWebSocket();
    const fetchSnapshot = vi.fn(async () =>
      snapshot({
        summary: {
          active_turn_count: 1,
          pending_turn_count: 0,
          callback_waiting_count: 0,
          recent_callback_count: 0,
        },
      })
    );
    const client = new ActivityClient({
      openWebSocket: () => ws as unknown as WebSocket,
      fetchSnapshot,
    });

    client.start();
    await flushMicrotasks();
    ws.open();
    ws.message(refreshEvent());
    await flushMicrotasks();

    expect(fetchSnapshot).toHaveBeenCalledTimes(1);
    expect(client.getSnapshot().summary.activeTurnCount).toBe(1);
    client.stop();
  });

  it('ignores duplicate websocket events', async () => {
    const ws = new FakeWebSocket();
    const client = new ActivityClient({
      openWebSocket: () => ws as unknown as WebSocket,
    });
    client.start();
    await flushMicrotasks();
    ws.open();
    ws.message(
      snapshotEvent(
        'snapshot-1',
        snapshot({
          summary: {
            active_turn_count: 1,
            pending_turn_count: 0,
            callback_waiting_count: 0,
            recent_callback_count: 0,
          },
        })
      )
    );
    ws.message(
      snapshotEvent(
        'snapshot-1',
        snapshot({
          summary: {
            active_turn_count: 9,
            pending_turn_count: 0,
            callback_waiting_count: 0,
            recent_callback_count: 0,
          },
        })
      )
    );

    expect(client.getSnapshot().summary.activeTurnCount).toBe(1);
    client.stop();
  });

  it('falls back to polling when heartbeat times out and reconnects with cursor', async () => {
    let currentTime = 0;
    const firstWs = new FakeWebSocket();
    const secondWs = new FakeWebSocket();
    const openWebSocket = vi
      .fn()
      .mockReturnValueOnce(firstWs as unknown as WebSocket)
      .mockReturnValueOnce(secondWs as unknown as WebSocket);
    const fetchSnapshot = vi.fn(async () => snapshot());
    const client = new ActivityClient({
      openWebSocket,
      fetchSnapshot,
      now: () => currentTime,
      heartbeatTimeoutMs: 3000,
      reconnectBaseMs: 10,
      pollIntervalMs: 10,
    });

    client.start();
    await flushMicrotasks();
    firstWs.open();
    firstWs.message(heartbeatEvent());
    currentTime = 4001;
    await vi.advanceTimersByTimeAsync(1000);
    await flushMicrotasks();
    await vi.advanceTimersByTimeAsync(10);
    await flushMicrotasks();

    expect(fetchSnapshot).toHaveBeenCalled();
    expect(openWebSocket).toHaveBeenLastCalledWith(
      '/api/activity/v1/ws?cursor=heartbeat-1%3Acursor'
    );
    client.stop();
  });

  it('sanitizes product-visible callback strings from websocket snapshots', async () => {
    const ws = new FakeWebSocket();
    const client = new ActivityClient({
      openWebSocket: () => ws as unknown as WebSocket,
    });
    client.start();
    await flushMicrotasks();
    ws.open();
    ws.message(
      snapshotEvent(
        'snapshot-hostile',
        snapshot({
          summary: {
            active_turn_count: 0,
            pending_turn_count: 0,
            callback_waiting_count: 1,
            recent_callback_count: 1,
          },
          workspaces: [
            {
              subject: {
                kind: 'workspace',
                id: 'workspace-1',
                workspace_id: 'workspace-1',
                session_id: null,
              },
              summary: {
                active_turn_count: 0,
                pending_turn_count: 0,
                callback_waiting_count: 1,
                recent_callback_count: 1,
              },
              sessions: [
                {
                  subject: {
                    kind: 'session',
                    id: 'session-1',
                    workspace_id: 'workspace-1',
                    session_id: 'session-1',
                  },
                  status: 'waiting_for_callback',
                  summary_text:
                    'webhook queue_item /Users/me raw XML provider diagnostics shell bd show git status trigger delivery ID execution process ID',
                  summary: {
                    active_turn_count: 0,
                    pending_turn_count: 0,
                    callback_waiting_count: 1,
                    recent_callback_count: 1,
                  },
                  callbacks: [
                    {
                      callback_id: 'callback-1',
                      kind: 'workflow_completion',
                      status: 'failed',
                      summary_text:
                        'webhook queue_item /tmp/secret raw JSON provider diagnostics',
                      workflow: {
                        run_id: 'run-1',
                        name: 'Workflow',
                        design_id: null,
                        version: null,
                      },
                      links: [],
                      created_at: generatedAt,
                      updated_at: generatedAt,
                    },
                  ],
                  links: [],
                  updated_at: generatedAt,
                },
              ],
              links: [],
              updated_at: generatedAt,
            },
          ],
        })
      )
    );

    const serialized = JSON.stringify(client.getSnapshot()).toLowerCase();
    for (const forbidden of [
      'webhook',
      'queue_item',
      '/users/',
      '/tmp/',
      'raw xml',
      'raw json',
      'provider diagnostics',
      'shell',
      'bd show',
      'git status',
      'trigger',
      'delivery id',
      'execution process id',
    ]) {
      expect(serialized).not.toContain(forbidden);
    }
    client.stop();
  });
});
