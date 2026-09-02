import { useCallback } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { queueApi } from '@/shared/lib/api';
import { AgentMessageSource, QueueStatusKind } from 'shared/types';
import type {
  AgentMessageQueueItem,
  ExecutorConfig,
  QueueStatusSummary,
} from 'shared/types';

interface UseSessionQueueInteractionOptions {
  /** Session ID for queue operations */
  sessionId: string | undefined;
}

interface UseSessionQueueInteractionResult {
  /** Whether one or more user-authored follow-up drafts are queued */
  isQueued: boolean;
  /** Number of pending user-authored follow-up drafts */
  queuedCount: number;
  /** Pending user-authored follow-up draft queue messages */
  queuedMessages: AgentMessageQueueItem[];
  /** Number of queued messages produced by workflow/system/agent automation */
  automationQueuedCount: number;
  /** Whether workflow/system/agent automation has queued work for this session */
  hasAutomationQueuedWork: boolean;
  /** First user-authored queued message content, if any */
  queuedMessage: string | null;
  /** The executor config from the first queued message, if any */
  queuedConfig: ExecutorConfig | null;
  /** Whether a queue operation is in progress */
  isQueueLoading: boolean;
  /** Queue a message for later execution */
  queueMessage: (message: string) => Promise<void>;
  /** Cancel queued messages */
  cancelQueue: () => Promise<void>;
  /** Refresh queue status from server */
  refreshQueueStatus: () => Promise<void>;
}

const QUEUE_STATUS_KEY = 'queue-status';

interface QueueInteractionState {
  isQueued: boolean;
  queuedCount: number;
  queuedMessages: AgentMessageQueueItem[];
  automationQueuedCount: number;
  hasAutomationQueuedWork: boolean;
  queuedMessage: string | null;
  queuedConfig: ExecutorConfig | null;
}

function isUserQueuedDraft(item: AgentMessageQueueItem): boolean {
  return item.source === AgentMessageSource.from_user;
}

/**
 * Separates user-authored queued drafts from automation-owned queued work.
 *
 * The chat composer may restore user drafts after Cancel Queue, but workflow
 * and system queue items are not drafts owned by the user. Treating them as
 * composer drafts leaks workflow prompts into the input and shows a misleading
 * Cancel Queue affordance for work the user did not queue.
 */
export function deriveSessionQueueInteractionState(
  queueStatus: QueueStatusSummary
): QueueInteractionState {
  const allQueuedMessages = queueStatus.messages ?? [];
  const legacySingleMessage = queueStatus.message;
  const userQueuedMessages = allQueuedMessages.filter(isUserQueuedDraft);
  const automationQueuedMessages = allQueuedMessages.filter(
    (message) => !isUserQueuedDraft(message)
  );

  if (legacySingleMessage && isUserQueuedDraft(legacySingleMessage)) {
    const alreadyIncluded = userQueuedMessages.some(
      (message) => message.id === legacySingleMessage.id
    );
    if (!alreadyIncluded) {
      userQueuedMessages.unshift(legacySingleMessage);
    }
  }

  const queuedMessageData = userQueuedMessages[0] ?? null;
  const queuedMessage = queuedMessageData?.data.message ?? null;
  const queuedConfig: ExecutorConfig | null = null;
  const queuedCount = userQueuedMessages.length;
  const automationQueuedCount = automationQueuedMessages.length;

  return {
    isQueued: queueStatus.status === QueueStatusKind.queued && queuedCount > 0,
    queuedCount,
    queuedMessages: userQueuedMessages,
    automationQueuedCount,
    hasAutomationQueuedWork: automationQueuedCount > 0,
    queuedMessage,
    queuedConfig,
  };
}

/**
 * Hook to manage queue interaction for session messages.
 * Uses TanStack Query for caching and mutation handling.
 */
export function useSessionQueueInteraction({
  sessionId,
}: UseSessionQueueInteractionOptions): UseSessionQueueInteractionResult {
  const queryClient = useQueryClient();

  const {
    data: queueStatus = {
      status: QueueStatusKind.empty,
      count: 0,
      messages: [],
      message: null,
    },
    refetch,
  } = useQuery<QueueStatusSummary>({
    queryKey: [QUEUE_STATUS_KEY, sessionId],
    queryFn: () => queueApi.getStatus(sessionId!),
    enabled: !!sessionId,
  });

  const {
    isQueued,
    queuedCount,
    queuedMessages,
    automationQueuedCount,
    hasAutomationQueuedWork,
    queuedMessage,
    queuedConfig,
  } = deriveSessionQueueInteractionState(queueStatus);

  const queueMutation = useMutation({
    mutationFn: ({ message }: { message: string }) =>
      queueApi.queue(sessionId!, { message }),
    onSuccess: (status) => {
      queryClient.setQueryData([QUEUE_STATUS_KEY, sessionId], status);
    },
  });

  const cancelMutation = useMutation({
    mutationFn: () => queueApi.cancel(sessionId!),
    onSuccess: (status) => {
      queryClient.setQueryData([QUEUE_STATUS_KEY, sessionId], status);
    },
  });

  const queueMessage = useCallback(
    async (message: string) => {
      if (!sessionId) return;
      await queueMutation.mutateAsync({ message });
    },
    [sessionId, queueMutation]
  );

  const cancelQueue = useCallback(async () => {
    if (!sessionId) return;
    await cancelMutation.mutateAsync();
  }, [sessionId, cancelMutation]);

  const refreshQueueStatus = useCallback(async () => {
    if (!sessionId) return;
    await refetch();
  }, [sessionId, refetch]);

  return {
    isQueued,
    queuedCount,
    queuedMessages,
    automationQueuedCount,
    hasAutomationQueuedWork,
    queuedMessage,
    queuedConfig,
    isQueueLoading: queueMutation.isPending || cancelMutation.isPending,
    queueMessage,
    cancelQueue,
    refreshQueueStatus,
  };
}
