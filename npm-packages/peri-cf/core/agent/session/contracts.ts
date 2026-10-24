import type { ChatCommand, ChatCommandPayload, CommandDisposition, SyncState } from "../../shared";

/**
 * 会话命令（宿主运行期的唯一执行入口）。失败以 `ChatCommandError` 抛出，码与处置保持稳定：
 * `rejected`（明确未生效）：prompt `busy`/`blocked`；cancel `blocked`；interaction `settled`/`invalid_answer`；
 * payload `unavailable`/`integrity_error`/`too_large`；会话缺失 `not_found`。
 * `unknown`（执行状态未知）：`internal_error`、cancel `cancel_timeout`/`cancel_failed`。
 */
export type { ChatCommand, ChatCommandPayload } from "../../shared";

/** `watch` 建立订阅后返回的初始读面；`close` 只释放本次订阅。 */
export interface ChatRuntimeSubscription {
  /** 建立订阅瞬间的完整读面。 */
  initial: SyncState;
  /** 释放本次订阅；幂等，不影响其它订阅者。 */
  close(): void;
}

/** 驻留运行期的事实摘要：只读内存态，不触发复原。 */
export interface RuntimeSummary {
  /** 是否有执行在途。 */
  running: boolean;
  /** 未决交互数。 */
  pendingInteractionCount: number;
  /** 最近一轮终态；尚无轮次时为空。 */
  lastTurnStatus: string | null;
  /** 最近一轮完成时间（ISO）；尚无轮次时为空。 */
  lastCompletedAt: string | null;
}

/**
 * 命令通道的最小运行期接口：本机实现直接调用宿主，远程实现转发 HTTP/SSE。
 * `watch` 内部完成「先订阅、再取初始快照」，调用方不再自行拼装顺序。
 */
export interface ChatRuntimePort {
  /** 先订阅、再取初始快照；后续变化由 `onState` 推送。 */
  watch(onState: (state: SyncState) => void): Promise<ChatRuntimeSubscription>;
  /** 读取最新投影；首次访问可能触发冷复原。 */
  read(): Promise<SyncState>;
  /** 执行命令；失败以 `ChatCommandError` 抛出。 */
  command(command: ChatCommand): Promise<ChatCommandPayload | undefined>;
  /** 释放实例；之后不再接受新工作。 */
  dispose(): void;
}

/** 命令失败：`code` 稳定，`disposition` 表示是否明确未生效。 */
export class ChatCommandError extends Error {
  constructor(readonly code: string, message: string, readonly disposition: CommandDisposition) {
    super(message);
    this.name = "ChatCommandError";
  }
}

/** 会话不存在；与其它命令错误区分，供 API 映射 404。 */
export class ChatNotFoundError extends Error {
  constructor(readonly chatId: string) {
    super("Chat not found");
    this.name = "ChatNotFoundError";
  }
}
