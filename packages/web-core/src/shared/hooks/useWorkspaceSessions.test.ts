import { describe, expect, it } from 'vitest';
import type { Session } from 'shared/types';
import { resolveNextSessionSelection } from './useWorkspaceSessions';

describe('resolveNextSessionSelection', () => {
  it('selects a deep-linked session when it belongs to the workspace', () => {
    expect(
      resolveNextSessionSelection({
        previous: undefined,
        workspaceChanged: false,
        sessions: [session('latest'), session('target')],
        linkedSessionId: 'target',
      })
    ).toEqual({ mode: 'existing', sessionId: 'target' });
  });

  it('does not override an existing selection for an invalid deep link', () => {
    expect(
      resolveNextSessionSelection({
        previous: { mode: 'existing', sessionId: 'current' },
        workspaceChanged: false,
        sessions: [session('latest'), session('current')],
        linkedSessionId: 'missing',
      })
    ).toEqual({ mode: 'existing', sessionId: 'current' });
  });

  it('falls back to the latest session on workspace changes without a valid deep link', () => {
    expect(
      resolveNextSessionSelection({
        previous: { mode: 'existing', sessionId: 'old-workspace-session' },
        workspaceChanged: true,
        sessions: [session('latest'), session('other')],
        linkedSessionId: undefined,
      })
    ).toEqual({ mode: 'existing', sessionId: 'latest' });
  });
});

function session(id: string): Session {
  return {
    id,
    workspace_id: 'workspace-1',
    name: null,
    executor: null,
    agent_working_dir: null,
    context_reset_execution_process_id: null,
    created_at: '2026-07-30T00:00:00Z',
    updated_at: '2026-07-30T00:00:00Z',
  };
}
