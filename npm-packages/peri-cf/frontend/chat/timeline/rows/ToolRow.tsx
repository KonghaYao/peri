import { useState } from 'react';
import {
  Check, ChevronRight, CircleAlert, FilePenLine, FileText, Globe, ListChecks, Loader2,
  MessageCircleQuestion, Search, Terminal, Wrench, type LucideIcon,
} from 'lucide-react';
import { cn } from '@/lib';
import { loadPayloadValue, type ToolPayloadReader } from '../../runtime';
import { toolFailureSummary, type ToolFailureSummary } from '../failureSummary';
import {
  toolPresentation, toolResultPresentation, type ToolCategory,
} from '../toolPresentation';
import type { TimelineWorkItem } from '../types';

const ICONS: Record<ToolCategory, LucideIcon> = {
  command: Terminal,
  read: FileText,
  write: FilePenLine,
  edit: FilePenLine,
  search: Search,
  web: Globe,
  task: Wrench,
  ask: MessageCircleQuestion,
  plan: ListChecks,
  other: Wrench,
};

const RUNNING = new Set(['pending', 'streaming', 'running']);

function formatValue(value: unknown): string {
  if (value === undefined) return '';
  if (typeof value === 'string') return value;
  try { return JSON.stringify(value, null, 2); } catch { return String(value); }
}

/** 失败证据：优先真实结果文本与退出码；只有结果没有任何内容时才用公开 error.message。 */
function failureLabel(item: TimelineWorkItem, summary: ToolFailureSummary): string | null {
  if (!item.failed) return null;
  if (summary.detail && summary.exitCode) return `${summary.detail} · exit code ${summary.exitCode}`;
  if (summary.detail) return summary.detail;
  if (summary.exitCode) return `exit code ${summary.exitCode}`;
  return item.tool?.error?.message?.trim() || item.unavailable || '工具执行失败';
}

function StatusIcon({ status, failed }: { status: string; failed: boolean }) {
  if (failed) return <CircleAlert className="text-destructive size-3.5 shrink-0" aria-label="失败" />;
  if (RUNNING.has(status)) return <Loader2 className="text-muted-foreground size-3.5 shrink-0 animate-spin motion-reduce:animate-none" aria-label="运行中" />;
  if (status === 'completed') return <Check className="text-muted-foreground size-3.5 shrink-0" aria-label="完成" />;
  if (status === 'cancelled') return <span className="text-muted-foreground/70 shrink-0 text-[10px]">已取消</span>;
  return <span className="bg-muted-foreground/50 size-1.5 shrink-0 rounded-full" aria-hidden="true" />;
}

/** 工具行：折叠态只给动作与目标，展开后看参数和结果预览。 */
export function ToolRow({ item, payloadReader }: { item: TimelineWorkItem; payloadReader?: ToolPayloadReader }) {
  const [open, setOpen] = useState(false);
  const [fullResult, setFullResult] = useState<string | null>(null);
  const [fullResultError, setFullResultError] = useState('');
  const [loadingFullResult, setLoadingFullResult] = useState(false);
  const presentation = toolPresentation(item.tool ?? null);
  const Icon = ICONS[presentation.category];
  const tool = item.tool;
  const argumentsText = formatValue(tool?.arguments) || tool?.argumentsRef?.preview || '';
  const resultPresentation = toolResultPresentation(tool?.result);
  const resultText = resultPresentation.text || tool?.resultRef?.preview || '';
  const resultRef = tool?.resultRef;
  const summary = toolFailureSummary(tool?.result, resultRef?.preview ?? '');
  const expandedText = fullResult ?? (item.failed ? summary.fullText : resultText);
  const canLoadFullResult = Boolean(resultRef && payloadReader);
  const hasDetails = Boolean(argumentsText || expandedText || resultRef || item.unavailable);
  const failure = failureLabel(item, summary);

  const loadFullResult = async () => {
    if (!resultRef || !payloadReader || loadingFullResult) return;
    setLoadingFullResult(true);
    setFullResultError('');
    const loaded = await loadPayloadValue(resultRef, payloadReader);
    if (loaded.ok) setFullResult(toolFailureSummary(loaded.value).fullText);
    else setFullResultError(loaded.error);
    setLoadingFullResult(false);
  };

  return (
    <div
      className="py-0.5"
      data-slot="peri-tool-row"
      data-tool-status={item.status}
      data-tool-failed={item.failed ? 'true' : 'false'}
    >
      <button
        type="button"
        className="hover:bg-muted/70 flex w-full min-w-0 items-center gap-1.5 rounded-md px-1 py-0.5 text-left transition-colors"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        disabled={!hasDetails}
      >
        <ChevronRight
          className={cn('size-3 shrink-0 transition-transform', open && 'rotate-90', !hasDetails && 'opacity-0')}
          aria-hidden="true"
        />
        <Icon className="text-muted-foreground size-3.5 shrink-0" aria-hidden="true" />
        <span className={cn('shrink-0 text-[13px]', item.failed && 'text-destructive')}>{item.label}</span>
        {item.target && (
          <span className="text-muted-foreground truncate font-mono text-[11px]" title={item.target}>
            {item.target}
          </span>
        )}
        <span className="ml-auto flex shrink-0 items-center">
          <StatusIcon status={item.status} failed={item.failed} />
        </span>
      </button>
      {failure && (
        <p
          className="text-destructive/90 mt-0.5 ml-5 line-clamp-2 text-[11px] leading-4 [overflow-wrap:anywhere]"
          data-slot="peri-tool-failure"
        >
          {failure}
        </p>
      )}
      {open && (
        <div className="mt-1 mb-1.5 ml-5 flex flex-col gap-1.5">
          {item.unavailable && <p className="text-muted-foreground text-xs">工具记录暂不可用：{item.unavailable}</p>}
          {argumentsText && (
            <div>
              <div className="text-muted-foreground mb-0.5 text-[10px] tracking-wide uppercase">参数</div>
              <pre className="bg-muted/70 max-h-60 overflow-auto rounded-lg p-2.5 text-[11px] leading-4 whitespace-pre-wrap">{argumentsText}</pre>
            </div>
          )}
          {resultRef && payloadReader && !fullResult && (
            <button
              type="button"
              className="text-muted-foreground hover:text-foreground self-start text-[11px] underline underline-offset-2 disabled:opacity-45"
              data-slot="peri-tool-full-text"
              disabled={loadingFullResult}
              onClick={() => void loadFullResult()}
            >
              {loadingFullResult ? '读取全文中…' : '查看全文'}
            </button>
          )}
          {(expandedText || canLoadFullResult) && (
            <div>
              <div className="text-muted-foreground mb-0.5 text-[10px] tracking-wide uppercase">结果</div>
              <pre
                className="bg-muted/70 max-h-60 overflow-auto rounded-lg p-2.5 text-[11px] leading-4 whitespace-pre-wrap"
                data-slot="peri-tool-result"
              >{expandedText}</pre>
            </div>
          )}
          {fullResultError && (
            <p className="text-destructive text-[11px]" role="alert" data-slot="peri-tool-full-text-error">
              全文读取失败：{fullResultError}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
