import { describe, expect, test } from "bun:test";
import type { ChatBindingTarget } from "../src/chat/types";
import { ChatView } from "../src/view/chat-view";
import { ChatReplica } from "../src/view/chat-replica";
import { ChatSyncTimeoutError } from "../src/view/chat-sync";
import { SyncBackpressureError } from "../src/sync/index";
import {
    FakeFactSource,
    MuteChannel,
    channelPair,
    update as notificationUpdate,
    waitFor,
} from "./chat-view-fixtures";

const TARGET: ChatBindingTarget = { chatId: "chat-1", sessionId: "s1", bindingRevision: 1 };

function update(kind: string, fields: Record<string, unknown>) {
    return notificationUpdate("s1", kind, fields);
}

function connect(view: ChatView, replica = new ChatReplica()) {
    const { server, client } = channelPair();
    const connection = view.sync(server);
    replica.connect(client);
    return { replica, connection, server, client };
}

describe("ChatView", () => {
    test("a late View rebuilds output, queue, tasks and interactions from the fact log", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "seed-config", response: { configOptions: [], modes: { currentModeId: "code" } } });
        source.emit({ kind: "history-batch", notifications: [
            update("user_message_chunk", { messageId: "u1", content: { type: "text", text: "Saved" } }),
            update("agent_message_chunk", { messageId: "a1", content: { type: "text", text: "Answer" } }),
        ] });
        source.emit({ kind: "input-queue", snapshot: { generation: "g", revision: 2, items: [{ inputId: "i1", state: "queued" }] } });
        source.emit({ kind: "task-snapshot", snapshot: { revision: 1, tasks: [
            { task_id: "bg", status: "running", kind: "shell", summary: "Build" },
        ] } });
        source.emit({ kind: "interaction-began", record: {
            id: "interaction:1", kind: "permission", sessionId: "s1", toolCallId: "t1",
            title: "Read", options: [{ optionId: "allow_once", name: "Allow" }],
        } });
        source.emit({ kind: "run-state", running: true, executionBlocked: false });

        const view = new ChatView(source);
        const { replica, connection } = connect(view);
        await waitFor(() => replica.status === "synced" && replica.read().pendingInteractions.length === 1);

        const read = replica.read();
        expect(read.sessionId).toBe("s1");
        expect(read.bindingRevision).toBe(1);
        expect(read.entries.map((entry) => entry.role)).toEqual(["user", "assistant"]);
        expect(read.inputQueue).toMatchObject({ generation: "g", revision: 2 });
        expect(read.tasks[0]).toMatchObject({ taskId: "bg", title: "Build" });
        expect(read.pendingInteractions[0]).toMatchObject({ id: "interaction:1", kind: "permission" });
        expect(read.running).toBe(true);

        connection.close();
        await view.dispose();
        await replica.dispose();
    });

    test("two Views share facts, and disposing one leaves the other live", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "input-delivered", inputId: "u1", text: "Start" });
        const first = new ChatView(source);
        const second = new ChatView(source);
        const firstConnection = connect(first);
        const secondConnection = connect(second);
        await waitFor(() => firstConnection.replica.status === "synced" && secondConnection.replica.status === "synced");

        await first.dispose();
        firstConnection.replica.dispose();
        expect(source.observerCount).toBe(1);

        source.emit({ kind: "notice", notification: update("agent_message_chunk", {
            messageId: "a1", content: { type: "text", text: "Still live" },
        }) });
        await waitFor(() => secondConnection.replica.read().entries[1]?.blocks[0]?.type === "text"
            && secondConnection.replica.read().entries[1]?.blocks[0]?.text === "Still live");
        expect(secondConnection.replica.status).toBe("synced");
        expect(secondConnection.replica.read().entries[0]?.blocks[0]).toMatchObject({ text: "Start" });

        secondConnection.connection.close();
        await second.dispose();
        await secondConnection.replica.dispose();
    });

    test("two Views share a pending interaction; a late View rebuilds it and one dispose leaves the other live", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "input-delivered", inputId: "u1", text: "Start" });
        const early = new ChatView(source);
        const earlyConnection = connect(early);
        await waitFor(() => earlyConnection.replica.status === "synced");

        source.emit({ kind: "interaction-began", record: {
            id: "interaction:pending", kind: "permission", sessionId: "s1",
            toolCallId: "t1", title: "Write file",
            options: [{ optionId: "allow_once", name: "Allow" }],
        } });
        await waitFor(() => earlyConnection.replica.read().pendingInteractions.length === 1);

        const late = new ChatView(source);
        const lateConnection = connect(late);
        await waitFor(() => lateConnection.replica.status === "synced");
        expect(lateConnection.replica.read().pendingInteractions).toMatchObject([
            { id: "interaction:pending", kind: "permission" },
        ]);
        expect(earlyConnection.replica.read().pendingInteractions).toHaveLength(1);

        await early.dispose();
        earlyConnection.replica.dispose();
        expect(source.observerCount).toBe(1);
        expect(lateConnection.replica.status).toBe("synced");
        expect(lateConnection.replica.read().pendingInteractions).toHaveLength(1);

        source.emit({ kind: "notice", notification: update("agent_message_chunk", {
            messageId: "a1", content: { type: "text", text: "Still live" },
        }) });
        await waitFor(() => lateConnection.replica.read().entries[1]?.blocks[0]?.type === "text");
        expect(lateConnection.replica.read().pendingInteractions).toHaveLength(1);

        lateConnection.connection.close();
        await late.dispose();
        await lateConnection.replica.dispose();
    });

    test("binding generations replace the replica pair without reusing old entries", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "input-delivered", inputId: "u1", text: "First session" });
        const view = new ChatView(source);
        const { replica, connection } = connect(view);
        await waitFor(() => replica.status === "synced" && replica.read().entries.length === 2);

        source.bind({ ...TARGET, sessionId: "s2", bindingRevision: 2 });
        source.emit({ kind: "input-delivered", inputId: "u2", text: "Second session" });
        await waitFor(() => replica.read().bindingRevision === 2
            && replica.read().sessionId === "s2"
            && replica.read().entries.length === 2);
        const entries = replica.read().entries;
        expect(entries[0]?.blocks[0]).toMatchObject({ text: "Second session" });
        expect(entries[1]?.role).toBe("assistant");
        expect(JSON.stringify(entries)).not.toContain("First session");

        connection.close();
        await view.dispose();
        await replica.dispose();
    });

    test("sync waits for HELLO and flushes the terminal update before bounded close", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "input-delivered", inputId: "u1", text: "Hello" });
        const view = new ChatView(source, { sync: { closeTimeoutMs: 200 } });
        const { replica, connection } = connect(view);
        await waitFor(() => replica.status === "synced");
        source.emit({ kind: "notice", notification: update("agent_message_chunk", {
            messageId: "a1", content: { type: "text", text: "Tail" },
        }) });
        await waitFor(() => replica.read().entries[1]?.blocks[0]?.type === "text");
        connection.close();
        expect(replica.status).toBe("failed");
        expect(replica.read().entries[1]).toMatchObject({ blocks: [{ text: "Tail" }] });
        await view.dispose();
        await replica.dispose();
    });

    test("a mute client is closed for exceeding delivery credit", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        const view = new ChatView(source, { sync: { maxOutstandingFrames: 1, closeTimeoutMs: 100 } });
        const client = new MuteChannel();
        view.sync(client);
        client.hello();
        source.emit({ kind: "input-delivered", inputId: "u1", text: "one" });
        source.emit({ kind: "input-delivered", inputId: "u2", text: "two" });
        await waitFor(() => client.closed);
        expect(client.closeReason).toBeInstanceOf(SyncBackpressureError);
        await view.dispose();
    });

    test("a client that never acknowledges is closed by the delivery deadline", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        const view = new ChatView(source, { sync: { ackTimeoutMs: 20, closeTimeoutMs: 100 } });
        const client = new MuteChannel();
        view.sync(client);
        client.hello();
        source.emit({ kind: "input-delivered", inputId: "u1", text: "pending" });
        await waitFor(() => client.closed);
        expect(client.closeReason).toBeInstanceOf(ChatSyncTimeoutError);
        await view.dispose();
    });

    test("source closure becomes a terminal display fact without closing the sync connection", async () => {
        const source = new FakeFactSource();
        source.bind(TARGET);
        source.emit({ kind: "input-delivered", inputId: "u1", text: "Before close" });
        const view = new ChatView(source);
        const { replica, connection } = connect(view);
        await waitFor(() => replica.status === "synced");
        source.close();
        await waitFor(() => replica.read().closed);
        expect(replica.status).toBe("synced");
        expect(replica.read().running).toBe(false);
        connection.close();
        await view.dispose();
        await replica.dispose();
    });

    test("ChatView exposes no snapshot/subscribe facade", () => {
        const source = new FakeFactSource();
        const view = new ChatView(source);
        expect("getSnapshot" in view).toBe(false);
        expect("subscribe" in view).toBe(false);
        void view.dispose();
    });
});
