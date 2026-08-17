import { describe, expect, it } from 'vitest';
import {
  AgentMessageQueueStatus,
  AgentMessageSource,
  type AgentMessageQueueItem,
  type QueueStatusSummary,
} from 'shared/types';
import { deriveSessionQueueInteractionState } from './useSessionQueueInteraction';

function queueItem(
  overrides: Partial<AgentMessageQueueItem> & {
    id: string;
    source: AgentMessageSource;
    message: string;
  }
): AgentMessageQueueItem {
  return {
    id: overrides.id,
    session_id: 'session-1',
    workspace_id: 'workspace-1',
    status: AgentMessageQueueStatus.pending,
    source: overrides.source,
    priority: 0n,
    data: { message: overrides.message },
    started_execution_process_id: null,
    lease_owner: null,
    lease_expires_at: null,
    attempt_count: 0n,
    last_error: null,
    queued_at: '2026-08-17T00:00:00.000Z',
    created_at: '2026-08-17T00:00:00.000Z',
    updated_at: '2026-08-17T00:00:00.000Z',
    ...overrides,
  };
}

function queueStatus(messages: AgentMessageQueueItem[]): QueueStatusSummary {
  return {
    status: messages.length > 0 ? 'queued' : 'empty',
    count: messages.length,
    messages,
    message: messages[0] ?? null,
  };
}

describe('deriveSessionQueueInteractionState', () => {
  it('does not treat workflow queued work as a user queued draft', () => {
    const workflowPrompt =
      '<workflow-internal>do not leak me</workflow-internal>';

    const state = deriveSessionQueueInteractionState(
      queueStatus([
        queueItem({
          id: 'queue-workflow-1',
          source: AgentMessageSource.workflow,
          message: workflowPrompt,
        }),
      ])
    );

    expect(state.isQueued).toBe(false);
    expect(state.queuedCount).toBe(0);
    expect(state.queuedMessages).toEqual([]);
    expect(state.queuedMessage).toBeNull();
    expect(state.automationQueuedCount).toBe(1);
    expect(state.hasAutomationQueuedWork).toBe(true);
  });

  it('still exposes user queued drafts for the Cancel Queue composer flow', () => {
    const state = deriveSessionQueueInteractionState(
      queueStatus([
        queueItem({
          id: 'queue-user-1',
          source: AgentMessageSource.from_user,
          message: 'please continue',
        }),
      ])
    );

    expect(state.isQueued).toBe(true);
    expect(state.queuedCount).toBe(1);
    expect(state.queuedMessages.map((message) => message.id)).toEqual([
      'queue-user-1',
    ]);
    expect(state.queuedMessage).toBe('please continue');
    expect(state.automationQueuedCount).toBe(0);
    expect(state.hasAutomationQueuedWork).toBe(false);
  });

  it('separates user drafts from automation work when both are queued', () => {
    const state = deriveSessionQueueInteractionState(
      queueStatus([
        queueItem({
          id: 'queue-workflow-1',
          source: AgentMessageSource.workflow,
          message: 'workflow generated prompt',
        }),
        queueItem({
          id: 'queue-user-1',
          source: AgentMessageSource.from_user,
          message: 'my queued draft',
        }),
      ])
    );

    expect(state.isQueued).toBe(true);
    expect(state.queuedCount).toBe(1);
    expect(state.queuedMessages.map((message) => message.id)).toEqual([
      'queue-user-1',
    ]);
    expect(state.queuedMessage).toBe('my queued draft');
    expect(state.automationQueuedCount).toBe(1);
  });
});
