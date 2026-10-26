import { useEffect, useRef, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib';
import { ReasoningRow } from './ReasoningRow';
import { ToolRow } from './ToolRow';
import type { ToolPayloadReader } from '../../runtime';
import type { TimelineWorkItem } from '../types';

const RUNNING_STATUSES = new Set(['pending', 'streaming', 'running']);

function summaryLabel(items: readonly TimelineWorkItem[]): string {
  const reasoning = items.filter((item) => item.kind === 'reasoning').length;
  const tools = items.length - reasoning;
  if (tools === 0) return `思考过程 · ${reasoning} 段`;
  if (reasoning === 0) return `工具调用 · ${tools} 个`;
  return `思考与工具 · ${tools} 个工具 / ${reasoning} 段思考`;
}

/**
 * 连续 reasoning/tool 的合并行：运行中默认展开并跟随当前活动；历史默认折叠。
 * 失败工具、失败/取消 turn 默认展开；状态转失败时即使曾手动折叠也展开一次。
 */
export function WorkGroupRow({
  items,
  active,
  failed,
  defaultExpanded = active,
  forceExpanded = false,
  payloadReader,
}: {
  items: readonly TimelineWorkItem[];
  active: boolean;
  failed: boolean;
  /** 首帧展开事实：由 deriveTimeline 按运行/失败/取消状态计算。 */
  defaultExpanded?: boolean;
  /** 失败终态：从 false 变为 true 时覆盖用户此前的折叠。 */
  forceExpanded?: boolean;
  /** 结果全文按需读取；缺席时只显示 inline/预览。 */
  payloadReader?: ToolPayloadReader;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded || failed);
  const userToggled = useRef(false);
  const previousForce = useRef(forceExpanded || failed);
  const previousFailures = useRef(items.filter((item) => item.failed).length);

  // 仅在没有用户手动操作时跟随运行态展开；折叠后不被普通流式更新强制打开。
  useEffect(() => {
    if (active && !userToggled.current) setExpanded(true);
  }, [active]);

  // 失败终态或新的失败工具出现时必须可见：展开一次，用户随后仍可手动折叠。
  useEffect(() => {
    const failures = items.filter((item) => item.failed).length;
    const terminal = forceExpanded || failed;
    if ((terminal && !previousForce.current) || failures > previousFailures.current) {
      setExpanded(true);
    }
    previousForce.current = terminal;
    previousFailures.current = failures;
  }, [failed, forceExpanded, items]);

  const toggle = () => {
    userToggled.current = true;
    setExpanded((value) => !value);
  };
  const current = [...items].reverse().find((item) => RUNNING_STATUSES.has(item.status)) ?? items.at(-1);
  const failedCount = items.filter((item) => item.failed).length;

  return (
    <div
      className="border-border/70 border-b py-1"
      data-slot="peri-work-group"
      data-active={active ? 'true' : 'false'}
      data-work-failed={failed ? 'true' : 'false'}
    >
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
          {summaryLabel(items)}
        </span>
        {failed && (
          <span className="text-destructive shrink-0 text-[11px]" data-slot="peri-work-failed-count">
            失败 {failedCount}
          </span>
        )}
        {active && <span className="text-muted-foreground/70 ml-auto shrink-0 text-[11px]">进行中</span>}
      </button>
      {!expanded && active && current && (
        <div
          className="text-muted-foreground ml-5 flex min-w-0 items-center gap-1.5 py-0.5 text-xs"
          data-slot="peri-work-current"
        >
          <span className="bg-foreground/50 size-1.5 shrink-0 animate-pulse rounded-full motion-reduce:animate-none" aria-hidden="true" />
          <span className="shrink-0">{current.label}</span>
          {current.target && <span className="truncate font-mono text-[11px]">{current.target}</span>}
        </div>
      )}
      {expanded && (
        <div className="border-border/70 ml-2 flex flex-col border-l pl-2">
          {items.map((item) => item.kind === 'reasoning'
            ? <ReasoningRow key={item.id} item={item} />
            : <ToolRow key={item.id} item={item} payloadReader={payloadReader} />)}
        </div>
      )}
    </div>
  );
}
