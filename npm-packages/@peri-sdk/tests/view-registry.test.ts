import { describe, expect, test } from "bun:test";
import { ChatReplica } from "../src/view/chat-replica";
import type { ChatFactObserver, ChatFactSource } from "../src/contracts/chat";
import type { ChatBindingTarget } from "../src/contracts/chat";
import { ChatViewRegistry } from "../src/view/view-registry";
import {
    FakeFactSource,
    TestChannel,
    channelPair,
    update as notificationUpdate,
    waitFor,
} from "./chat-view-fixtures";

const TARGET: ChatBindingTarget = {
    chatId: "chat-1",
    sessionId: "session-1",
    bindingRevision: 1,
};

function update(kind: string, fields: Record<string, unknown>) {
    return notificationUpdate("session-1", kind, fields);
}

class FlakyCloseChannel extends TestChannel {
    failures = 1;

    override close(reason?: Error): void {
        if (this.failures > 0) {
            this.failures -= 1;
            throw new Error("channel close failed");
        }
        super.close(reason);
    }
}

/** 注册表只需要身份与事实源；不需要构造完整 ChatService。 */
function fakeChat(
    id: string,
    source?: ChatFactSource,
): ChatFactSource & { readonly id: string } {
    return {
        id,
        get binding() { return source?.binding ?? null; },
        subscribeFacts: (observer: ChatFactObserver) =>
            source ? source.subscribeFacts(observer) : () => {},
    };
}

describe("ChatViewRegistry", () => {
    test("同 Chat 共享一个 View，多连接各自同步", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({
            kind: "history-batch",
            notifications: [
                update("user_message_chunk", { messageId: "u1", content: { type: "text", text: "Saved" } }),
            ],
        });
        const chat = fakeChat("chat-1", source);
        const registry = new ChatViewRegistry();
        const view = registry.attach(chat);
        expect(registry.attach(chat)).toBe(view);
        expect(source.observerCount).toBe(1);

        const first = new ChatReplica();
        const second = new ChatReplica();
        const firstPair = channelPair();
        const secondPair = channelPair();
        const firstConnection = view.sync(firstPair.server);
        const secondConnection = view.sync(secondPair.server);
        first.connect(firstPair.client);
        second.connect(secondPair.client);

        await waitFor(() => first.status === "synced" && second.status === "synced");
        expect(first.read().sessionId).toBe("session-1");
        expect(second.read().entries[0]?.blocks[0]).toMatchObject({ text: "Saved" });

        await firstConnection.close();
        expect(source.observerCount).toBe(1);
        expect(registry.get("chat-1")).toBe(view);

        source.emit({
            kind: "notice",
            notification: update("agent_message_chunk", {
                messageId: "a1", content: { type: "text", text: "Still live" },
            }),
        });
        await waitFor(() => {
            const block = second.read().entries[1]?.blocks[0];
            return block?.type === "text" && block.text === "Still live";
        });

        await secondConnection.close();
        expect(registry.get("chat-1")).toBe(view);
        expect(registry.get("chat-1")?.binding?.bindingRevision).toBe(1);
        await first.dispose();
        await second.dispose();
        await registry.disposeAll();
    });

    test("绑定换代复用同一 View，副本成对替换", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "input-delivered", inputId: "u1", text: "First session" });
        const chat = fakeChat("chat-1", source);
        const registry = new ChatViewRegistry();
        const view = registry.attach(chat);

        const replica = new ChatReplica();
        const pair = channelPair();
        const connection = view.sync(pair.server);
        replica.connect(pair.client);
        await waitFor(() => replica.status === "synced" && replica.read().entries.length === 2);

        source.bind({ ...TARGET, sessionId: "session-2", bindingRevision: 2 });
        source.emit({ kind: "input-delivered", inputId: "u2", text: "Second session" });

        await waitFor(() => replica.read().bindingRevision === 2
            && replica.read().sessionId === "session-2"
            && replica.read().entries.length === 2);
        expect(replica.read().entries[0]?.blocks[0]).toMatchObject({ text: "Second session" });
        expect(registry.attach(chat)).toBe(view);
        expect(registry.get("chat-1")?.binding?.bindingRevision).toBe(2);

        await connection.close();
        await replica.dispose();
        await registry.disposeAll();
    });

    test("连接断开不释放 View，显式 dispose 才释放", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        const chat = fakeChat("chat-1", source);
        const registry = new ChatViewRegistry();
        const view = registry.attach(chat);

        const pair = channelPair();
        const connection = view.sync(pair.server);
        pair.client.close();
        await waitFor(() => pair.server.closed);
        expect(registry.get("chat-1")).toBe(view);
        expect(source.observerCount).toBe(1);

        await connection.close();
        await registry.dispose("chat-1");
        expect(registry.get("chat-1")).toBeUndefined();
        expect(source.observerCount).toBe(0);
    });

    test("dispose 失败保留条目，重试成功后才删除并关闭 channel", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        const chat = fakeChat("chat-1", source);
        const registry = new ChatViewRegistry();
        const view = registry.attach(chat);
        const channel = new FlakyCloseChannel();
        view.sync(channel);

        await expect(registry.dispose("chat-1")).rejects.toBeInstanceOf(
            AggregateError,
        );
        expect(registry.get("chat-1")).toBe(view);
        expect(source.observerCount).toBe(0);
        expect(channel.closed).toBe(false);

        await registry.dispose("chat-1");
        expect(registry.get("chat-1")).toBeUndefined();
        expect(channel.closed).toBe(true);
    });

    test("disposeAll 聚合失败并保留失败条目，重试成功后清空", async () => {
        const failing = new FakeFactSource();
        failing.bind(TARGET);
        const healthy = new FakeFactSource();
        healthy.bind({ ...TARGET, chatId: "chat-2" });
        const failingChat = fakeChat("chat-1", failing);
        const healthyChat = fakeChat("chat-2", healthy);
        const registry = new ChatViewRegistry();
        const failingView = registry.attach(failingChat);
        registry.attach(healthyChat);
        const channel = new FlakyCloseChannel();
        failingView.sync(channel);

        const failure = await registry.disposeAll().catch((error: unknown) => error);
        expect(failure).toBeInstanceOf(AggregateError);
        expect((failure as AggregateError).errors).toHaveLength(1);
        expect(registry.get("chat-1")).toBe(failingView);
        expect(registry.get("chat-2")).toBeUndefined();
        expect(channel.closed).toBe(false);

        await registry.disposeAll();
        expect(registry.get("chat-1")).toBeUndefined();
        expect(channel.closed).toBe(true);
    });

    test("同一 Chat id 的不同事实源实例被拒绝", () => {
        const registry = new ChatViewRegistry();
        registry.attach(fakeChat("chat-1"));
        expect(() => registry.attach(fakeChat("chat-1"))).toThrow("different chat instance");
    });
});
