import { useEffect, useRef, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib';
import { ReasoningRow } from './ReasoningRow';
import { ToolRow } from './ToolRow';
import type { ToolPayloadReader } from '../../runtime';
import type { TimelineWorkItem } from '../types';
import { countWork, workSummaryLabel } from '../workSummary';

const RUNNING_STATUSES = new Set(['pending', 'streaming', 'running']);

/**
 * 连续 reasoning/tool 的合并行：运行中默认展开并跟随当前活动；历史默认折叠。
 * 失败/取消也保持收起，失败信号由摘要计数与红色样式提示。
 */
export function WorkGroupRow({
  items,
  active,
  failed,
  defaultExpanded = active,
  payloadReader,
  bare = false,
}: {
  items: readonly TimelineWorkItem[];
  active: boolean;
  failed: boolean;
  /** 首帧展开事实：由 deriveTimeline 按运行状态计算。 */
  defaultExpanded?: boolean;
  /** 结果全文按需读取；缺席时只显示 inline/预览。 */
  payloadReader?: ToolPayloadReader;
  /** 外层 turn fold 已展开：直接展示条目，不再渲染组头与折叠按钮。 */
  bare?: boolean;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const userToggled = useRef(false);

  // 仅在没有用户手动操作时跟随运行态展开；折叠后不被普通流式更新强制打开。
  useEffect(() => {
    if (active && !userToggled.current) setExpanded(true);
  }, [active]);

  const toggle = () => {
    userToggled.current = true;
    setExpanded((value) => !value);
  };
  const current = [...items].reverse().find((item) => RUNNING_STATUSES.has(item.status)) ?? items.at(-1);
  const failedCount = items.filter((item) => item.failed).length;
  const counts = countWork(items);
  const open = bare || expanded;

  return (
    <div
      className={bare ? 'pb-1' : 'py-1'}
      data-slot="peri-work-group"
      data-active={active ? 'true' : 'false'}
      data-work-failed={failed ? 'true' : 'false'}
      data-bare={bare ? 'true' : 'false'}
    >
      {!bare && (
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground flex w-full min-w-0 items-center gap-1.5 rounded-md px-1 py-0.5 text-left text-[13px] transition-colors"
          aria-expanded={expanded}
          onClick={toggle}
        >
          <ChevronRight
            className={cn('size-3.5 shrink-0 transition-transform', expanded && 'rotate-90')}
            aria-hidden="true"
          />
          <span className={cn('truncate', failed && 'text-destructive/80', active && 'shimmer motion-reduce:animate-none')}>
            {workSummaryLabel(counts)}
          </span>
          {failed && (
            <span className="text-destructive shrink-0 text-[11px]" data-slot="peri-work-failed-count">
              失败 {failedCount}
            </span>
          )}
          {active && <span className="text-muted-foreground/70 ml-auto shrink-0 text-[11px]">进行中</span>}
        </button>
      )}
      {!bare && !expanded && active && current && (
        <div
          className="text-muted-foreground ml-5 flex min-w-0 items-center gap-1.5 py-0.5 text-xs"
          data-slot="peri-work-current"
        >
          <span className="bg-foreground/50 size-1.5 shrink-0 animate-pulse rounded-full motion-reduce:animate-none" aria-hidden="true" />
          <span className="shrink-0">{current.label}</span>
          {current.target && <span className="truncate font-mono text-[11px]">{current.target}</span>}
        </div>
      )}
      {open && (
        <div className={cn('flex flex-col', bare ? 'pt-1' : 'border-border/70 ml-2 border-l pl-2')}>
          {items.map((item) => item.kind === 'reasoning'
            ? <ReasoningRow key={item.id} item={item} rail={!bare} />
            : <ToolRow key={item.id} item={item} payloadReader={payloadReader} />)}
        </div>
      )}
    </div>
  );
}
