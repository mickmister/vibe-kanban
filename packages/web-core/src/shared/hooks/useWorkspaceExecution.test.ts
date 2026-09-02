/**
 * @vitest-environment jsdom
 */

import React, { act, useEffect, useRef } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createRoot } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ExecutionProcessStatus,
  type ExecutionProcess,
  type ExecutionProcessRunReason,
} from 'shared/types';
import {
  getStopExecutionMutationKey,
  getStoppableExecutionProcesses,
  useWorkspaceExecution,
} from './useWorkspaceExecution';
import {
  ExecutionProcessesContext,
  type ExecutionProcessesContextType,
} from './useExecutionProcessesContext';
import { sessionsApi } from '@/shared/lib/api';

vi.mock('@/shared/lib/api', () => ({
  executionProcessesApi: {
    getDetails: vi.fn(),
  },
  sessionsApi: {
    stopExecution: vi.fn().mockResolvedValue(undefined),
  },
}));

const stopExecutionMock = vi.mocked(sessionsApi.stopExecution);

(
  globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const process = (
  id: string,
  runReason: ExecutionProcessRunReason,
  status: ExecutionProcessStatus
): ExecutionProcess => ({
  id,
  session_id: 'session-1',
  run_reason: runReason,
  executor_action: {
    type: 'ScriptRequest',
  } as ExecutionProcess['executor_action'],
  status,
  exit_code: null,
  dropped: false,
  started_at: '2026-06-17T00:00:00.000Z',
  completed_at: null,
  created_at: '2026-06-17T00:00:00.000Z',
  updated_at: '2026-06-17T00:00:00.000Z',
});

afterEach(() => {
  vi.clearAllMocks();
});

describe('getStoppableExecutionProcesses', () => {
  it('targets only running non-dev-server agent/script processes', () => {
    const processes = [
      process('running-agent', 'codingagent', ExecutionProcessStatus.running),
      process('running-setup', 'setupscript', ExecutionProcessStatus.running),
      process(
        'running-cleanup',
        'cleanupscript',
        ExecutionProcessStatus.running
      ),
      process(
        'running-archive',
        'archivescript',
        ExecutionProcessStatus.running
      ),
      process(
        'running-dev-server',
        'devserver',
        ExecutionProcessStatus.running
      ),
      process(
        'completed-agent',
        'codingagent',
        ExecutionProcessStatus.completed
      ),
      process('failed-agent', 'codingagent', ExecutionProcessStatus.failed),
      process('killed-agent', 'codingagent', ExecutionProcessStatus.killed),
    ];

    expect(
      getStoppableExecutionProcesses(processes).map(({ id }) => id)
    ).toEqual([
      'running-agent',
      'running-setup',
      'running-cleanup',
      'running-archive',
    ]);
  });
});

describe('getStopExecutionMutationKey', () => {
  it('scopes stop pending state by session', () => {
    expect(getStopExecutionMutationKey('workspace-1', 'session-1')).toEqual([
      'stopSessionExecution',
      'workspace-1',
      'session-1',
    ]);
    expect(getStopExecutionMutationKey('workspace-1', 'session-2')).toEqual([
      'stopSessionExecution',
      'workspace-1',
      'session-2',
    ]);
  });
});

describe('useWorkspaceExecution', () => {
  it('stops the provider session when the process list is empty', async () => {
    await renderAndStop({
      sessionId: 'provider-session',
      executionProcessesVisible: [],
    });

    expect(stopExecutionMock).toHaveBeenCalledWith('provider-session');
  });

  it('stops the provider session when visible processes contain a stale session id', async () => {
    await renderAndStop({
      sessionId: 'provider-session',
      executionProcessesVisible: [
        {
          id: 'process-1',
          session_id: 'stale-session',
          run_reason: 'codingagent',
          status: 'running',
        } as ExecutionProcess,
      ],
    });

    expect(stopExecutionMock).toHaveBeenCalledWith('provider-session');
  });
});

async function renderAndStop(
  contextOverrides: Partial<ExecutionProcessesContextType>
) {
  const queryClient = new QueryClient();
  const container = document.createElement('div');
  document.body.appendChild(container);
  const root = createRoot(container);

  const contextValue: ExecutionProcessesContextType = {
    sessionId: undefined,
    executionProcessesAll: [],
    executionProcessesByIdAll: {},
    isAttemptRunningAll: false,
    executionProcessesVisible: [],
    executionProcessesByIdVisible: {},
    isAttemptRunningVisible: false,
    isLoading: false,
    isConnected: true,
    error: null,
    ...contextOverrides,
  };

  function StopOnMount() {
    const { stopExecution } = useWorkspaceExecution('workspace-1');
    const didStop = useRef(false);

    useEffect(() => {
      if (didStop.current) return;
      didStop.current = true;
      void stopExecution();
    }, [stopExecution]);

    return null;
  }

  await act(async () => {
    root.render(
      React.createElement(
        QueryClientProvider,
        { client: queryClient },
        React.createElement(
          ExecutionProcessesContext.Provider,
          { value: contextValue },
          React.createElement(StopOnMount)
        )
      )
    );
  });

  await act(async () => {
    await Promise.resolve();
  });

  await act(async () => {
    root.unmount();
  });
  container.remove();
}
