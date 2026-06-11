import type { AppDestination } from './routes/appNavigation';

const VIBE_KANBAN_IFRAME_MESSAGE_SOURCE = 'vibe-kanban';

type WorkspaceDestinationWithId = Extract<
  AppDestination,
  { workspaceId: string }
>;

type VibeKanbanIframeMessage =
  | {
      source: typeof VIBE_KANBAN_IFRAME_MESSAGE_SOURCE;
      version: 1;
      event: 'workspace:navigate';
      workspaceId: string;
      destinationKind: WorkspaceDestinationWithId['kind'];
      hostId?: string;
    }
  | {
      source: typeof VIBE_KANBAN_IFRAME_MESSAGE_SOURCE;
      version: 1;
      event: 'workspace:message-submitted';
      workspaceId: string;
      sessionId?: string;
      isNewSessionMode: boolean;
    }
  | {
      source: typeof VIBE_KANBAN_IFRAME_MESSAGE_SOURCE;
      version: 1;
      event: 'host:open-sidebar';
      requestId: string;
    };

type VibeDashboardIframeAck = {
  source: 'vibe-dashboard';
  version: 1;
  event: 'host:open-sidebar:ack';
  requestId: string;
};

function isRunningInIframe(): boolean {
  return typeof window !== 'undefined' && window.parent !== window;
}

function postToIframeHost(message: VibeKanbanIframeMessage): boolean {
  if (!isRunningInIframe()) {
    return false;
  }

  window.parent.postMessage(message, '*');
  return true;
}

function createRequestId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID();
  }

  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function isSidebarOpenAck(
  value: unknown,
  requestId: string
): value is VibeDashboardIframeAck {
  if (!value || typeof value !== 'object') {
    return false;
  }

  const data = value as Record<string, unknown>;
  return (
    data.source === 'vibe-dashboard' &&
    data.version === 1 &&
    data.event === 'host:open-sidebar:ack' &&
    data.requestId === requestId
  );
}

export function postWorkspaceNavigationToIframeHost(
  destination: AppDestination
): boolean {
  if (destination.kind !== 'workspace') {
    return false;
  }

  return postToIframeHost({
    source: VIBE_KANBAN_IFRAME_MESSAGE_SOURCE,
    version: 1,
    event: 'workspace:navigate',
    workspaceId: destination.workspaceId,
    destinationKind: destination.kind,
    ...('hostId' in destination && destination.hostId
      ? { hostId: destination.hostId }
      : {}),
  });
}

export function postWorkspaceMessageSubmittedToIframeHost(args: {
  workspaceId: string;
  sessionId?: string;
  isNewSessionMode: boolean;
}): void {
  postToIframeHost({
    source: VIBE_KANBAN_IFRAME_MESSAGE_SOURCE,
    version: 1,
    event: 'workspace:message-submitted',
    workspaceId: args.workspaceId,
    ...(args.sessionId ? { sessionId: args.sessionId } : {}),
    isNewSessionMode: args.isNewSessionMode,
  });
}

export function openIframeHostSidebarOrFallback(fallback: () => void): void {
  if (!isRunningInIframe()) {
    fallback();
    return;
  }

  const requestId = createRequestId();
  let fallbackTimeoutId: number | undefined;

  const cleanup = () => {
    window.removeEventListener('message', handleAck);
    if (fallbackTimeoutId !== undefined) {
      window.clearTimeout(fallbackTimeoutId);
    }
  };

  const handleAck = (event: MessageEvent) => {
    if (event.source !== window.parent) {
      return;
    }

    if (!isSidebarOpenAck(event.data, requestId)) {
      return;
    }

    cleanup();
  };

  window.addEventListener('message', handleAck);
  fallbackTimeoutId = window.setTimeout(() => {
    cleanup();
    fallback();
  }, 250);

  const posted = postToIframeHost({
    source: VIBE_KANBAN_IFRAME_MESSAGE_SOURCE,
    version: 1,
    event: 'host:open-sidebar',
    requestId,
  });

  if (!posted) {
    cleanup();
    fallback();
  }
}
