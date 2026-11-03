import type {
  ChatFact,
  ChatFactEnvelope,
  ChatFactObserver,
  ChatServiceContract,
} from "@peri-code/sdk";

/**
 * 每个 Chat 的运行事实计数器。
 *
 * 只保留数值与标识：不保存通知原文、prompt、消息、工具标题/参数/结果或错误原文。
 * 计数自当前绑定的 `onBinding` 起累计；会话切换会重置。
 */

export interface InstanceTurnMetrics {
  started: number;
  completed: number;
  cancelled: number;
  error: number;
  /** 已结算回合的墙钟总和；不是 CPU 时间。 */
  totalWallMs: number;
  /** 最近一个已结算回合的墙钟；未结算时为 null。 */
  lastWallMs: number | null;
  /** 当前回合已运行的墙钟；空闲或起始时间未知时为 null。 */
  activeWallMs: number | null;
}

export interface InstanceTokenMetrics {
  /** ACP `usage_update` 次数，含子代理。 */
  llmCalls: number;
  inputTokens: number | null;
  outputTokens: number | null;
  /** 带 `sourceAgentId` 的调用数；已包含在 `llmCalls` 与 token 总和中。 */
  auxiliaryCalls: number;
  /** 任一调用缺少 token 数值；累计值不完整。 */
  partial: boolean;
}

export interface InstanceToolMetrics {
  started: number;
  completed: number;
  failed: number;
  cancelled: number;
}

export interface InstanceBusinessSnapshot {
  running: boolean;
  lastActivityAtMs: number | null;
  queueDepth: number;
  pendingInteractions: number;
  turns: InstanceTurnMetrics;
  tokens: InstanceTokenMetrics | null;
  context: { used: number; size: number } | null;
  tools: InstanceToolMetrics | null;
  lastStopReason: string | null;
}

/** `turn-complete.stopReason` 只保留 ACP 已知枚举，避免把运行侧自由文本带出网。 */
const STOP_REASONS = new Set(["end_turn", "max_tokens", "max_turn_requests", "cancelled", "error"]);
const QUEUE_SETTLED = new Set(["delivered", "withdrawn"]);
const TERMINAL_TOOL_STATUSES = new Set(["completed", "failed", "error", "cancelled"]);
const MAX_TRACKED_IDS = 1024;

