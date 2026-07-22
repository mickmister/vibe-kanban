import { describe, expect, it } from 'vitest';
import {
  ExecutionProcessStatus,
  type ExecutionProcess,
  type ExecutionProcessRunReason,
} from 'shared/types';
import {
  getStopExecutionMutationKey,
  getStoppableExecutionProcesses,
} from './useWorkspaceExecution';

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
  });
});
