import type {
  ActivityV1Callback,
  ActivityV1Snapshot,
  ActivityV1WsEvent,
} from 'shared/types';
import {
  makeLocalApiRequest,
  openLocalApiWebSocket,
} from '@/shared/lib/localApiTransport';
import { handleApiResponse } from '@/shared/lib/api';

type ActivityStatus =
  | 'idle'
  | 'connecting'
  | 'connected'
  | 'polling'
  | 'reconnecting'
  | 'closed'
  | 'error';

export interface ActivityClientFilters {
  workspaceId?: string | null;
  sessionId?: string | null;
}

export interface ActivityConnectionState {
  status: ActivityStatus;
  lastCursor: string | null;
  lastHeartbeatAt: number | null;
  lastSnapshotAt: number | null;
  lastError: string | null;
  usingPollingFallback: boolean;
}

export interface ActivityForegroundState {
  snapshot: ActivityV1Snapshot | null;
  connection: ActivityConnectionState;
  summary: {
    activeTurnCount: number;
    pendingTurnCount: number;
    callbackWaitingCount: number;
    recentCallbackCount: number;
  };
  callbacks: ActivityV1Callback[];
}

export interface ActivityClientOptions {
  filters?: ActivityClientFilters;
  fetchSnapshot?: (
    filters: ActivityClientFilters
  ) => Promise<ActivityV1Snapshot>;
  openWebSocket?: (url: string) => Promise<WebSocket> | WebSocket;
  pollIntervalMs?: number;
  heartbeatTimeoutMs?: number;
  reconnectBaseMs?: number;
  reconnectMaxMs?: number;
  maxSeenEvents?: number;
  now?: () => number;
}

type ActivityListener = (state: ActivityForegroundState) => void;

const DEFAULT_POLL_INTERVAL_MS = 30_000;
const DEFAULT_HEARTBEAT_TIMEOUT_MS = 45_000;
const DEFAULT_RECONNECT_BASE_MS = 1_000;
const DEFAULT_RECONNECT_MAX_MS = 15_000;

const initialConnection: ActivityConnectionState = {
  status: 'idle',
  lastCursor: null,
  lastHeartbeatAt: null,
  lastSnapshotAt: null,
  lastError: null,
  usingPollingFallback: false,
};

export class ActivityClient {
  private state: ActivityForegroundState = buildForegroundState(null, {
    ...initialConnection,
  });
  private readonly listeners = new Set<ActivityListener>();
  private readonly fetchSnapshotImpl: (
    filters: ActivityClientFilters
  ) => Promise<ActivityV1Snapshot>;
  private readonly openWebSocketImpl: (
    url: string
  ) => Promise<WebSocket> | WebSocket;
  private readonly pollIntervalMs: number;
  private readonly heartbeatTimeoutMs: number;
  private readonly reconnectBaseMs: number;
  private readonly reconnectMaxMs: number;
  private readonly maxSeenEvents: number;
  private readonly now: () => number;
  private readonly filters: ActivityClientFilters;
  private ws: WebSocket | null = null;
  private stopped = true;
  private reconnectAttempt = 0;
  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private readonly seenEvents: string[] = [];
  private readonly seenEventIds = new Set<string>();
  private readonly handleVisibilityChange = () => {
    if (isDocumentVisible()) {
      void this.pollOnce();
      if (!this.ws && !this.stopped) {
        void this.connect();
      }
    }
  };
  private readonly handleFocus = () => {
    if (!this.stopped) {
      void this.pollOnce();
    }
  };

  constructor(options: ActivityClientOptions = {}) {
    this.filters = options.filters ?? {};
    this.fetchSnapshotImpl = options.fetchSnapshot ?? fetchActivityV1Snapshot;
    this.openWebSocketImpl = options.openWebSocket ?? openActivityV1WebSocket;
    this.pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
    this.heartbeatTimeoutMs =
      options.heartbeatTimeoutMs ?? DEFAULT_HEARTBEAT_TIMEOUT_MS;
    this.reconnectBaseMs = options.reconnectBaseMs ?? DEFAULT_RECONNECT_BASE_MS;
    this.reconnectMaxMs = options.reconnectMaxMs ?? DEFAULT_RECONNECT_MAX_MS;
    this.maxSeenEvents = options.maxSeenEvents ?? 200;
    this.now = options.now ?? (() => Date.now());
  }

  start() {
    if (!this.stopped) return;
    this.stopped = false;
    this.attachForegroundListeners();
    this.startHeartbeatMonitor();
    void this.connect();
  }

  stop() {
    this.stopped = true;
    this.detachForegroundListeners();
    this.clearTimers();
    this.closeWebSocket();
    this.setConnection({ status: 'closed', usingPollingFallback: false });
  }

  subscribe(listener: ActivityListener): () => void {
    this.listeners.add(listener);
    listener(this.state);
    return () => {
      this.listeners.delete(listener);
    };
  }

  getSnapshot(): ActivityForegroundState {
    return this.state;
  }

