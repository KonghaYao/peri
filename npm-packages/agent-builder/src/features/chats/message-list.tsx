import type { ChatMessage } from '@shared/chat';
import { useEffect, useRef } from 'react';
import { EmptyState } from '@/components/empty-state';

export interface MessageListProps {
  readonly messages: readonly ChatMessage[];
  readonly running: boolean;
}

function roleLabel(role: ChatMessage['role']): string {
  return role === 'user' ? '你' : 'Agent';
}

export function MessageList({ messages, running }: MessageListProps) {
  const endRef = useRef<HTMLDivElement>(null);

  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll again whenever messages or the running flag change
  useEffect(() => {
    const node = endRef.current;
    if (node !== null && typeof node.scrollIntoView === 'function') {
      node.scrollIntoView({ block: 'end' });
    }
  }, [messages, running]);

  if (messages.length === 0) {
    return <EmptyState title="还没有消息" description="发送第一条消息后，Agent 的回复会在这里流式出现。" />;
  }

  return (
    <div className="space-y-3">
      {messages.map((message) => (
        <article
          key={message.id}
          data-testid="chat-message"
          className={
            message.role === 'user'
              ? 'ml-auto max-w-[85%] rounded-xl bg-primary px-4 py-3 text-primary-foreground'
              : 'mr-auto max-w-[85%] rounded-xl border bg-card px-4 py-3'
          }
        >
          <p className="mb-1 text-xs opacity-70">
            {roleLabel(message.role)}
            {/* The SDK replica marks terminal messages `completed`; `complete` never appears. */}
            {message.status !== 'completed' ? ` · ${message.status}` : ''}
          </p>
          <p className="whitespace-pre-wrap break-words text-sm">{message.text}</p>
        </article>
      ))}
      {running ? (
        <p className="text-xs text-muted-foreground" role="status">
          Agent 正在回复…
        </p>
      ) : null}
      <div ref={endRef} />
    </div>
  );
}
