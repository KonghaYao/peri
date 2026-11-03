import type { ChatFactSource } from "../contracts/chat";
import { ChatView, type ChatViewOptions } from "./chat-view";

/** 注册表只需要事实源与稳定身份；不依赖具体 Chat 句柄实现。 */
type ChatFactSourceIdentity = ChatFactSource & { readonly id: string };

interface ViewEntry {
    readonly chat: ChatFactSourceIdentity;
    readonly view: ChatView;
}

/**
 * 每个 Chat 一个 `ChatView`，同 Chat 的所有同步连接共享这个视图。
 *
 * - 连接数由 `ChatView` 自己管理；这里不因最后一个连接断开而释放视图。
 * - 视图按事实源重放，绑定换代由 `ChatView` 成对替换文档。共享单 View 是
 *   可选的宿主策略，不是 SDK 不变量；宿主也可以为每条连接各建 View。
 * - 关闭 Chat 或进程退出时才 `dispose`；释放失败保留条目供重试。
 */
export class ChatViewRegistry {
    private readonly entries = new Map<string, ViewEntry>();
    private readonly options: ChatViewOptions;

    constructor(options: ChatViewOptions = {}) {
        this.options = options;
    }

    /** 返回同 Chat 的既有视图；不存在时同步挂载。 */
    attach(chat: ChatFactSourceIdentity): ChatView {
        const existing = this.entries.get(chat.id);
        if (existing) {
            if (existing.chat !== chat) {
                throw new Error(
                    `Chat ${chat.id} already has a view from a different chat instance`,
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

    /** 释放单个视图；不存在是幂等 no-op。只有释放成功才移除条目，失败保留供重试。 */
    async dispose(chatId: string): Promise<void> {
        const entry = this.entries.get(chatId);
        if (!entry) return;
        await entry.view.dispose();
        if (this.entries.get(chatId) === entry) this.entries.delete(chatId);
    }

    /** 关闭所有连接并销毁文档；聚合失败，失败的条目保留供重试。 */
    async disposeAll(): Promise<void> {
        const entries = [...this.entries.entries()];
        const results = await Promise.allSettled(
            entries.map(async ([chatId, entry]) => {
                await entry.view.dispose();
                if (this.entries.get(chatId) === entry) this.entries.delete(chatId);
            }),
        );
        const failures = results.flatMap((result) =>
            result.status === "rejected" ? [result.reason] : []);
        if (failures.length > 0) {
            throw new AggregateError(failures, "Chat view disposal is incomplete");
        }
    }
}
