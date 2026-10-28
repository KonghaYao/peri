import { Check, Copy } from 'lucide-react';
import { StreamdownText } from '../StreamdownText';
import { OpenUIMessage, type OpenUIActionEvent } from '../../openui';
import { useCopyToClipboard } from '../../../hooks';

interface AssistantMessageRowProps {
  text: string;
  streaming: boolean;
  onOpenUIAction?: (event: OpenUIActionEvent) => void;
  actionsEnabled?: boolean;
}

const renderMarkdownFor = (streaming: boolean) => (value: string) =>
  <StreamdownText text={value} streaming={streaming} />;

/** assistant 消息：无气泡正文；流式期间不显示复制完成态。复制始终复制原始文本（含 DSL）。 */
export function AssistantMessageRow({
  text, streaming, onOpenUIAction, actionsEnabled,
}: AssistantMessageRowProps) {
  const { isCopied, copyToClipboard } = useCopyToClipboard();
  const body = text || (streaming ? '' : '（空响应）');
  return (
    <article className="group/assistant relative min-w-0 px-0.5 py-0.5" data-slot="peri-assistant-message">
      {body && (
        <OpenUIMessage
          text={body}
          streaming={streaming}
          renderMarkdown={renderMarkdownFor(streaming)}
          onOpenUIAction={onOpenUIAction}
          actionsEnabled={actionsEnabled}
        />
      )}
      {text && !streaming && (
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground mt-1 inline-flex size-6 items-center justify-center rounded-md opacity-0 transition-opacity group-hover/assistant:opacity-100 focus-visible:opacity-100"
          aria-label="复制回答"
          onClick={() => copyToClipboard(text)}
        >
          {isCopied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
        </button>
      )}
    </article>
  );
}
