import { describe, expect, it } from 'vitest';
import { getStopExecutionMutationKey } from './useWorkspaceExecution';

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
