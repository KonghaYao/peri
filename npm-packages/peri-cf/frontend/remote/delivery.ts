import type { ChatDisplayState } from "./display-state";

/**
 * 投递结算：202 只代表入队，送达事实只能由副本结算。
 *
 * 两条独立证据：输入队列把该 `inputId` 结算为 `delivered`，或对应 user 条目
 * 已进入时间线。`withdrawn` 与连接释放都按未送达拒绝，调用方保留可重试错误。
 */

export type DeliveryFact = "pending" | "delivered" | "withdrawn";

/** 输入撤回：副本明确给出未送达。 */
export const INPUT_WITHDRAWN_MESSAGE = "输入已撤回，消息未送达。";
/** 连接释放：等待中的投递不能悬空。 */
export const CONNECTION_RELEASED_MESSAGE = "会话连接已释放。";

/** 单个输入的投递事实；未出现在队列也未见 user 条目时仍为 `pending`。 */
export function deliveryFactOf(state: ChatDisplayState, inputId: string): DeliveryFact {
  const item = state.inputQueue?.items.find((candidate) => candidate.inputId === inputId);
  const deliveredEntry = state.entries.some(
    (entry) => entry.role === "user" && entry.entryId === `turn:${inputId}:user`,
  );
  // 用户条目已经出现时，队列里的 withdrawn 不能把它改判为未送达。
  if (item?.state === "delivered" || deliveredEntry) return "delivered";
  if (item?.state === "withdrawn") return "withdrawn";
  return "pending";
}

interface PendingDelivery {
  inputId: string;
  resolve(): void;
  reject(error: Error): void;
}

/**
 * 每个活跃连接一份的投递等待表。
 *
 * 不设独立超时：投递由副本事实结算，连接释放时统一拒绝；等待登记后立即
 * 用当前副本状态核对一次，避免投递事实早于回执。
 */
export class DeliveryTracker {
  private pending: PendingDelivery[] = [];

  constructor(private readonly currentState: () => ChatDisplayState) {}

  /** 等待某个输入的投递结果；已结算时立即返回。 */
  wait(inputId: string): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.pending.push({ inputId, resolve, reject });
      this.settle(this.currentState());
    });
  }

  /** 用最新副本状态结算所有已可判定的等待。 */
  settle(state: ChatDisplayState = this.currentState()): void {
    if (this.pending.length === 0) return;
    for (const pending of [...this.pending]) {
      const fact = deliveryFactOf(state, pending.inputId);
      if (fact === "delivered") {
        this.remove(pending);
        pending.resolve();
      } else if (fact === "withdrawn") {
        this.remove(pending);
        pending.reject(new Error(INPUT_WITHDRAWN_MESSAGE));
      }
    }
  }

  /** 连接释放：拒绝全部等待，避免新的输入静默丢失。 */
  rejectAll(error: Error = new Error(CONNECTION_RELEASED_MESSAGE)): void {
    for (const pending of this.pending) pending.reject(error);
    this.pending = [];
  }

  private remove(target: PendingDelivery): void {
    this.pending = this.pending.filter((candidate) => candidate !== target);
  }
}
