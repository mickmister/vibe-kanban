import type { ExecutionProcess } from 'shared/types';
import { ExecutionProcessStatus } from 'shared/types';

import type { ExecutionProcessStateStore } from '@/shared/hooks/useConversationHistory/types';

export function shouldAutoReplayRemainingHistoryAfterInitialLoad(): boolean {
  return false;
}

export function hasUnloadedCompletedHistoryProcesses(
  processes: readonly Pick<ExecutionProcess, 'id' | 'status'>[],
  displayedProcesses: ExecutionProcessStateStore
): boolean {
  return processes.some(
    (process) =>
      process.status !== ExecutionProcessStatus.running &&
      displayedProcesses[process.id] == null
  );
}

interface LoadExplicitEarlierHistoryBatchOptions {
  generation: number;
  batchSize: number;
  inFlightRef: { current: boolean };
  isCurrentGeneration: (generation: number) => boolean;
  hasUnloadedHistory: () => boolean;
  updateHasMoreHistoryForGeneration: (generation: number) => void;
  setLoadingHistoryForGeneration: (
    generation: number,
    isLoading: boolean
  ) => void;
  loadRemainingEntriesInBatches: (batchSize: number) => Promise<boolean>;
  emitHistoricEntriesForGeneration: (generation: number) => boolean;
}

export type LoadExplicitEarlierHistoryBatchResult =
  | 'loaded'
  | 'empty'
  | 'deduped'
  | 'stale';

export async function loadExplicitEarlierHistoryBatch({
  generation,
  batchSize,
  inFlightRef,
  isCurrentGeneration,
  hasUnloadedHistory,
  updateHasMoreHistoryForGeneration,
  setLoadingHistoryForGeneration,
  loadRemainingEntriesInBatches,
  emitHistoricEntriesForGeneration,
}: LoadExplicitEarlierHistoryBatchOptions): Promise<LoadExplicitEarlierHistoryBatchResult> {
  if (!isCurrentGeneration(generation)) return 'stale';
  if (inFlightRef.current) return 'deduped';

  if (!hasUnloadedHistory()) {
    updateHasMoreHistoryForGeneration(generation);
    return 'empty';
  }

  inFlightRef.current = true;
  setLoadingHistoryForGeneration(generation, true);

  try {
    const anyUpdated = await loadRemainingEntriesInBatches(batchSize);
    if (!isCurrentGeneration(generation)) return 'stale';

    if (anyUpdated) {
      emitHistoricEntriesForGeneration(generation);
    }
    updateHasMoreHistoryForGeneration(generation);

    return anyUpdated ? 'loaded' : 'empty';
  } finally {
    inFlightRef.current = false;
    setLoadingHistoryForGeneration(generation, false);
  }
}
