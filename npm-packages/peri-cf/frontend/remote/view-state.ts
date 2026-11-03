import type { ChatReplicaStatus } from "@peri-code/sdk/view";
import { RemoteActionError, parseActionTarget } from "./actions";
import type { ActionTargetDTO, Chat } from "./contracts";
import type { ChatDisplayState } from "./display-state";
import type { SyncConnectionStatus } from "./sync";

/**
 * 视图状态派生：把控制器内部事实（副本就绪度、连接状态、取消意图、动作错误）
 * 映射为 `ChatView` 字段。
 *
 * 全部是无副作用的纯函数：输入显式、不读类实例、不构造 `ChatDisplayState`
 * （DTO 物化只由 `display-state` 负责），便于单独覆盖边界。
 */

/** 页面阶段；`streaming`/`cancelling` 由副本运行事实驱动，不由列表推断。 */
export type Phase = "idle" | "loading" | "streaming" | "cancelling" | "syncing" | "blocked";

/** 读面状态：`unsupported` 与「空历史」明确区分。 */
export type ChatLoadState = "idle" | "loading" | "ready" | "failed" | "unsupported";

export type SyncStatus = "connecting" | "synced" | "failed";

/** 恢复入口：错误文案必须有可执行动作，不能让用户停留在无法解释的阻断态。 */
export type ChatRecovery = "none" | "reload";

/** 发送结果：202 只代表入队；`delivered` 由副本队列/用户条目结算。 */
export type ChatSendOutcome =
  | { delivered: true; sessionId: string }
  | {
    delivered: false;
    sessionId: string | null;
    message: string;
    /** 只有 rejected 允许在草稿未改写时直接重试；unknown 必须先重新同步。 */
    disposition?: "rejected" | "unknown" | "not_sent";
  };

/** 控制器发布的只读视图；UI 只消费，不直接修改。 */
export interface ChatView {
  selected: Chat | null;
  state: ChatDisplayState | null;
  phase: Phase;
  historyReady: boolean;
  loadState: ChatLoadState;
  liveSync: SyncStatus;
  error: string;
  /** 当前错误对应的恢复动作；`none` 表示只能等待或 dismiss。 */
  recovery: ChatRecovery;
}

/** 不受支持的数据版本：只在读面事实为 `unsupported` 时出现。 */
const UNSUPPORTED = "会话使用了不受支持的数据版本，请刷新页面或升级应用后再继续。";

/** 错误归一：宿主字符串、Error 与任意抛出值用同一条规则取文案。 */
export function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export interface LiveSyncInput {
  /** 副本状态；没有活跃副本时为 undefined。 */
  projectionStatus?: ChatReplicaStatus;
  syncStatus?: SyncConnectionStatus;
}

/** 读面同步状态：副本已同步才算 `synced`；副本或连接任一失败都算 `failed`。 */
export function deriveLiveSync(input: LiveSyncInput): SyncStatus {
  if (input.projectionStatus === "synced") return "synced";
  if (input.projectionStatus === "failed" || input.syncStatus === "failed") return "failed";
  return "connecting";
}

export interface LoadStateInput {
  hasActive: boolean;
  hasSelection: boolean;
  /** 副本就绪度；没有活跃副本时为 null。 */
  readiness: ChatDisplayState["readiness"] | null;
  /** 本连接是否曾经就绪；用于断线后仍保留可读历史。 */
  everReady: boolean;
  /** 上一次发布的读面状态；`unsupported` 一旦确认不会被断线降级。 */
  previous: ChatLoadState;
  liveSync: SyncStatus;
  connectionError: string;
}

/** 读面状态：先看副本就绪度，再看断线/连接错误，最后落到加载中。 */
export function deriveLoadState(input: LoadStateInput): ChatLoadState {
  if (!input.hasActive) return input.hasSelection ? "loading" : "idle";
  if (input.readiness === "ready") return "ready";
  if (input.readiness === "unsupported") return "unsupported";
  if (input.everReady) return input.previous === "unsupported" ? "unsupported" : "ready";
  return input.liveSync === "failed" || input.connectionError ? "failed" : "loading";
}

