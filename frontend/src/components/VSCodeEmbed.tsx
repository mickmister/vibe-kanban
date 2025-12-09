import { useEffect, useState } from 'react';
import { useTaskAttempt } from '@/hooks/useTaskAttempt';

interface VSCodeEmbedProps {
  attemptId: string | null;
}

export function VSCodeEmbed({ attemptId }: VSCodeEmbedProps) {
  const [iframeUrl, setIframeUrl] = useState<string>('');
  const { data: attempt } = useTaskAttempt(attemptId || undefined);

  useEffect(() => {
    if (!attemptId || !attempt) {
      setIframeUrl('');
      return;
    }

    // Use the worktree path from container_ref to open the code-server
    // The code-server is served via Caddy on port 3001
    const worktreePath = attempt.container_ref;
    if (worktreePath) {
      // Code-server URL with the folder parameter
      const codeServerUrl = `http://localhost:3001/?folder=${encodeURIComponent(worktreePath)}`;
      setIframeUrl(codeServerUrl);
    } else {
      setIframeUrl('');
    }
  }, [attemptId, attempt]);

  if (!iframeUrl) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        No editor available
      </div>
    );
  }

  return (
    <div className="h-full w-full flex flex-col bg-[#1e1e1e]">
      {/* VSCode-like title bar */}
      <div className="flex items-center h-9 bg-[#323233] border-b border-[#2d2d30] px-2 gap-2 flex-shrink-0">
        <div className="flex gap-1.5">
          <div className="w-3 h-3 rounded-full bg-[#ff5f56]" />
          <div className="w-3 h-3 rounded-full bg-[#ffbd2e]" />
          <div className="w-3 h-3 rounded-full bg-[#27c93f]" />
        </div>
        <div className="flex-1 text-center text-xs text-[#cccccc] font-medium">
          Code Editor
        </div>
      </div>

      {/* VSCode iframe */}
      <div className="flex-1 relative">
        <iframe
          src={iframeUrl}
          className="absolute inset-0 w-full h-full border-0"
          title="VSCode Editor"
          sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals"
          allow="clipboard-read; clipboard-write; fullscreen"
        />
      </div>
    </div>
  );
}