  async pollOnce(): Promise<void> {
    try {
      const snapshot = await this.fetchSnapshotImpl(this.filters);
      this.applySnapshot(snapshot, {
        status: this.ws ? 'connected' : 'polling',
        usingPollingFallback: !this.ws,
        lastError: null,
      });
    } catch (error) {
      this.setConnection({
        status: 'error',
        lastError: productSafeText(errorMessage(error)),
        usingPollingFallback: !this.ws,
      });
    }
  }

  private async connect(): Promise<void> {
    if (this.stopped || !isDocumentVisible()) return;
    this.closeWebSocket();
    this.setConnection({
      status: this.reconnectAttempt > 0 ? 'reconnecting' : 'connecting',
      usingPollingFallback: false,
    });

    try {
      const ws = await this.openWebSocketImpl(
        buildActivityV1WsUrl(this.filters, this.state.connection.lastCursor)
      );
      if (this.stopped) {
        ws.close();
        return;
      }
      this.ws = ws;
      ws.onmessage = (event) => this.handleMessage(event.data);
      ws.onclose = () => this.handleSocketClosed();
      ws.onerror = () => this.handleSocketError();
      ws.onopen = () => {
        this.setConnection({ status: 'connected', lastError: null });
      };
    } catch (error) {
      this.handleConnectFailure(error);
    }
  }

  private handleMessage(data: unknown) {
    const event = parseActivityEvent(data);
    if (!event || this.isDuplicateEvent(event.event_id)) return;
    this.rememberEvent(event.event_id);
    this.setConnection({
      lastCursor: event.cursor,
      lastHeartbeatAt:
        event.event_type === 'heartbeat'
          ? this.now()
          : this.state.connection.lastHeartbeatAt,
      status: 'connected',
      usingPollingFallback: false,
      lastError: null,
    });

    if (event.event_type === 'snapshot' && event.snapshot) {
      this.applySnapshot(event.snapshot, {
        status: 'connected',
        usingPollingFallback: false,
        lastCursor: event.cursor,
      });
    }

    if (event.event_type === 'refresh_snapshot') {
      void this.pollOnce();
    }
  }

  private handleSocketError() {
    this.setConnection({
      status: 'error',
      lastError: 'Activity stream unavailable',
    });
  }

  private handleSocketClosed() {
    this.ws = null;
    if (this.stopped) return;
    this.scheduleReconnect();
  }

  private handleConnectFailure(error: unknown) {
    this.ws = null;
    this.setConnection({
      status: 'polling',
      lastError: productSafeText(errorMessage(error)),
      usingPollingFallback: true,
    });
    void this.pollOnce();
    this.scheduleReconnect();
  }

  private scheduleReconnect() {
    if (this.stopped) return;
    const delay = Math.min(
      this.reconnectMaxMs,
      this.reconnectBaseMs * 2 ** this.reconnectAttempt
    );
    this.reconnectAttempt += 1;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = setTimeout(() => void this.connect(), delay);
    this.schedulePoll();
  }

  private schedulePoll() {
    if (this.stopped || this.pollTimer) return;
    this.pollTimer = setTimeout(async () => {
      this.pollTimer = null;
      await this.pollOnce();
      if (!this.ws && !this.stopped) this.schedulePoll();
    }, this.pollIntervalMs);
  }

  private startHeartbeatMonitor() {
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = setInterval(
      () => {
        if (!this.ws || this.stopped) return;
        const lastSeen =
          this.state.connection.lastHeartbeatAt ??
          this.state.connection.lastSnapshotAt ??
          this.now();
        if (this.now() - lastSeen > this.heartbeatTimeoutMs) {
          this.closeWebSocket();
          this.setConnection({
            status: 'polling',
            lastError: 'Activity stream heartbeat timed out',
            usingPollingFallback: true,
          });
          void this.pollOnce();
          this.scheduleReconnect();
        }
      },
      Math.max(1_000, Math.floor(this.heartbeatTimeoutMs / 3))
    );
  }

  private attachForegroundListeners() {
    document?.addEventListener?.(
      'visibilitychange',
      this.handleVisibilityChange
    );
    window?.addEventListener?.('focus', this.handleFocus);
  }

  private detachForegroundListeners() {
    document?.removeEventListener?.(
      'visibilitychange',
      this.handleVisibilityChange
    );
    window?.removeEventListener?.('focus', this.handleFocus);
  }

  private clearTimers() {
    if (this.pollTimer) clearTimeout(this.pollTimer);
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    this.pollTimer = null;
    this.reconnectTimer = null;
    this.heartbeatTimer = null;
  }

  private closeWebSocket() {
    const ws = this.ws;
    this.ws = null;
    if (!ws) return;
    ws.onclose = null;
    ws.onerror = null;
    ws.onmessage = null;
    ws.onopen = null;
    ws.close();
  }

  private isDuplicateEvent(eventId: string): boolean {
    return this.seenEventIds.has(eventId);
  }

  private rememberEvent(eventId: string) {
    this.seenEventIds.add(eventId);
    this.seenEvents.push(eventId);
    while (this.seenEvents.length > this.maxSeenEvents) {
      const removed = this.seenEvents.shift();
      if (removed) this.seenEventIds.delete(removed);
    }
  }

