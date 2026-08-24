import { useMemo, useCallback } from 'react';
import {
  useMutation,
  useMutationState,
  useQueries,
} from '@tanstack/react-query';
<<<<<<< HEAD
import { executionProcessesApi } from '@/shared/lib/api';
=======
import { executionProcessesApi, sessionsApi } from '@/shared/lib/api';
>>>>>>> 2a65548022970333e0548dc9e213dd005ccfbc21
import { useExecutionProcessesContext } from '@/shared/hooks/useExecutionProcessesContext';
import type { AttemptData } from '@/shared/lib/types';
import type { ExecutionProcess } from 'shared/types';

<<<<<<< HEAD
export function getStoppableExecutionProcesses(
  executionProcesses: ExecutionProcess[]
) {
  return executionProcesses.filter(
    (process) =>
      process.status === 'running' &&
      (process.run_reason === 'codingagent' ||
        process.run_reason === 'setupscript' ||
        process.run_reason === 'cleanupscript' ||
        process.run_reason === 'archivescript')
  );
}

=======
>>>>>>> 2a65548022970333e0548dc9e213dd005ccfbc21
export function getStopExecutionMutationKey(
  workspaceId: string | undefined,
  sessionId: string | undefined
) {
  return ['stopSessionExecution', workspaceId, sessionId] as const;
}

export function useWorkspaceExecution(workspaceId?: string) {
  const {
<<<<<<< HEAD
=======
    sessionId,
>>>>>>> 2a65548022970333e0548dc9e213dd005ccfbc21
    executionProcessesVisible: executionProcesses,
    isAttemptRunningVisible: isAttemptRunning,
    isLoading: streamLoading,
  } = useExecutionProcessesContext();
<<<<<<< HEAD
  const sessionId = executionProcesses[0]?.session_id;
=======
>>>>>>> 2a65548022970333e0548dc9e213dd005ccfbc21

  const stopMutationKey = useMemo(
    () => getStopExecutionMutationKey(workspaceId, sessionId),
    [workspaceId, sessionId]
  );

  const stopMutation = useMutation({
    mutationKey: stopMutationKey,
<<<<<<< HEAD
    mutationFn: async (processesToStop: ExecutionProcess[]) => {
      if (!workspaceId) return;
      await Promise.all(
        getStoppableExecutionProcesses(processesToStop).map((process) =>
          executionProcessesApi.stopExecutionProcess(process.id)
        )
      );
=======
    mutationFn: async () => {
      if (!workspaceId || !sessionId) return;
      await sessionsApi.stopExecution(sessionId);
>>>>>>> 2a65548022970333e0548dc9e213dd005ccfbc21
    },
  });

  const isStopping =
    useMutationState({
      filters: {
        mutationKey: stopMutationKey,
        status: 'pending',
      },
    }).length > 0;

  // Get setup script processes that need detailed info
  const setupProcesses = useMemo(() => {
    if (!executionProcesses.length) return [] as ExecutionProcess[];
    return executionProcesses.filter((p) => p.run_reason === 'setupscript');
  }, [executionProcesses]);

  // Fetch details for setup processes
  const processDetailQueries = useQueries({
    queries: setupProcesses.map((process) => ({
      queryKey: ['processDetails', process.id],
      queryFn: () => executionProcessesApi.getDetails(process.id),
      enabled: !!process.id,
    })),
  });

  // Build attempt data combining processes and details
  const attemptData: AttemptData = useMemo(() => {
    if (!executionProcesses.length) {
      return { processes: [], runningProcessDetails: {} };
    }

    const runningProcessDetails: Record<string, ExecutionProcess> = {};

    setupProcesses.forEach((process, index) => {
      const detailQuery = processDetailQueries[index];
      if (detailQuery?.data) {
        runningProcessDetails[process.id] = detailQuery.data;
      }
    });

    return {
      processes: executionProcesses,
      runningProcessDetails,
    };
  }, [executionProcesses, setupProcesses, processDetailQueries]);

  const stopExecution = useCallback(async () => {
    if (!workspaceId || isStopping) return;

    try {
      await stopMutation.mutateAsync(executionProcesses);
    } catch (error) {
      console.error('Failed to stop executions:', error);
      throw error;
    }
  }, [workspaceId, isStopping, stopMutation, executionProcesses]);

  const isLoading =
    streamLoading || processDetailQueries.some((q) => q.isLoading);
  const isFetching =
    streamLoading || processDetailQueries.some((q) => q.isFetching);

  return {
    processes: executionProcesses,
    attemptData,
    runningProcessDetails: attemptData.runningProcessDetails,
    isAttemptRunning,
    isLoading,
    isFetching,
    stopExecution,
    isStopping,
  };
}
