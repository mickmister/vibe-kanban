import { describe, expect, it } from 'vitest';
import {
  BaseCodingAgent,
  ExecutionProcessStatus,
  type ExecutorAction,
} from 'shared/types';

import { deriveConversationEntries } from './deriveConversationEntries';
import type {
  ConversationTimelineSource,
  ExecutionProcessState,
} from '@/shared/hooks/useConversationHistory/types';

const executorAction = (prompt: string): ExecutorAction => ({
  typ: {
    type: 'CodingAgentInitialRequest',
    prompt,
    executor_config: {
      executor: BaseCodingAgent.CLAUDE_CODE,
    },
    working_dir: null,
  },
  next_action: null,
});

const agentProcess = ({
  id,
  prompt,
  createdAt,
  updatedAt,
  entries,
}: {
  id: string;
  prompt: string;
  createdAt: string;
  updatedAt: string;
  entries: ExecutionProcessState['entries'];
}): ExecutionProcessState => ({
  executionProcess: {
    id,
    created_at: createdAt,
    updated_at: updatedAt,
    executor_action: executorAction(prompt),
  },
  entries,
});

describe('deriveConversationEntries', () => {
  it('adds local-displayable timestamps to user prompts and only the final assistant message', () => {
    const source: ConversationTimelineSource = {
      executionProcessState: {
        process_1: agentProcess({
          id: 'process_1',
          prompt: 'Make dinner',
          createdAt: '2026-06-04T21:19:28.000Z',
          updatedAt: '2026-06-04T21:21:00.000Z',
          entries: [
            {
              type: 'NORMALIZED_ENTRY',
              content: {
                entry_type: { type: 'assistant_message' },
                content: 'Chopping vegetables.',
                timestamp: null,
              },
              patchKey: 'process_1:0',
              executionProcessId: 'process_1',
            },
            {
              type: 'NORMALIZED_ENTRY',
              content: {
                entry_type: { type: 'assistant_message' },
                content: 'Dinner is ready.',
                timestamp: null,
              },
              patchKey: 'process_1:1',
              executionProcessId: 'process_1',
            },
          ],
        }),
      },
      liveExecutionProcesses: [],
    };

    const result = deriveConversationEntries({
      source,
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
    const source: ConversationTimelineSource = {
      executionProcessState: {
        process_1: agentProcess({
          id: 'process_1',
          prompt: 'Keep cooking',
          createdAt: '2026-06-04T21:19:28.000Z',
          updatedAt: '2026-06-04T21:21:00.000Z',
          entries: [
            {
              type: 'NORMALIZED_ENTRY',
              content: {
                entry_type: { type: 'assistant_message' },
                content: 'Still cooking.',
                timestamp: '2026-06-04T21:20:00.000Z',
              },
              patchKey: 'process_1:0',
              executionProcessId: 'process_1',
            },
          ],
        }),
      },
      liveExecutionProcesses: [
        {
          id: 'process_1',
          session_id: 'session_1',
          run_reason: 'codingagent',
          executor_action: executorAction('Keep cooking'),
          status: ExecutionProcessStatus.running,
          exit_code: null,
          dropped: false,
          started_at: '2026-06-04T21:19:28.000Z',
          completed_at: null,
          created_at: '2026-06-04T21:19:28.000Z',
          updated_at: '2026-06-04T21:21:00.000Z',
        },
      ],
    };

    const result = deriveConversationEntries({
      source,
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
});
