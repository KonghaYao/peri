import { useEffect, useId, useRef, useState } from 'react';
import type { ContextUsage } from './composerUsage';

interface ContextUsageMeterProps {
  usage: ContextUsage;
}

function tokenLabel(value: number): string {
  return `${Math.round(value)} tokens`;
}

/**
 * 上下文环形用量：只读事实，没有 Compact 入口。
 * 无 max 时只展示预算百分比，不推算窗口大小。
 */
export function ContextUsageMeter({ usage }: ContextUsageMeterProps) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const detailsId = useId();
  const percent = Math.round(usage.percent);
  const danger = usage.percent > 90;
  const description = usage.max === null
    ? `上下文用量 ${percent}%，已用 ${tokenLabel(usage.used)}`
    : `上下文用量 ${percent}%，已用 ${tokenLabel(usage.used)} / ${tokenLabel(usage.max)}`;

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      setOpen(false);
      trigger.current?.focus();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);

  return (
    <span className="relative inline-flex min-w-0" data-slot="peri-context-usage">
      <button
        ref={trigger}
        type="button"
        className={`inline-flex min-w-0 items-center gap-1 rounded px-1 py-0.5 transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none ${
          danger ? 'text-destructive' : 'text-muted-foreground'
        }`}
        aria-label={description}
        aria-expanded={open}
        aria-controls={open ? detailsId : undefined}
        title={description}
        onClick={() => setOpen((value) => !value)}
      >
        <svg viewBox="0 0 20 20" className="size-4 shrink-0" aria-hidden="true">
          <circle cx="10" cy="10" r="7" fill="none" stroke="currentColor" strokeOpacity="0.2" strokeWidth="3" />
          <circle
            cx="10"
            cy="10"
            r="7"
            fill="none"
            stroke="currentColor"
            strokeWidth="3"
            strokeLinecap="round"
            pathLength={100}
            strokeDasharray={`${usage.percent} 100`}
            transform="rotate(-90 10 10)"
          />
        </svg>
        <span className="hidden truncate sm:inline">{percent}%</span>
      </button>
      {open && (
        <div
          id={detailsId}
          role="dialog"
          aria-label="上下文用量详情"
          className="bg-popover text-popover-foreground absolute bottom-full left-0 z-40 mb-2 w-56 rounded-lg border p-3 text-xs shadow-lg"
        >
          <div className="mb-2 font-medium">上下文用量</div>
          <dl className="space-y-1">
            <div className="flex justify-between gap-3">
              <dt>已用</dt>
              <dd>{tokenLabel(usage.used)}</dd>
            </div>
            {usage.max !== null && (
              <div className="flex justify-between gap-3">
                <dt>上限</dt>
                <dd>{tokenLabel(usage.max)}</dd>
              </div>
            )}
            <div className="flex justify-between gap-3">
              <dt>百分比</dt>
              <dd>{percent}%</dd>
            </div>
            {usage.totalProcessed !== undefined && (
              <div className="flex justify-between gap-3">
                <dt>累计处理</dt>
                <dd>{tokenLabel(usage.totalProcessed)}</dd>
              </div>
            )}
          </dl>
          {usage.max === null && (
            <p className="text-muted-foreground mt-2">仅能读取预算百分比，无法确定窗口上限。</p>
          )}
        </div>
      )}
    </span>
  );
}
