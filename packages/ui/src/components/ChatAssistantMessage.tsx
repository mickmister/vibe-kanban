import type { ReactNode } from 'react';

export interface ChatAssistantMessageRenderProps {
  content: string;
  workspaceId?: string;
}

interface ChatAssistantMessageProps {
  content: string;
  kitchenTime?: string | null;
  workspaceId?: string;
  renderMarkdown: (props: ChatAssistantMessageRenderProps) => ReactNode;
}

export function ChatAssistantMessage({
  content,
  kitchenTime,
  workspaceId,
  renderMarkdown,
}: ChatAssistantMessageProps) {
  return (
    <div>
      {kitchenTime && (
        <div className="mb-base flex justify-end">
          <time className="text-xs text-low tabular-nums">{kitchenTime}</time>
        </div>
      )}
      {renderMarkdown({ content, workspaceId })}
    </div>
  );
}
