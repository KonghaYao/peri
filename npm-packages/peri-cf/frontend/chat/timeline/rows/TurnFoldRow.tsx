import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib';

/**
 * turn 级折叠行：已结算 turn 的 work 收束为一条摘要；失败/取消只保留终态配色，
 * 折叠状态仍由用户控制，错误摘要始终在标签上可见。
 */
export function TurnFoldRow({
  label,
  summary,
  expanded,
  failed = false,
  onToggle,
}: {
  label: string;
  summary?: string;
  expanded: boolean;
  failed?: boolean;
  onToggle?: () => void;
}) {
  const labelClass = cn(
    'flex w-full min-w-0 items-center gap-1.5 px-1 py-0.5 text-left text-[13px]',
    failed ? 'text-destructive/80' : 'text-muted-foreground',
  );

  return (
    <div className="border-border/70 border-b py-1" data-slot="peri-turn-fold">
      <button
        type="button"
        className={cn(
          labelClass,
          'rounded-md transition-colors',
          failed ? 'hover:text-destructive' : 'hover:text-foreground',
        )}
        aria-expanded={expanded}
        onClick={onToggle}
        disabled={!onToggle}
      >
        <ChevronRight
          className={cn('size-3.5 shrink-0 transition-transform', expanded && 'rotate-90')}
          aria-hidden="true"
        />
        <span className="truncate">{label}</span>
        {summary && (
          <span className="text-muted-foreground/60 shrink-0 text-[11px]">· {summary}</span>
        )}
      </button>
    </div>
  );
}
