import type { ChatDisplayState, ToolCall } from '../runtime';
import { toolPresentation } from './toolPresentation';
import type { TimelineRow, TimelineWorkItem } from './types';
import { workCountsLabel, type WorkCounts } from './workSummary';

const RUNNING_STATUSES = new Set(['pending', 'streaming', 'running']);
/** `cancelled` 是用户停止造成的中性终态，不算失败；只有 failed/error 进入红色语义。 */
const FAILED_STATUSES = new Set(['failed', 'error']);
/** 载荷超限只表示结果被截断/降级；工具执行本身在 SDK 中仍是 completed。 */
const DEGRADED_ERROR_CODE = 'payload_too_large';

function isRunning(status: string): boolean {
  return RUNNING_STATUSES.has(status);
}

/** 工具执行事实：状态是唯一权威，错误码只区分「降级」与「失败」。 */
export interface ToolWorkState {
  failed: boolean;
  degraded: boolean;
  cancelled: boolean;
}

/** 单一规则：时间线行与 turn 摘要共用，避免两处对 `error !== null` 各判一次。 */
export function toolWorkState(tool: ToolCall | null | undefined): ToolWorkState {
  if (!tool) return { failed: true, degraded: false, cancelled: false };
  return {
    failed: FAILED_STATUSES.has(tool.status),
    degraded: tool.error?.code === DEGRADED_ERROR_CODE,
    cancelled: tool.status === 'cancelled',
  };
}

/** 与 t3code 一致的时间压缩：短于 10s 保留十分位，之后整秒/分/时。 */
export function formatWorkDuration(durationMs: number): string {
  if (!Number.isFinite(durationMs) || durationMs <= 0) return '0ms';
  if (durationMs < 1_000) return `${Math.round(durationMs)}ms`;
  if (durationMs < 10_000) {
    const tenths = Math.round(durationMs / 100) / 10;
    return tenths >= 10 ? '10s' : `${tenths.toFixed(1)}s`;
  }
  const totalSeconds = Math.round(durationMs / 1_000);
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  const parts: string[] = [];
  if (hours > 0) parts.push(`${hours}h`);
  if (minutes > 0) parts.push(`${minutes}m`);
  if (seconds > 0) parts.push(`${seconds}s`);
  return parts.join(' ');
}

/** 只使用 turn 自身的起止时间；缺失时显示不带时长的完成态，不猜时间。 */
export function turnFoldLabel(turn: {
  status?: string; startedAt: string | null; completedAt: string | null;
}, facts: { failed?: boolean; cancelled?: boolean } = {}): string {
  const failed = facts.failed === true || turn.status === 'error' || turn.status === 'failed';
  // 失败优先于取消：同一轮既有失败工具又被取消时，不能把失败降级成中性取消。
  const cancelled = !failed && (facts.cancelled === true || turn.status === 'cancelled');
  const prefix = failed ? 'Failed' : cancelled ? 'Cancelled' : 'Worked';
  if (!turn.startedAt || !turn.completedAt) return prefix;
  const start = Date.parse(turn.startedAt);
  const end = Date.parse(turn.completedAt);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return prefix;
  const duration = formatWorkDuration(end - start);
  // `Worked` 沿用 t3code 文案；失败/取消用 after 明确区分终态。
  return failed || cancelled ? `${prefix} after ${duration}` : `Worked for ${duration}`;
}

interface TurnWorkFacts {
  failed: boolean;
  cancelled: boolean;
}

/**
 * fold 摘要必须按整轮汇总事实，而不是只看 fold 发出时的当前 work 组：
 * 「工具 → 文本 → 失败工具」里的失败发生在后一组，摘要仍要显示 Failed。
 */
function collectTurnWorkFacts(state: ChatDisplayState): Map<string, TurnWorkFacts> {
  const facts = new Map<string, TurnWorkFacts>();
  for (const entry of state.entries) {
    if (entry.role === 'user') continue;
    const current = facts.get(entry.turnId) ?? { failed: false, cancelled: false };
    for (const block of entry.blocks) {
      if (block.type !== 'tool_call') continue;
      const outcome = toolWorkState(block.tool);
      if (outcome.failed) current.failed = true;
      if (outcome.cancelled) current.cancelled = true;
    }
    if (current.failed || current.cancelled) facts.set(entry.turnId, current);
  }
  return facts;
}

/** 整轮工作量按 turn 汇总，供 fold 摘要使用；hidden reasoning 不计入。 */
function collectTurnWorkCounts(state: ChatDisplayState): Map<string, WorkCounts> {
  const counts = new Map<string, WorkCounts>();
  for (const entry of state.entries) {
    if (entry.role === 'user') continue;
    const current = counts.get(entry.turnId) ?? { tools: 0, reasoning: 0 };
    for (const block of entry.blocks) {
      if (block.type === 'reasoning' && block.visibility !== 'hidden') current.reasoning += 1;
      if (block.type === 'tool_call') current.tools += 1;
    }
    if (current.tools > 0 || current.reasoning > 0) counts.set(entry.turnId, current);
  }
  return counts;
}

