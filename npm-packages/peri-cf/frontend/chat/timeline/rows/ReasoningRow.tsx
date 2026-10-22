import { Sparkles } from 'lucide-react';
import type { TimelineWorkItem } from '../types';

/** 单条 reasoning：外层 work 组负责折叠，这里只承担内容排版。 */
export function ReasoningRow({ item }: { item: TimelineWorkItem }) {
  return (
    <div className="flex flex-col gap-1 py-1" data-slot="peri-reasoning-row">
      <div className="text-muted-foreground flex items-center gap-1.5 text-xs">
        <Sparkles className="size-3.5 shrink-0" aria-hidden="true" />
        <span>思考</span>
        {item.status === 'pending' || item.status === 'streaming' ? (
          <span className="text-muted-foreground/70">· 进行中</span>
        ) : null}
      </div>
      {item.text && (
        <p className="text-muted-foreground border-border/70 border-l-2 pl-2.5 text-[13px] leading-5 whitespace-pre-wrap">
          {item.text}
        </p>
      )}
    </div>
  );
}