  private applySnapshot(
    snapshot: ActivityV1Snapshot,
    connection: Partial<ActivityConnectionState>
  ) {
    this.reconnectAttempt = 0;
    const safeSnapshot = sanitizeSnapshot(snapshot);
    this.state = buildForegroundState(safeSnapshot, {
      ...this.state.connection,
      ...connection,
      lastSnapshotAt: this.now(),
    });
    this.emit();
  }

  private setConnection(connection: Partial<ActivityConnectionState>) {
    this.state = {
      ...this.state,
      connection: {
        ...this.state.connection,
        ...connection,
      },
    };
    this.emit();
  }

  private emit() {
    for (const listener of this.listeners) {
      listener(this.state);
    }
  }
}

export async function fetchActivityV1Snapshot(
  filters: ActivityClientFilters = {}
): Promise<ActivityV1Snapshot> {
  const params = new URLSearchParams();
  if (filters.workspaceId) params.set('workspace_id', filters.workspaceId);
  if (filters.sessionId) params.set('session_id', filters.sessionId);
  const suffix = params.toString();
  const response = await makeLocalApiRequest(
    `/api/activity/v1${suffix ? `?${suffix}` : ''}`
  );
  return handleApiResponse<ActivityV1Snapshot>(response);
}

export function openActivityV1WebSocket(url: string): Promise<WebSocket> {
  return openLocalApiWebSocket(url);
}

export function buildActivityV1WsUrl(
  filters: ActivityClientFilters = {},
  cursor: string | null = null
): string {
  const params = new URLSearchParams();
  if (filters.workspaceId) params.set('workspace_id', filters.workspaceId);
  if (filters.sessionId) params.set('session_id', filters.sessionId);
  if (cursor) params.set('cursor', cursor);
  const suffix = params.toString();
  return `/api/activity/v1/ws${suffix ? `?${suffix}` : ''}`;
}

function buildForegroundState(
  snapshot: ActivityV1Snapshot | null,
  connection: ActivityConnectionState
): ActivityForegroundState {
  const callbacks = snapshot?.workspaces.flatMap((workspace) =>
    workspace.sessions.flatMap((session) => session.callbacks)
  );
  return {
    snapshot,
    connection,
    summary: {
      activeTurnCount: snapshot?.summary.active_turn_count ?? 0,
      pendingTurnCount: snapshot?.summary.pending_turn_count ?? 0,
      callbackWaitingCount: snapshot?.summary.callback_waiting_count ?? 0,
      recentCallbackCount: snapshot?.summary.recent_callback_count ?? 0,
    },
    callbacks: callbacks ?? [],
  };
}

function parseActivityEvent(data: unknown): ActivityV1WsEvent | null {
  try {
    const parsed = typeof data === 'string' ? JSON.parse(data) : data;
    if (
      !parsed ||
      typeof parsed !== 'object' ||
      (parsed as ActivityV1WsEvent).schema_version !== 'activity.v1.ws'
    ) {
      return null;
    }
    return parsed as ActivityV1WsEvent;
  } catch {
    return null;
  }
}

function sanitizeSnapshot(snapshot: ActivityV1Snapshot): ActivityV1Snapshot {
  return sanitizeValue(snapshot) as ActivityV1Snapshot;
}

function sanitizeValue(value: unknown): unknown {
  if (typeof value === 'string') return productSafeText(value);
  if (Array.isArray(value)) return value.map(sanitizeValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [key, sanitizeValue(child)])
    );
  }
  return value;
}

export function productSafeText(value: string): string {
  let text = value.replace(/\bqueue[_ -]?item\b/gi, 'pending work');
  text = text.replace(/\bwebhook\b/gi, 'connection');
  text = text.replace(/\bhmac\b/gi, 'signature');
  text = text.replace(/\btrigger\b/gi, 'automation event');
  text = text.replace(/\bdelivery[_ ]?id\b/gi, 'delivery');
  text = text.replace(/\bexecution[_ ]?process[_ ]?id\b/gi, 'run');
  text = text.replace(/\bprovider diagnostics\b/gi, 'status details');
  text = text.replace(/\braw\s+xml\b/gi, 'structured response');
  text = text.replace(/\braw\s+json\b/gi, 'structured data');
  text = text.replace(/\bbd\s+show\b/gi, 'task details');
  text = text.replace(/\bgit\s+status\b/gi, 'version-control status');
  text = text.replace(/\bshell\b/gi, 'automation');
  text = text.replace(
    /(?:^|\s)(?:\/Users|\/tmp|\/private\/var)\/\S+/gi,
    (match) =>
      match.startsWith(' ') ? ' workspace location' : 'workspace location'
  );
  return text.length > 500 ? `${text.slice(0, 497)}...` : text;
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return 'Activity unavailable';
}

function isDocumentVisible(): boolean {
  return (
    typeof document === 'undefined' || document.visibilityState !== 'hidden'
  );
}