export interface PhaseInput {
  hasActive: boolean;
  loadState: ChatLoadState;
  /** 动作结果未知：必须阻断输入，直到重新同步。 */
  uncertain: boolean;
  executionBlocked: boolean;
  recovery: ChatRecovery;
  running: boolean;
  cancelPending: boolean;
  historyReady: boolean;
}

/**
 * 页面阶段。阻断优先于运行态：未知结果与执行阻断都不能靠再次发送解除；
 * 已就绪但需要重新同步的恢复动作同样先阻断，避免重复发送同一条 409。
 */
export function derivePhase(input: PhaseInput): Phase {
  if (!input.hasActive) return "idle";
  if (input.loadState === "loading") return "loading";
  if (input.uncertain || input.executionBlocked) return "blocked";
  if (input.recovery === "reload" && input.loadState === "ready") return "blocked";
  if (input.running) return input.cancelPending ? "cancelling" : "streaming";
  if (!input.historyReady && input.loadState !== "failed" && input.loadState !== "unsupported") return "syncing";
  return "idle";
}

/** 取消意图只对发起时的 running turn 有效。 */
export interface CancelIntent {
  requested: boolean;
  /** 发起取消时的 turnId；null 表示没有可归属的轮次。 */
  turnId: string | null;
}

/** `running=false` 或 active turn 已换代即失效，避免停止后输入被永久拦住。 */
export function isCancelPending(intent: CancelIntent, state: ChatDisplayState | null): boolean {
  if (!intent.requested) return false;
  if (!state?.running) return false;
  if (intent.turnId !== null && state.activeTurn.turnId !== null
    && state.activeTurn.turnId !== intent.turnId) return false;
  return true;
}

/** 动作目标绑定：副本尚未绑定或代次非法时返回 null，不猜目标。 */
export function bindingTargetOf(state: ChatDisplayState | null): ActionTargetDTO | null {
  if (!state?.sessionId || state.bindingRevision === null) return null;
  try {
    return parseActionTarget({ sessionId: state.sessionId, bindingRevision: state.bindingRevision });
  } catch {
    return null;
  }
}

export interface ActionFailure {
  /** 已按 disposition/code 补充恢复说明的可见文案。 */
  message: string;
  recovery: ChatRecovery;
  /** 结果为未知时需要置位；调用方只置 true，不把已确认的未知状态回退。 */
  uncertain: boolean;
}

/**
 * 动作失败归并：`disposition` 是唯一可重试性权威。
 * 未知结果与过期绑定都需要重新同步；普通拒绝（例如校验失败）没有恢复动作。
 */
export function describeActionFailure(error: unknown): ActionFailure {
  const remote = error instanceof RemoteActionError
    ? error
    : new RemoteActionError("internal_error", messageOf(error), "unknown");
  const base = remote.code === "close_incomplete" ? `关闭未完成：${remote.message}` : remote.message;
  const recovery = remote.disposition === "unknown" || remote.code === "binding_conflict" ? "reload" : "none";
  const message = remote.disposition === "unknown"
    ? `${base} 执行状态尚未确认，请重新加载会话后再继续。`
    : remote.code === "binding_conflict"
      ? `${base} 会话绑定已过期，请重新同步后再发送。`
      : base;
  return { message, recovery, uncertain: remote.disposition === "unknown" };
}

export interface RawErrorInput {
  actionError: string;
  connectionError: string;
  loadState: ChatLoadState;
}

/** 原始错误优先级：动作错误 > 连接错误 > 不受支持的数据版本。 */
export function rawErrorOf(input: RawErrorInput): string {
  if (input.actionError) return input.actionError;
  if (input.connectionError) return input.connectionError;
  if (input.loadState === "unsupported") return UNSUPPORTED;
  return "";
}
