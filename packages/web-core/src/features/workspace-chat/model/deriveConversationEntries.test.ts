import { describe, expect, it } from 'vitest';
import {
  BaseCodingAgent,
  ExecutionProcessStatus,
  type ExecutorAction,
  type NormalizedEntry,
} from 'shared/types';

import { deriveConversationEntries } from './deriveConversationEntries';
import type {
  ConversationTimelineSource,
  ExecutionProcessState,
  PatchTypeWithKey,
} from '@/shared/hooks/useConversationHistory/types';

const codexExecutorConfig = {
  executor: BaseCodingAgent.CODEX,
  variant: null,
  model_id: null,
  agent_id: null,
  reasoning_id: null,
  permission_policy: null,
} as const;

function initialExecutorAction(prompt: string): ExecutorAction {
  return {
    typ: {
      type: 'CodingAgentInitialRequest',
      prompt,
      session_id: null,
      executor_config: codexExecutorConfig,
      working_dir: null,
    },
    next_action: null,
  };
}

function sessionCommandExecutorAction(
  command: 'clear' | 'compact',
  prompt = `/${command}`
): ExecutorAction {
  return {
    typ: {
      type: 'CodingAgentSessionCommandRequest',
      command:
        command === 'clear'
          ? { type: 'clear' }
          : { type: 'compact', instructions: null },
      prompt,
      session_id: 'thread-1',
      executor_config: codexExecutorConfig,
      working_dir: null,
    },
    next_action: null,
  };
}

function normalEntry(
  processId: string,
  entry: NormalizedEntry,
  index: number
): PatchTypeWithKey {
  return {
    type: 'NORMALIZED_ENTRY',
    content: entry,
    patchKey: `${processId}:${index}`,
    executionProcessId: processId,
  };
}

function processState({
  id,
  createdAt,
  updatedAt = createdAt,
  action,
  entries,
}: {
  id: string;
  createdAt: string;
  updatedAt?: string;
  action: ExecutorAction;
  entries: PatchTypeWithKey[];
}): ExecutionProcessState {
  return {
    executionProcess: {
      id,
      created_at: createdAt,
      updated_at: updatedAt,
      executor_action: action,
    },
    entries,
  };
}

function source(
  processes: ExecutionProcessState[],
  liveExecutionProcesses: ConversationTimelineSource['liveExecutionProcesses'] = []
): ConversationTimelineSource {
  return {
    executionProcessState: Object.fromEntries(
      processes.map((process) => [process.executionProcess.id, process])
    ),
    liveExecutionProcesses,
  };
}

