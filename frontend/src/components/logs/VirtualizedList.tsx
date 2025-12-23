import { useEffect, useMemo, useRef, useState } from 'react';
import { Virtuoso, VirtuosoHandle } from 'react-virtuoso';

import DisplayConversationEntry from '../NormalizedConversation/DisplayConversationEntry';
import { useEntries } from '@/contexts/EntriesContext';
import {
  AddEntryType,
  PatchTypeWithKey,
  useConversationHistory,
} from '@/hooks/useConversationHistory';
import { Loader2 } from 'lucide-react';
import { TaskWithAttemptStatus } from 'shared/types';
import type { WorkspaceWithSession } from '@/types/attempt';
import { ApprovalFormProvider } from '@/contexts/ApprovalFormContext';

interface VirtualizedListProps {
  attempt: WorkspaceWithSession;
  task?: TaskWithAttemptStatus;
}

const VirtualizedList = ({ attempt, task }: VirtualizedListProps) => {
  const [entries, setLocalEntries] = useState<PatchTypeWithKey[]>([]);
  const [loading, setLoading] = useState(true);
  const { setEntries, reset } = useEntries();
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const didInitScroll = useRef(false);
  const [atBottom, setAtBottom] = useState(true);
  const prevLenRef = useRef(0);

  useEffect(() => {
    setLoading(true);
    setLocalEntries([]);
    reset();
    didInitScroll.current = false;
  }, [attempt.id, reset]);

  const onEntriesUpdated = (
    newEntries: PatchTypeWithKey[],
    _addType: AddEntryType,
    newLoading: boolean
  ) => {
    setLocalEntries(newEntries);
    setEntries(newEntries);

    if (loading) {
      setLoading(newLoading);
    }
  };

  useConversationHistory({ attempt, onEntriesUpdated });

  const messageListContext = useMemo(
    () => ({ attempt, task }),
    [attempt, task]
  );

  // Initial scroll to bottom when data first loads
  useEffect(() => {
    if (!didInitScroll.current && entries.length > 0 && !loading) {
      didInitScroll.current = true;
      requestAnimationFrame(() => {
        virtuosoRef.current?.scrollToIndex({
          index: entries.length - 1,
          align: 'end',
        });
      });
    }
  }, [entries.length, loading]);

  // Auto-scroll to bottom on large bursts of new entries
  useEffect(() => {
    const prev = prevLenRef.current;
    const grewBy = entries.length - prev;
    prevLenRef.current = entries.length;

    const LARGE_BURST = 5;
    if (grewBy >= LARGE_BURST && atBottom && entries.length > 0) {
      requestAnimationFrame(() => {
        virtuosoRef.current?.scrollToIndex({
          index: entries.length - 1,
          align: 'end',
        });
      });
    }
  }, [entries.length, atBottom]);

  const itemContent = (_index: number, data: PatchTypeWithKey) => {
    if (data.type === 'STDOUT') {
      return <p>{data.content}</p>;
    }
    if (data.type === 'STDERR') {
      return <p>{data.content}</p>;
    }
    if (data.type === 'NORMALIZED_ENTRY') {
      return (
        <DisplayConversationEntry
          expansionKey={data.patchKey}
          entry={data.content}
          executionProcessId={data.executionProcessId}
          taskAttempt={messageListContext.attempt}
          task={messageListContext.task}
        />
      );
    }

    return null;
  };

  return (
    <ApprovalFormProvider>
      <div className="flex-1 relative">
        <Virtuoso<PatchTypeWithKey>
          ref={virtuosoRef}
          className="flex-1"
          data={entries}
          itemContent={itemContent}
          computeItemKey={(_index, data) => `l-${data.patchKey}`}
          atBottomStateChange={setAtBottom}
          followOutput={atBottom ? 'smooth' : false}
          increaseViewportBy={{ top: 0, bottom: 600 }}
          components={{
            Header: () => <div className="h-2"></div>,
            Footer: () => <div className="h-2"></div>,
          }}
        />
        {loading && (
          <div className="absolute top-0 left-0 w-full h-full bg-primary flex flex-col gap-2 justify-center items-center">
            <Loader2 className="h-8 w-8 animate-spin" />
            <p>Loading History</p>
          </div>
        )}
      </div>
    </ApprovalFormProvider>
  );
};

export default VirtualizedList;
