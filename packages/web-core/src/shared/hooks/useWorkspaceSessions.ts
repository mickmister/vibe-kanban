import { useQuery } from '@tanstack/react-query';
import { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { sessionsApi } from '@/shared/lib/api';
import { useHostId } from '@/shared/providers/HostIdProvider';
import { workspaceSessionKeys } from '@/shared/hooks/workspaceSessionKeys';
import type { Session } from 'shared/types';

interface UseWorkspaceSessionsOptions {
  enabled?: boolean;
  selectedSessionId?: string;
}

/** Discriminated union for session selection state */
export type SessionSelection =
  | { mode: 'existing'; sessionId: string }
  | { mode: 'new' };

interface UseWorkspaceSessionsResult {
  sessions: Session[];
  selectedSession: Session | undefined;
  selectedSessionId: string | undefined;
  selectSession: (sessionId: string) => void;
  selectLatestSession: () => void;
  isLoading: boolean;
  /** Whether user is creating a new session */
  isNewSessionMode: boolean;
  /** Enter new session mode */
  startNewSession: () => void;
}

/**
 * Hook for managing sessions within a workspace.
 * Fetches all sessions for a workspace and provides session switching capability.
 * Sessions are ordered by most recently used (latest non-dev server execution first).
 */
export function useWorkspaceSessions(
  workspaceId: string | undefined,
  options: UseWorkspaceSessionsOptions = {}
): UseWorkspaceSessionsResult {
  const hostId = useHostId();
  const { enabled = true, selectedSessionId: linkedSessionId } = options;
  const [selection, setSelection] = useState<SessionSelection | undefined>(
    undefined
  );
  const prevWorkspaceIdRef = useRef(workspaceId);

  const { data: sessions = [], isLoading } = useQuery<Session[]>({
    queryKey: workspaceSessionKeys.byWorkspace(workspaceId, hostId),
    queryFn: () => sessionsApi.getByWorkspace(workspaceId!),
    enabled: enabled && !!workspaceId,
  });

  // Combined effect: handle workspace changes, deep-linked sessions, and auto-select sessions
  // This replaces two separate effects that had a race condition where the reset
  // effect would fire after auto-select when sessions were cached, undoing the selection.
  useEffect(() => {
    const workspaceChanged = prevWorkspaceIdRef.current !== workspaceId;
    prevWorkspaceIdRef.current = workspaceId;

    setSelection((prev) =>
      resolveNextSessionSelection({
        previous: prev,
        workspaceChanged,
        sessions,
        linkedSessionId,
      })
    );
  }, [workspaceId, sessions, linkedSessionId]);

  const isNewSessionMode = selection?.mode === 'new' || sessions.length === 0;
  const selectedSessionId =
    selection?.mode === 'existing' ? selection.sessionId : undefined;

  const selectedSession = useMemo(
    () => sessions.find((s) => s.id === selectedSessionId),
    [sessions, selectedSessionId]
  );

  const selectSession = useCallback((sessionId: string) => {
    setSelection({ mode: 'existing', sessionId });
  }, []);

  const selectLatestSession = useCallback(() => {
    if (sessions.length > 0) {
      setSelection({ mode: 'existing', sessionId: sessions[0].id });
    }
  }, [sessions]);

  const startNewSession = useCallback(() => {
    setSelection({ mode: 'new' });
  }, []);

  return {
    sessions,
    selectedSession,
    selectedSessionId,
    selectSession,
    selectLatestSession,
    isLoading,
    isNewSessionMode,
    startNewSession,
  };
}

export function resolveNextSessionSelection({
  previous,
  workspaceChanged,
  sessions,
  linkedSessionId,
}: {
  previous: SessionSelection | undefined;
  workspaceChanged: boolean;
  sessions: Session[];
  linkedSessionId?: string;
}): SessionSelection | undefined {
  if (sessions.length === 0) return undefined;

  if (
    linkedSessionId &&
    sessions.some((session) => session.id === linkedSessionId)
  ) {
    return { mode: 'existing', sessionId: linkedSessionId };
  }

  if (previous?.mode === 'new' && !workspaceChanged) return previous;

  if (
    previous?.mode === 'existing' &&
    !workspaceChanged &&
    sessions.some((session) => session.id === previous.sessionId)
  ) {
    return previous;
  }

  // Sessions are ordered by most recently used, so first is the most recently used.
  return { mode: 'existing', sessionId: sessions[0].id };
}
