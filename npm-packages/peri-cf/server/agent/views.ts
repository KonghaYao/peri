import type { ChatService } from "@peri-code/sdk";
import {
  ChatView,
  type ChatViewOptions,
} from "@peri-code/sdk";

interface ViewEntry {
  readonly chat: ChatService;
  readonly view: ChatView;
}

/**
 * 每个 Chat 一个 `ChatView`，同 Chat 的所有同步连接共享这个视图。
 *
 * - 连接数由 `ChatView` 自己管理；这里不因最后一个连接断开而释放视图。
 * - 视图按 ChatService 的事实日志重放，绑定换代由 `ChatView` 成对替换文档；
 *   registry 只暴露当前代次供诊断/测试，不自行重建副本。
 * - 关闭 Chat 或进程退出时才 `dispose`；释放失败保留条目供重试。
 */
export class ChatViewRegistry {
  private readonly entries = new Map<string, ViewEntry>();
  private readonly options: ChatViewOptions;

  constructor(options: ChatViewOptions = {}) {
    this.options = options;
  }

  /** 返回同 Chat 的既有视图；不存在时同步挂载。 */
  attach(chat: ChatService): ChatView {
    const existing = this.entries.get(chat.id);
    if (existing) {
      if (existing.chat !== chat) {
        throw new Error(
          `Chat ${chat.id} already has a view from a different ChatService instance`,
        );
      }
      return existing.view;
    }
    const view = new ChatView(chat, this.options);
    this.entries.set(chat.id, { chat, view });
    return view;
  }

  get(chatId: string): ChatView | undefined {
    return this.entries.get(chatId)?.view;
  }

  /** 当前已投影的绑定代次；未绑定为 `null`，未知 Chat 为 `undefined`。 */
  revisionOf(chatId: string): number | null | undefined {
    const entry = this.entries.get(chatId);
    if (!entry) return undefined;
    return entry.view.binding?.bindingRevision ?? null;
  }

  /** 释放单个视图；不存在是幂等 no-op。失败时不重新登记。 */
  async dispose(chatId: string): Promise<void> {
    const entry = this.entries.get(chatId);
    if (!entry) return;
    this.entries.delete(chatId);
    await entry.view.dispose();
  }

  /** 关闭所有连接并销毁文档；聚合失败，保留已释放的部分。 */
  async disposeAll(): Promise<void> {
    const entries = [...this.entries.values()];
    this.entries.clear();
    const results = await Promise.allSettled(
      entries.map((entry) => entry.view.dispose()),
    );
    const failures = results.flatMap((result) =>
      result.status === "rejected" ? [result.reason] : []);
    if (failures.length > 0) {
      throw new AggregateError(failures, "Chat view disposal is incomplete");
    }
  }

  get size(): number {
    return this.entries.size;
  }
}