describe('deriveConversationEntries', () => {
  it('adds local-displayable timestamps to user prompts and only the final assistant message', () => {
    const processId = 'process_1';
    const result = deriveConversationEntries({
      source: source([
        processState({
          id: processId,
          createdAt: '2026-06-04T21:19:28.000Z',
          updatedAt: '2026-06-04T21:21:00.000Z',
          action: initialExecutorAction('Make dinner'),
          entries: [
            normalEntry(
              processId,
              {
                entry_type: { type: 'assistant_message' },
                content: 'Chopping vegetables.',
                timestamp: null,
              },
              0
            ),
            normalEntry(
              processId,
              {
                entry_type: { type: 'assistant_message' },
                content: 'Dinner is ready.',
                timestamp: null,
              },
              1
            ),
          ],
        }),
      ]),
      scriptOutputCache: new Map(),
    });

    const normalizedEntries = result.entries.filter(
      (entry) => entry.type === 'NORMALIZED_ENTRY'
    );

    expect(normalizedEntries[0].content.entry_type.type).toBe('user_message');
    expect(normalizedEntries[0].content.timestamp).toBe(
      '2026-06-04T21:19:28.000Z'
    );

    expect(normalizedEntries[1].content.entry_type.type).toBe(
      'assistant_message'
    );
    expect(normalizedEntries[1].content.timestamp).toBeNull();

    expect(normalizedEntries[2].content.entry_type.type).toBe(
      'assistant_message'
    );
    expect(normalizedEntries[2].content.timestamp).toBe(
      '2026-06-04T21:21:00.000Z'
    );
  });

  it('does not timestamp the latest assistant message while an agent turn is still running', () => {
    const processId = 'process_1';
    const result = deriveConversationEntries({
      source: source(
        [
          processState({
            id: processId,
            createdAt: '2026-06-04T21:19:28.000Z',
            updatedAt: '2026-06-04T21:21:00.000Z',
            action: initialExecutorAction('Keep cooking'),
            entries: [
              normalEntry(
                processId,
                {
                  entry_type: { type: 'assistant_message' },
                  content: 'Still cooking.',
                  timestamp: '2026-06-04T21:20:00.000Z',
                },
                0
              ),
            ],
          }),
        ],
        [
          {
            id: processId,
            session_id: 'session_1',
            run_reason: 'codingagent',
            executor_action: initialExecutorAction('Keep cooking'),
            status: ExecutionProcessStatus.running,
            exit_code: null,
            dropped: false,
            started_at: '2026-06-04T21:19:28.000Z',
            completed_at: null,
            created_at: '2026-06-04T21:19:28.000Z',
            updated_at: '2026-06-04T21:21:00.000Z',
          },
        ]
      ),
      scriptOutputCache: new Map(),
    });

    const assistantEntry = result.entries.find(
      (entry) =>
        entry.type === 'NORMALIZED_ENTRY' &&
        entry.content.entry_type.type === 'assistant_message'
    );

    expect(assistantEntry?.type).toBe('NORMALIZED_ENTRY');
    expect(
      assistantEntry?.type === 'NORMALIZED_ENTRY'
        ? assistantEntry.content.timestamp
        : undefined
    ).toBeNull();
  });

  it('emits /clear as a user message and resets stale token usage', () => {
    const initialProcessId = 'initial-process';
    const clearProcessId = 'clear-process';
    const result = deriveConversationEntries({
      source: source([
        processState({
          id: initialProcessId,
          createdAt: '2026-06-17T00:00:00.000Z',
          action: initialExecutorAction('Hello'),
          entries: [
            normalEntry(
              initialProcessId,
              {
                entry_type: {
                  type: 'token_usage_info',
                  total_tokens: 1234,
                  model_context_window: 200_000,
                },
                content: '',
                timestamp: null,
              },
              0
            ),
          ],
        }),
        processState({
          id: clearProcessId,
          createdAt: '2026-06-17T00:01:00.000Z',
          action: sessionCommandExecutorAction('clear'),
          entries: [
            normalEntry(
              clearProcessId,
              {
                entry_type: { type: 'system_message' },
                content: 'Context cleared.',
                timestamp: null,
              },
              0
            ),
          ],
        }),
      ]),
      scriptOutputCache: new Map(),
    });

    expect(
      result.entries.some(
        (entry) =>
          entry.type === 'NORMALIZED_ENTRY' &&
          entry.content.entry_type.type === 'user_message' &&
          entry.content.content === '/clear'
      )
    ).toBe(true);
    expect(result.latestTokenUsageInfo).toEqual({
      total_tokens: 0,
      model_context_window: 200_000,
    });
  });

  it('emits the original /compact prompt and does not retain stale token usage', () => {
    const initialProcessId = 'initial-process';
    const compactProcessId = 'compact-process';
    const result = deriveConversationEntries({
      source: source([
        processState({
          id: initialProcessId,
          createdAt: '2026-06-17T00:00:00.000Z',
          action: initialExecutorAction('Hello'),
          entries: [
            normalEntry(
              initialProcessId,
              {
                entry_type: {
                  type: 'token_usage_info',
                  total_tokens: 1234,
                  model_context_window: 200_000,
                },
                content: '',
                timestamp: null,
              },
              0
            ),
          ],
        }),
        processState({
          id: compactProcessId,
          createdAt: '2026-06-17T00:01:00.000Z',
          action: sessionCommandExecutorAction(
            'compact',
            '/compact keep imports stable'
          ),
          entries: [
            normalEntry(
              compactProcessId,
              {
                entry_type: { type: 'system_message' },
                content: 'Context compacted.',
                timestamp: null,
              },
              0
            ),
          ],
        }),
      ]),
      scriptOutputCache: new Map(),
    });

    expect(
      result.entries.some(
        (entry) =>
          entry.type === 'NORMALIZED_ENTRY' &&
          entry.content.entry_type.type === 'user_message' &&
          entry.content.content === '/compact keep imports stable'
      )
    ).toBe(true);
    expect(result.latestTokenUsageInfo).toBeNull();
  });
});