/** 运行中的 turn 永不折叠：运行元数据优先于可能残留的 activeTurnId。 */
const LIVE_TURN_STATUSES = new Set(['accepting', 'running', 'cancelling']);
function liveTurnId(state: ChatDisplayState): string | null {
  if (state.running === true) return state.activeTurn.turnId;
  const status = state.activeTurn.status;
  return status !== null && LIVE_TURN_STATUSES.has(status) ? state.activeTurn.turnId : null;
}

interface DeriveTimelineOptions {
  /** 用户已展开的 turn fold；未列出的已完成 turn 隐藏 work 行。 */
  expandedTurnIds?: ReadonlySet<string>;
  /** 用户显式折叠的 turn；失败/取消 turn 默认展开，因此需要单独的折叠集合。 */
  collapsedTurnIds?: ReadonlySet<string>;
}

function isTurnExpanded(
  turn: { turnId: string; status: string },
  expanded: ReadonlySet<string>,
  collapsed: ReadonlySet<string>,
): boolean {
  if (collapsed.has(turn.turnId)) return false;
  if (expanded.has(turn.turnId)) return true;
  // 失败/取消的 work 默认可见，只在用户显式折叠后收起。
  return turn.status !== 'completed';
}

function toolWorkItem(
  entry: ChatDisplayState['entries'][number],
  block: Extract<ChatDisplayState['entries'][number]['blocks'][number], { type: 'tool_call' }>,
): TimelineWorkItem {
  const tool = block.tool;
  const presentation = toolPresentation(tool);
  const status = tool?.status ?? 'unavailable';
  const outcome = toolWorkState(tool);
  return {
    kind: 'tool',
    id: tool?.toolCallId ?? `${entry.entryId}:${block.blockId}`,
    entryId: entry.entryId,
    blockId: block.blockId,
    ...(block.toolCallId ? { toolCallId: block.toolCallId } : {}),
    status,
    label: presentation.action,
    ...(presentation.target ? { target: presentation.target } : {}),
    createdAt: tool?.createdAt ?? entry.createdAt,
    completedAt: tool?.completedAt ?? entry.completedAt,
    failed: outcome.failed,
    ...(outcome.degraded ? { degraded: true } : {}),
    ...(block.unavailable ? { unavailable: block.unavailable } : {}),
    ...(tool ? { tool } : {}),
  };
}

function reasoningWorkItem(
  entry: ChatDisplayState['entries'][number],
  block: Extract<ChatDisplayState['entries'][number]['blocks'][number], { type: 'reasoning' }>,
): TimelineWorkItem {
  return {
    kind: 'reasoning',
    id: `${entry.entryId}:${block.blockId}`,
    entryId: entry.entryId,
    blockId: block.blockId,
    status: entry.status,
    label: '思考',
    text: block.text,
    createdAt: entry.createdAt,
    completedAt: entry.completedAt,
    failed: false,
  };
}

/**
 * 从全量 `ChatDisplayState` 派生时间线行：只读、无副作用，行序与投影 entries 一致。
 * reasoning/tool 连续出现时合并为一个 work 组；text、unknown、error 与 user 行都会打断组。
 * 已结算 turn 的 work 折叠为一条 `Worked for X`；live turn 永不折叠。
 * 失败/取消 turn 默认展开，折叠摘要保留终态；失败工具始终可见。
 */
