import { useCallback, useState, useEffect, useRef } from 'react';
import { ScratchType, type WorkspaceNotesData } from 'shared/types';
import { useScratch } from '@/shared/hooks/useScratch';
import { useDebouncedCallback } from '@/shared/hooks/useDebouncedCallback';

export interface UseWorkspaceNotesResult {
  content: string;
  isLoading: boolean;
  isConnected: boolean;
  error: string | null;
  setContent: (content: string) => void;
}

/**
 * Hook for managing workspace notes stored in scratch memory.
 * Provides debounced saves and local state for immediate UI feedback.
 */
export function useWorkspaceNotes(
  workspaceId: string | undefined
): UseWorkspaceNotesResult {
  const {
    scratch,
    updateScratch,
    isLoading: isScratchLoading,
    isConnected,
    error,
  } = useScratch(ScratchType.WORKSPACE_NOTES, workspaceId ?? '', {
    enabled: !!workspaceId,
  });

  // Local state for immediate UI feedback
  const [localContent, setLocalContent] = useState('');
  const localContentRef = useRef('');

  // Ignore websocket echoes while we still have newer local edits that have not
  // been observed coming back from the server.
  const hasPendingLocalChangesRef = useRef(false);

  // Extract content from scratch payload
  const scratchData: WorkspaceNotesData | undefined =
    scratch?.payload?.type === 'WORKSPACE_NOTES'
      ? scratch.payload.data
      : undefined;

  useEffect(() => {
    localContentRef.current = '';
    hasPendingLocalChangesRef.current = false;
    setLocalContent('');
  }, [workspaceId]);

  // Sync from server when scratch loads, but never let an older websocket echo
  // overwrite newer local text while the editor is dirty.
  useEffect(() => {
    if (isScratchLoading) return;

    const serverContent = scratchData?.content ?? '';
    if (serverContent === localContentRef.current) {
      hasPendingLocalChangesRef.current = false;
      return;
    }

    if (hasPendingLocalChangesRef.current) return;

    localContentRef.current = serverContent;
    setLocalContent(serverContent);
  }, [isScratchLoading, scratchData?.content]);

  // Debounced save to server
  const { debounced: saveContent } = useDebouncedCallback(
    useCallback(
      async (content: string) => {
        if (!workspaceId) return;
        try {
          await updateScratch({
            payload: {
              type: 'WORKSPACE_NOTES',
              data: { content },
            },
          });
        } catch (e) {
          console.error('Failed to save workspace notes', e);
        }
      },
      [workspaceId, updateScratch]
    ),
    500
  );

  const setContent = useCallback(
    (content: string) => {
      hasPendingLocalChangesRef.current = true;
      localContentRef.current = content;
      setLocalContent(content);
      saveContent(content);
    },
    [saveContent]
  );

  return {
    content: localContent,
    isLoading: isScratchLoading,
    isConnected,
    error,
    setContent,
  };
}
