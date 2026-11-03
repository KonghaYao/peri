import type { ChatDisplayState } from '../runtime';

/** 上下文用量只表达「当前窗口」，不把累计处理量冒充上下文占用。 */
export interface ContextUsage {
  used: number;
  /** 上下文窗口上限；仅预算百分比可用时为 null。 */
  max: number | null;
  /** 0–100，已按数据源 clamp。 */
  percent: number;
  /** 可选的累计处理量（输入 + 输出）；没有可靠数据时缺席。 */
  totalProcessed?: number;
}

function finiteNonNegative(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function finitePositive(value: unknown): number | null {
  const number = finiteNonNegative(value);
  return number !== null && number > 0 ? number : null;
}

function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}

function totalProcessedFrom(usage: Record<string, unknown>): number | undefined {
  const input = finiteNonNegative(usage.inputTokens);
  const output = finiteNonNegative(usage.outputTokens);
  if (input === null || output === null) return undefined;
  return input + output > 0 ? input + output : undefined;
}

/**
 * 读取上下文用量：
 * 1. 最新 assistant entry 的 `tokenUsage.totalTokens/contextWindow`；
 * 2. 回退 `stateSnapshotMeta.context_total_tokens` + `budgetPct`；
 * 3. 没有可信数据时返回 null（调用方隐藏整个 meter，不显示 0）。
 */
export function readContextUsage(state: ChatDisplayState | null): ContextUsage | null {
  if (!state) return null;

  const latest = [...state.entries].reverse().find(
    (entry) => entry.role === 'assistant' && entry.tokenUsage !== undefined,
  );
  if (latest?.tokenUsage) {
    const used = finitePositive(latest.tokenUsage.totalTokens);
    const max = finitePositive(latest.tokenUsage.contextWindow);
    if (used !== null && max !== null) {
      const totalProcessed = totalProcessedFrom(latest.tokenUsage);
      return {
        used,
        max,
        percent: clampPercent((used / max) * 100),
        ...(totalProcessed === undefined ? {} : { totalProcessed }),
      };
    }
  }

  const meta = state.session.stateSnapshotMeta;
  if (!meta) return null;
  const used = finitePositive(meta.context_total_tokens);
  const budgetPct = finitePositive(meta.budgetPct);
  if (used === null || budgetPct === null || budgetPct > 1) return null;
  return { used, max: null, percent: clampPercent(budgetPct * 100) };
}
