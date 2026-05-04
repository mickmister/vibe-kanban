import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ScratchType,
  type DraftFollowUpData,
  type ExecutorConfig,
} from 'shared/types';
import { useScratch } from '@/shared/hooks/useScratch';
import { useDebouncedCallback } from '@/shared/hooks/useDebouncedCallback';
import {
  areScratchDraftValuesEqual,
  clearStoredScratchDraft,
  readStoredScratchDraft,
  writeStoredScratchDraft,
} from '@/shared/lib/scratchDraftStore';

interface UseSessionMessageEditorOptions {
  /** Scratch ID (workspaceId for new session, sessionId for existing) */
  scratchId: string | undefined;
}

interface UseSessionMessageEditorResult {
  /** Current message value */
  localMessage: string;
  /** Set local message directly */
  setLocalMessage: (value: string) => void;
  /** Scratch data (message and variant) */
  scratchData: DraftFollowUpData | undefined;
  /** Whether scratch is loading */
  isScratchLoading: boolean;
  /** Whether the initial value has been applied from scratch */
  hasInitialValue: boolean;
  /** Save message and executor config to scratch */
  saveToScratch: (
    message: string,
    executorConfig: ExecutorConfig
  ) => Promise<void>;
  /** Delete the draft scratch */
  clearDraft: () => Promise<void>;
  /** Cancel pending debounced save */
  cancelDebouncedSave: () => void;
  /** Handle message change with debounced save */
  handleMessageChange: (value: string, executorConfig: ExecutorConfig) => void;
}

/**
 * Hook to manage message editing with draft persistence.
 * Handles local state, debounced saves to scratch, and sync on load.
 */
export function useSessionMessageEditor({
  scratchId,
}: UseSessionMessageEditorOptions): UseSessionMessageEditorResult {
  const {
    scratch,
    updateScratch,
    deleteScratch,
    isLoading: isScratchLoading,
    isConnected: isScratchConnected,
  } = useScratch(ScratchType.DRAFT_FOLLOW_UP, scratchId ?? '');

  const scratchData: DraftFollowUpData | undefined =
    scratch?.payload?.type === 'DRAFT_FOLLOW_UP'
      ? scratch.payload.data
      : undefined;

  const [localMessage, setLocalMessage] = useState('');
  const [hasInitialValue, setHasInitialValue] = useState(false);

  const saveToScratch = useCallback(
    async (message: string, executorConfig: ExecutorConfig) => {
      if (!scratchId) return;
      const payload: DraftFollowUpData = {
        message,
        executor_config: executorConfig,
      };
      try {
        await updateScratch({
          payload: {
            type: 'DRAFT_FOLLOW_UP',
            data: payload,
          },
        });
        writeStoredScratchDraft(
          ScratchType.DRAFT_FOLLOW_UP,
          scratchId,
          payload,
          false
        );
      } catch (e) {
        console.error('Failed to save follow-up draft', e);
      }
    },
    [scratchId, updateScratch]
  );

  const {
    debounced: debouncedSave,
    cancel: cancelDebouncedSave,
    flush: flushDebouncedSave,
  } = useDebouncedCallback(saveToScratch, 500);

  // Track whether initial load has happened to avoid re-syncing during typing
  const hasLoadedRef = useRef(false);

  // Reset load state and clear message when scratchId changes (e.g., switching to approval mode)
  useEffect(() => {
    hasLoadedRef.current = false;
    setHasInitialValue(false);
    const cachedDraft = scratchId
      ? readStoredScratchDraft<DraftFollowUpData>(
          ScratchType.DRAFT_FOLLOW_UP,
          scratchId
        )
      : null;
    setLocalMessage(cachedDraft?.value.message ?? '');
  }, [scratchId]);

  // Sync local message from scratch only on initial load
  useEffect(() => {
    if (isScratchLoading) return;
    if (hasLoadedRef.current) return;
    hasLoadedRef.current = true;
    const cachedDraft = scratchId
      ? readStoredScratchDraft<DraftFollowUpData>(
          ScratchType.DRAFT_FOLLOW_UP,
          scratchId
        )
      : null;
    const serverData = scratchData ?? null;

    if (
      scratchId &&
      cachedDraft &&
      serverData &&
      areScratchDraftValuesEqual(cachedDraft.value, serverData)
    ) {
      writeStoredScratchDraft(
        ScratchType.DRAFT_FOLLOW_UP,
        scratchId,
        serverData,
        false
      );
    }

    const preferredDraft =
      cachedDraft?.dirty === true
        ? cachedDraft.value
        : (cachedDraft?.value ?? serverData);

    setLocalMessage(preferredDraft?.message ?? serverData?.message ?? '');
    setHasInitialValue(true);
  }, [isScratchLoading, scratchData, scratchId]);

  useEffect(() => {
    if (!scratchId || !isScratchConnected) return;

    const cachedDraft = readStoredScratchDraft<DraftFollowUpData>(
      ScratchType.DRAFT_FOLLOW_UP,
      scratchId
    );
    if (!cachedDraft?.dirty) return;

    void saveToScratch(
      cachedDraft.value.message,
      cachedDraft.value.executor_config
    );
  }, [isScratchConnected, saveToScratch, scratchId]);

  useEffect(() => {
    return () => {
      flushDebouncedSave();
    };
  }, [flushDebouncedSave]);

  // Handle message change with debounced save
  // Pass executor profile at call-time to avoid stale closure
  const handleMessageChange = useCallback(
    (value: string, executorConfig: ExecutorConfig) => {
      setLocalMessage(value);
      if (scratchId) {
        writeStoredScratchDraft(
          ScratchType.DRAFT_FOLLOW_UP,
          scratchId,
          {
            message: value,
            executor_config: executorConfig,
          },
          true
        );
      }
      debouncedSave(value, executorConfig);
    },
    [debouncedSave, scratchId]
  );

  return {
    localMessage,
    setLocalMessage,
    scratchData,
    isScratchLoading,
    hasInitialValue,
    saveToScratch,
    clearDraft: async () => {
      cancelDebouncedSave();
      if (scratchId) {
        clearStoredScratchDraft(ScratchType.DRAFT_FOLLOW_UP, scratchId);
      }
      await deleteScratch();
    },
    cancelDebouncedSave,
    handleMessageChange,
  };
}
