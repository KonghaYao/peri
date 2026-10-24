import { Check, Copy } from 'lucide-react';
import { MarkdownText } from '../MarkdownText';
import { useCopyToClipboard } from '../../../hooks';

export interface AssistantMessageRowProps {
  text: string;
  streaming: boolean;
}

/** assistant 消息：无气泡正文；流式期间不显示复制完成态。 */
export function AssistantMessageRow({ text, streaming }: AssistantMessageRowProps) {
  const { isCopied, copyToClipboard } = useCopyToClipboard();
  const body = text || (streaming ? '' : '（空响应）');
  return (
    <article className="group/assistant relative min-w-0 px-0.5 py-0.5" data-slot="peri-assistant-message">
      <div className="text-muted-foreground mb-1 text-[11px] font-medium">Peri</div>
      {body && <MarkdownText text={body} />}
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
