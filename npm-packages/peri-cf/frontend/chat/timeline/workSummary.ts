import type { TimelineWorkItem } from './types';

export interface WorkCounts {
  tools: number;
  reasoning: number;
}

/** 统计连续 work 组；调用方已过滤 hidden reasoning。 */
export function countWork(items: readonly TimelineWorkItem[]): WorkCounts {
  const reasoning = items.filter((item) => item.kind === 'reasoning').length;
  return { tools: items.length - reasoning, reasoning };
}

/** 组头文案：只保留一种工作类型时不写「与」。 */
export function workSummaryLabel(counts: WorkCounts): string {
  if (counts.tools === 0) return `思考过程 · ${counts.reasoning} 段`;
  if (counts.reasoning === 0) return `工具调用 · ${counts.tools} 个`;
  return `思考与工具 · ${counts.tools} 个工具 / ${counts.reasoning} 段思考`;
}

/** turn fold 的紧凑摘要：接在「Worked for X」后，不重复组头前缀。 */
export function workCountsLabel(counts: WorkCounts): string {
  if (counts.tools === 0) return `${counts.reasoning} 段思考`;
  if (counts.reasoning === 0) return `${counts.tools} 个工具`;
  return `${counts.tools} 个工具 / ${counts.reasoning} 段思考`;
}
