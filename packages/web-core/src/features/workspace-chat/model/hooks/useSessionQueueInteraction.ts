import { useCallback } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { queueApi } from '@/shared/lib/api';
import type { AgentMessageQueueItem, ExecutorConfig, QueueStatusSummary } from 'shared/types';

interface UseSessionQueueInteractionOptions {
  /** Session ID for queue operations */
  sessionId: string | undefined;
}

interface UseSessionQueueInteractionResult {
  /** Whether one or more messages are currently queued */
  isQueued: boolean;
  /** Number of pending queued/running queue messages */
  queuedCount: number;
  /** Pending queued messages */
  queuedMessages: AgentMessageQueueItem[];
  /** First queued message content, if any */
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

/**
 * Hook to manage queue interaction for session messages.
 * Uses TanStack Query for caching and mutation handling.
 */
export function useSessionQueueInteraction({
  sessionId,
}: UseSessionQueueInteractionOptions): UseSessionQueueInteractionResult {
  const queryClient = useQueryClient();

  const { data: queueStatus = { status: 'empty' as const, count: 0, messages: [] }, refetch } =
    useQuery<QueueStatusSummary>({
      queryKey: [QUEUE_STATUS_KEY, sessionId],
      queryFn: () => queueApi.getStatus(sessionId!),
      enabled: !!sessionId,
    });

  const queuedMessages = 'messages' in queueStatus ? queueStatus.messages : [];
  const queuedCount = 'count' in queueStatus ? queueStatus.count : queuedMessages.length;
  const isQueued = queueStatus.status === 'queued' && queuedCount > 0;
  const queuedMessageData = queuedMessages[0] ??
    (queueStatus.status === 'queued' && 'message' in queueStatus ? queueStatus.message : null);
  const queuedMessage = queuedMessageData?.data.message ?? null;
  const queuedConfig: ExecutorConfig | null = null;

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
    queuedMessage,
    queuedConfig,
    isQueueLoading: queueMutation.isPending || cancelMutation.isPending,
    queueMessage,
    cancelQueue,
    refreshQueueStatus,
  };
}
