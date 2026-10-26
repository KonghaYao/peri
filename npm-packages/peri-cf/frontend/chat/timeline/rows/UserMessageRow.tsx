import { Check, Copy } from 'lucide-react';
import type { Entry } from '../../runtime';
import { useCopyToClipboard } from '../../../hooks';

function userText(entry: Entry): string {
  return entry.blocks
    .filter((block): block is Extract<Entry['blocks'][number], { type: 'text' }> => block.type === 'text')
    .map((block) => block.text)
    .join('\n');
}

/** 用户消息：右对齐气泡；复制只在有正文时出现。 */
export function UserMessageRow({ entry }: { entry: Entry }) {
  const text = userText(entry);
  const { isCopied, copyToClipboard } = useCopyToClipboard();
  return (
    <div className="group flex flex-col items-end gap-1" data-slot="peri-user-message">
      <div className="text-muted-foreground me-1 text-[11px]">你</div>
      <div className="bg-[var(--chat-user-bubble)] text-foreground relative max-w-[80%] rounded-2xl px-3.5 py-2.5 text-sm leading-6 whitespace-pre-wrap [overflow-wrap:anywhere]">
        {text || '（空消息）'}
        {text && (
          <button
            type="button"
            className="text-muted-foreground hover:text-foreground absolute -bottom-2 -left-7 inline-flex size-6 items-center justify-center rounded-md opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
            aria-label="复制消息"
            onClick={() => copyToClipboard(text)}
          >
            {isCopied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
          </button>
        )}
      </div>
    </div>
  );
}
