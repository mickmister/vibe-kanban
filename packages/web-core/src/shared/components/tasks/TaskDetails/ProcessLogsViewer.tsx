import { VirtualizedProcessLogs } from '@/shared/components/VirtualizedProcessLogs';
import { useLogStream } from '@/shared/hooks/useLogStream';
import type { PatchType } from 'shared/types';

type LogEntry = Extract<PatchType, { type: 'STDOUT' } | { type: 'STDERR' }>;

interface ProcessLogsViewerProps {
  processId: string;
}

export function ProcessLogsViewerContent({
  logs,
  error,
}: {
  logs: LogEntry[];
  error: string | null;
}) {
  return (
    <div className="flex-1 min-h-0">
      <VirtualizedProcessLogs
        logs={logs}
        error={error}
        searchQuery=""
        matchIndices={[]}
        currentMatchIndex={-1}
      />
    </div>
  );
}

export default function ProcessLogsViewer({
  processId,
}: ProcessLogsViewerProps) {
  const { logs, error } = useLogStream(processId);
  return <ProcessLogsViewerContent logs={logs} error={error} />;
}