interface ChatMetricsState {
  subscribed: boolean;
  running: boolean;
  turnStartedAtMs: number | null;
  lastActivityAtMs: number | null;
  queueDepth: number;
  pendingInteractions: Set<string>;
  turns: InstanceTurnMetrics;
  tokens: InstanceTokenMetrics | null;
  context: { used: number; size: number } | null;
  tools: InstanceToolMetrics | null;
  seenToolIds: Map<string, true>;
  lastStopReason: string | null;
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function finite(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function nonEmpty(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function emptyState(): ChatMetricsState {
  return {
    subscribed: true,
    running: false,
    turnStartedAtMs: null,
    lastActivityAtMs: null,
    queueDepth: 0,
    pendingInteractions: new Set(),
    turns: {
      started: 0,
      completed: 0,
      cancelled: 0,
      error: 0,
      totalWallMs: 0,
      lastWallMs: null,
      activeWallMs: null,
    },
    tokens: null,
    context: null,
    tools: null,
    seenToolIds: new Map(),
    lastStopReason: null,
  };
}

/** 有界登记：只保留 id，防止异常长的交互/工具流无限增长。 */
function remember(set: Set<string>, value: string): void {
  if (set.has(value)) return;
  set.add(value);
  if (set.size <= MAX_TRACKED_IDS) return;
  const oldest = set.values().next().value;
  if (oldest !== undefined) set.delete(oldest);
}

function rememberTool(map: Map<string, true>, id: string): void {
  if (map.has(id)) return;
  map.set(id, true);
  if (map.size <= MAX_TRACKED_IDS) return;
  const oldest = map.keys().next().value;
  if (oldest !== undefined) map.delete(oldest);
}

function queueDepthOf(snapshot: unknown): number {
  const value = record(snapshot);
  if (!Array.isArray(value?.items)) return 0;
  let depth = 0;
  for (const item of value.items) {
    const state = nonEmpty(record(item)?.state);
    if (state !== null && !QUEUE_SETTLED.has(state)) depth += 1;
  }
  return depth;
}

export class ChatInstanceMetricsCollector {
  private readonly states = new Map<string, ChatMetricsState>();
  private readonly unsubscribers = new Map<string, () => void>();

  constructor(private readonly now: () => number = Date.now) {}

  /** 订阅句柄的事实流；重复 track 无副作用。 */
  track(chat: ChatServiceContract): void {
    if (this.states.has(chat.id) || this.unsubscribers.has(chat.id)) return;
    const state = emptyState();
    this.states.set(chat.id, state);
    const observer: ChatFactObserver = {
      onBinding: (_target, replay) => this.onBinding(chat.id, replay),
      onFact: (envelope) => this.onFact(chat.id, envelope),
      onClosed: () => {
        const current = this.states.get(chat.id);
        if (current) current.subscribed = false;
      },
    };
    try {
      this.unsubscribers.set(chat.id, chat.subscribeFacts(observer));
    } catch {
      // 订阅失败只关闭计数，不影响 Chat 创建；UI 显示“业务计数不可用”。
      const current = this.states.get(chat.id) ?? state;
      current.subscribed = false;
      this.states.set(chat.id, current);
    }
  }

  /** 取消订阅并删除计数；关闭后的 Chat 不再出现在监控里。 */
  forget(chatId: string): void {
    try { this.unsubscribers.get(chatId)?.(); }
    catch { /* 退订失败不影响关闭 */ }
    this.unsubscribers.delete(chatId);
    this.states.delete(chatId);
  }

  disposeAll(): void {
    for (const unsubscribe of this.unsubscribers.values()) {
      try { unsubscribe(); } catch { /* 停机清理聚合在注册表 */ }
    }
    this.unsubscribers.clear();
    this.states.clear();
  }

  /** 只读业务计数；未订阅或无状态时返回 null，不用 0 冒充。 */
  snapshot(chatId: string, nowMs: number): InstanceBusinessSnapshot | null {
    const state = this.states.get(chatId);
    if (!state || !state.subscribed) return null;
    return {
      running: state.running,
      lastActivityAtMs: state.lastActivityAtMs,
      queueDepth: state.queueDepth,
      pendingInteractions: state.pendingInteractions.size,
      turns: {
        ...state.turns,
        activeWallMs: state.running && state.turnStartedAtMs !== null
          ? Math.max(0, nowMs - state.turnStartedAtMs)
          : null,
      },
      tokens: state.tokens === null ? null : { ...state.tokens },
      context: state.context === null ? null : { ...state.context },
      tools: state.tools === null ? null : { ...state.tools },
      lastStopReason: state.lastStopReason,
    };
  }

  /** 绑定代次切换：重置计数，只从 replay 恢复当前运行态与未决计数。 */
  private onBinding(chatId: string, replay: readonly ChatFactEnvelope[]): void {
    const previous = this.states.get(chatId);
    const state = emptyState();
    state.subscribed = previous?.subscribed ?? true;
    this.states.set(chatId, state);
    for (const envelope of replay) {
      const fact = envelope.fact;
      if (fact.kind === "run-state") {
        state.running = fact.running;
        // replay 没有可靠时间戳；不伪造当前回合墙钟。
        state.turnStartedAtMs = null;
      } else if (fact.kind === "input-queue") {
        state.queueDepth = queueDepthOf(fact.snapshot);
      } else if (fact.kind === "interaction-began") {
        remember(state.pendingInteractions, fact.record.id);
      } else if (fact.kind === "interaction-settled") {
        state.pendingInteractions.delete(fact.id);
      } else if (fact.kind === "interactions-expired") {
        state.pendingInteractions.clear();
      }
    }
  }

  private onFact(chatId: string, envelope: ChatFactEnvelope): void {
    const state = this.states.get(chatId);
    if (!state || !state.subscribed) return;
    const now = this.now();
    state.lastActivityAtMs = now;
    this.accept(state, envelope.fact, now);
  }

  private accept(state: ChatMetricsState, fact: ChatFact, now: number): void {
    switch (fact.kind) {
      case "run-state":
        if (fact.running) {
          if (!state.running) {
            state.running = true;
            state.turns.started += 1;
            state.turnStartedAtMs = now;
          }
        } else {
          state.running = false;
          state.turnStartedAtMs = null;
        }
        return;
      case "turn-complete": {
        const wall = state.turnStartedAtMs === null
          ? null
          : Math.max(0, now - state.turnStartedAtMs);
        if (fact.status === "completed") state.turns.completed += 1;
        else if (fact.status === "cancelled") state.turns.cancelled += 1;
        else state.turns.error += 1;
        if (wall !== null) {
          state.turns.totalWallMs += wall;
          state.turns.lastWallMs = wall;
        }
        state.lastStopReason = fact.stopReason !== undefined && STOP_REASONS.has(fact.stopReason)
          ? fact.stopReason
          : null;
        state.running = false;
        state.turnStartedAtMs = null;
        return;
      }
      case "input-queue":
        state.queueDepth = queueDepthOf(fact.snapshot);
        return;
      case "interaction-began":
        remember(state.pendingInteractions, fact.record.id);
        return;
      case "interaction-settled":
        state.pendingInteractions.delete(fact.id);
        return;
      case "interactions-expired":
        state.pendingInteractions.clear();
        return;
      case "notice":
        this.acceptNotice(state, fact.notification);
        return;
      default:
        return;
    }
  }

  /** 只读白名单数值字段；从不保存 `update`/`content`/`title` 等原始对象。 */
  private acceptNotice(state: ChatMetricsState,
    notification: { method?: unknown; params?: unknown }): void {
    if (notification.method !== "session/update") return;
    const params = record(notification.params);
    const update = record(params?.update);
    if (!update) return;
    const kind = nonEmpty(update.sessionUpdate);
    if (kind === "usage_update") {
      this.acceptUsage(state, update, record(params?._meta));
      return;
    }
    if (kind === "tool_call") {
      const id = nonEmpty(update.toolCallId) ?? nonEmpty(update.id);
      state.tools ??= { started: 0, completed: 0, failed: 0, cancelled: 0 };
      state.tools.started += 1;
      if (id !== null) rememberTool(state.seenToolIds, id);
      return;
    }
    if (kind === "tool_call_update") {
      const id = nonEmpty(update.toolCallId) ?? nonEmpty(update.id);
      const status = nonEmpty(update.status);
      if (id === null || status === null) return;
      state.tools ??= { started: 0, completed: 0, failed: 0, cancelled: 0 };
      // 没有对应的开始事件或重复终态：忽略，避免重复计数。
      if (!state.seenToolIds.delete(id)) return;
      if (status === "completed") state.tools.completed += 1;
      else if (status === "failed" || status === "error") state.tools.failed += 1;
      else if (status === "cancelled") state.tools.cancelled += 1;
      else state.seenToolIds.set(id, true);
      return;
    }
  }

  private acceptUsage(state: ChatMetricsState, update: Record<string, unknown>,
    paramsMeta: Record<string, unknown> | null): void {
    state.tokens ??= {
      llmCalls: 0,
      inputTokens: null,
      outputTokens: null,
      auxiliaryCalls: 0,
      partial: false,
    };
    state.tokens.llmCalls += 1;
    const sourceAgentId = nonEmpty(record(paramsMeta?.peri)?.sourceAgentId);
    if (sourceAgentId !== null) state.tokens.auxiliaryCalls += 1;

    const used = finite(update.used);
    const size = finite(update.size);
    if (sourceAgentId === null && used !== null && size !== null && size > 0) {
      state.context = { used, size };
    }

    const meta = record(update._meta);
    const input = finite(meta?.inputTokens);
    const output = finite(meta?.outputTokens);
    if (input === null || output === null) state.tokens.partial = true;
    if (input !== null) state.tokens.inputTokens = (state.tokens.inputTokens ?? 0) + input;
    if (output !== null) state.tokens.outputTokens = (state.tokens.outputTokens ?? 0) + output;
  }
}
