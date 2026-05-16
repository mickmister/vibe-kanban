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

export function postWorkspaceNavigationToIframeHost(
  destination: AppDestination
): boolean {
  if (!('workspaceId' in destination)) {
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