export function deriveTimeline(
  state: ChatDisplayState | null, options: DeriveTimelineOptions = {},
): TimelineRow[] {
  if (!state || state.readiness !== 'ready') return [];
  const rows: TimelineRow[] = [];
  const activeTurnId = liveTurnId(state);
  const expandedTurnIds = options.expandedTurnIds ?? new Set<string>();
  const collapsedTurnIds = options.collapsedTurnIds ?? new Set<string>();
  // 只有可确认结算、且不是 live 的 turn 才折叠；失败/取消默认展开但仍提供摘要。
  const settledTurns = new Map<string, ChatDisplayState['turns'][number]>();
  for (const turn of state.turns) {
    if (turn.turnId === activeTurnId) continue;
    if (turn.status !== 'completed' && turn.status !== 'error' && turn.status !== 'cancelled') continue;
    settledTurns.set(turn.turnId, turn);
  }
  const turnWorkFacts = collectTurnWorkFacts(state);
  const turnWorkCounts = collectTurnWorkCounts(state);
  const emittedFolds = new Set<string>();
  let workItems: TimelineWorkItem[] = [];
  let workActive = false;
  let workTurnId: string | null = null;

  const flushWork = (): void => {
    if (workItems.length === 0) {
      workTurnId = null;
      workActive = false;
      return;
    }
    const turnId = workTurnId;
    const foldTurn = turnId ? settledTurns.get(turnId) : undefined;
    const hasFailure = workItems.some((item) => item.failed);
    // fold 摘要覆盖整轮：先汇总全部 work 组，再决定 label 与失败色。
    const facts = turnId ? turnWorkFacts.get(turnId) : undefined;
    const turnToolFailure = facts?.failed === true;
    const turnFailed = turnToolFailure ||
      foldTurn?.status === 'error' || foldTurn?.status === 'failed';
    const turnCancelled = facts?.cancelled === true || foldTurn?.status === 'cancelled';
    const terminalFailure = foldTurn !== undefined &&
      (foldTurn.status === 'error' || foldTurn.status === 'failed' || foldTurn.status === 'cancelled');
    let foldExpanded = false;
    if (turnId && foldTurn) {
      // 有失败证据时 fold 只做静态摘要：既不会隐藏失败工具，也不提供无效的折叠开关。
      const expanded = turnFailed ? true : isTurnExpanded(foldTurn, expandedTurnIds, collapsedTurnIds);
      foldExpanded = expanded;
      if (!emittedFolds.has(turnId)) {
        emittedFolds.add(turnId);
        const counts = turnWorkCounts.get(turnId);
        rows.push({
          kind: 'turn-fold',
          id: `turn-fold:${turnId}`,
          turnId,
          status: foldTurn.status,
          label: turnFoldLabel(foldTurn, { failed: turnFailed, cancelled: turnCancelled }),
          ...(counts ? { summary: workCountsLabel(counts) } : {}),
          failed: turnFailed,
          startedAt: foldTurn.startedAt,
          completedAt: foldTurn.completedAt,
          expanded,
          ...(turnFailed ? { static: true } : {}),
        });
      }
      // 折叠只隐藏干净的已完成 work；失败工具始终保留，不因 fold 丢失错误证据。
      if (!expanded && !hasFailure && !turnToolFailure) {
        workItems = [];
        workActive = false;
        workTurnId = null;
        return;
      }
    }
    // 干净 turn 展开后由 fold 作为唯一折叠层；失败/取消保留组头与失败计数。
    const bare = foldTurn !== undefined && foldExpanded && !turnFailed && !terminalFailure;
    rows.push({
      kind: 'work',
      id: `work:${workItems[0]!.id}`,
      turnId,
      items: workItems,
      active: workActive || workItems.some((item) => isRunning(item.status)),
      failed: hasFailure,
      // 失败/取消 turn 的 work 与失败工具必须在首屏可见，不能只靠 fold 摘要。
      defaultExpanded: workActive || hasFailure || terminalFailure ||
        workItems.some((item) => isRunning(item.status)),
      forceExpanded: hasFailure || terminalFailure,
      ...(bare ? { bare: true } : {}),
    });
    workItems = [];
    workActive = false;
    workTurnId = null;
  };

  for (const entry of state.entries) {
    if (entry.role === 'user') {
      flushWork();
      rows.push({ kind: 'user', id: `user:${entry.entryId}`, entry });
      continue;
    }

    const entryActive = activeTurnId !== null && entry.turnId === activeTurnId;
    for (const block of entry.blocks) {
      switch (block.type) {
        case 'text':
          flushWork();
          rows.push({
            kind: 'assistant',
            id: `assistant:${entry.entryId}:${block.blockId}`,
            entryId: entry.entryId,
            text: block.text,
            streaming: isRunning(entry.status),
            status: entry.status,
            createdAt: entry.createdAt,
          });
          break;
        case 'reasoning':
          // hidden reasoning 不进入展示，但也不应打断相邻的 work 组。
          if (block.visibility === 'hidden') break;
          workTurnId ??= entry.turnId;
          workItems.push(reasoningWorkItem(entry, block));
          workActive ||= entryActive || isRunning(entry.status);
          break;
        case 'tool_call':
          workTurnId ??= entry.turnId;
          workItems.push(toolWorkItem(entry, block));
          workActive ||= entryActive;
          break;
        case 'unknown':
          flushWork();
          rows.push({
            kind: 'unknown',
            id: `unknown:${entry.entryId}:${block.blockId}`,
            entryId: entry.entryId,
            blockId: block.blockId,
            reason: block.reason,
          });
          break;
      }
    }

    if (entry.error) {
      flushWork();
      rows.push({
        kind: 'error',
        id: `error:${entry.entryId}`,
        entryId: entry.entryId,
        message: entry.error.message,
      });
    }
  }
  flushWork();

  if (state.running || activeTurnId !== null) {
    const turn = activeTurnId
      ? state.turns.find((item) => item.turnId === activeTurnId)
      : state.turns.at(-1);
    rows.push({
      kind: 'working',
      id: 'working-indicator-row',
      turnId: activeTurnId,
      startedAt: turn?.startedAt ?? null,
    });
  }

  return rows;
}
