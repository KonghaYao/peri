import { useEffect, useRef, useState } from 'react';
import { ChevronRight, Sparkles } from 'lucide-react';
import { cn } from '@/lib';
import type { TimelineWorkItem } from '../types';

/** 单条 reasoning：运行中展开，结束后自动收起；turn 折叠内不画左侧竖线。 */
export function ReasoningRow({ item, rail = true }: { item: TimelineWorkItem; rail?: boolean }) {
  const streaming = item.status === 'pending' || item.status === 'streaming';
  const [expanded, setExpanded] = useState(streaming);
  const userToggled = useRef(false);

  useEffect(() => {
    if (!userToggled.current) setExpanded(streaming);
  }, [streaming]);

  const text = item.text ?? '';
  const hasText = text.trim().length > 0;
  const preview = text.replace(/\s+/g, ' ').trim();

  return (
    <div className="flex flex-col gap-1 py-1" data-slot="peri-reasoning-row">
      <button
        type="button"
        className="text-muted-foreground flex w-full min-w-0 items-center gap-1.5 text-left text-xs"
        aria-expanded={expanded}
        onClick={() => {
          userToggled.current = true;
          setExpanded((value) => !value);
        }}
      >
        <Sparkles className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="shrink-0">思考</span>
        {streaming ? <span className="text-muted-foreground/70 shrink-0">· 进行中</span> : null}
        {!expanded && hasText ? (
          <span className="text-muted-foreground/45 min-w-0 flex-1 truncate">{preview}</span>
        ) : null}
        <ChevronRight
          className={cn('ml-auto size-3.5 shrink-0 transition-transform', expanded && 'rotate-90')}
          aria-hidden="true"
        />
      </button>
      {expanded && hasText ? (
        <p
          className={cn(
            'text-muted-foreground text-[13px] leading-5 whitespace-pre-wrap break-words',
            rail && 'border-border/70 border-l-2 pl-2.5',
          )}
        >
          {item.text}
        </p>
      ) : null}
    </div>
  );
}
