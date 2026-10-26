import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { ChatDocument } from "../src/view/chat-document";
import { ChatReplica } from "../src/view/chat-replica";
import type { ChatYjsChannel } from "../src/view/channel";
import { ChatSyncConnection } from "../src/view/chat-sync";
import {
    encodeSyncFrame, SessionDocSync, type DocSnapshot, type DocUpdate,
} from "../src/sync/index";

const TARGET = { chatId: "chat-1", sessionId: "s1", bindingRevision: 1 };

function update(kind: string, fields: Record<string, unknown>) {
    return { jsonrpc: "2.0" as const, method: "session/update", params: {
        sessionId: "s1", update: { sessionUpdate: kind, ...fields },
    } };
}

class TestChannel implements ChatYjsChannel {
    peer: TestChannel | null = null;
    closed = false;
    private readonly messages = new Set<(frame: Uint8Array) => void>();
    private readonly closes = new Set<(error?: Error) => void>();

    send(frame: Uint8Array): void {
        if (this.closed) throw new Error("channel is closed");
        const peer = this.peer;
        if (!peer || peer.closed) throw new Error("peer is closed");
        for (const listener of [...peer.messages]) listener(new Uint8Array(frame));
    }
    onMessage(listener: (frame: Uint8Array) => void): () => void {
        this.messages.add(listener);
        return () => this.messages.delete(listener);
    }
    onClose(listener: (error?: Error) => void): () => void {
        this.closes.add(listener);
        return () => this.closes.delete(listener);
    }
    close(reason?: Error): void {
        if (this.closed) return;
        this.closed = true;
        for (const listener of [...this.closes]) listener(reason);
        if (this.peer && !this.peer.closed) this.peer.close(reason);
    }
}

function pair() {
    const server = new TestChannel();
    const client = new TestChannel();
    server.peer = client;
    client.peer = server;
    return { server, client };
}

function serverConnection(document: ChatDocument) {
    const sync = new SessionDocSync(document.chat, document.session);
    const channels = pair();
    const connection = new ChatSyncConnection(() => sync, channels.server, {
        closeTimeoutMs: 200, ackTimeoutMs: 500,
    });
    return { sync, connection, ...channels };
}

function fullSnapshot(document: ChatDocument, generation = "generation-1"): DocSnapshot {
    return {
        protocol: 2, generation, sequence: 0, mode: "snapshot",
        chat: Y.encodeStateAsUpdateV2(document.chat),
        session: Y.encodeStateAsUpdateV2(document.session),
    };
}

async function waitFor(check: () => boolean, timeoutMs = 1500): Promise<void> {
    const started = Date.now();
    while (!check()) {
        if (Date.now() - started > timeoutMs) throw new Error("condition timed out");
        await new Promise((resolve) => setTimeout(resolve, 5));
    }
}

describe("ChatReplica", () => {
    test("receives full then incremental updates and exposes the session view", async () => {
        const document = new ChatDocument({ binding: TARGET });
        document.applyFact({ kind: "input-delivered", inputId: "u1", text: "Read" });
        const { sync, connection, client } = serverConnection(document);
        const replica = new ChatReplica();
        expect(replica.status).toBe("connecting");
        const replicaConnection = replica.connect(client);
        await waitFor(() => replica.status === "synced");
        expect(replica.read().readiness).toBe("ready");
        expect(replica.read().entries).toHaveLength(2);

        document.accept(update("agent_message_chunk", { messageId: "a1", content: { type: "text", text: "Done" } }));
        sync.flush();
        await waitFor(() => replica.read().entries[1]?.blocks[0]?.type === "text");
        const view = replica.read();
        expect(view.entries[1]?.blocks[0]).toMatchObject({ text: "Done" });
        expect(view.entries[1]?.entryId).toMatch(/:assistant$/);
        expect(view.pendingInteractions).toEqual([]);

        await replicaConnection.close();
        expect(replica.status).toBe("failed");
        await connection.close();
        await replica.dispose();
    });

    test("HELLO resume replays offline changes and keeps prior entries", async () => {
        const document = new ChatDocument({ binding: TARGET });
        document.applyFact({ kind: "input-delivered", inputId: "u1", text: "Online" });
        const sync = new SessionDocSync(document.chat, document.session);
        const firstChannels = pair();
        const firstConnection = new ChatSyncConnection(() => sync, firstChannels.server, {
            closeTimeoutMs: 200, ackTimeoutMs: 500,
        });
        const replica = new ChatReplica();
        const firstReplica = replica.connect(firstChannels.client);
        await waitFor(() => replica.status === "synced");
        await firstReplica.close();
        await firstConnection.close();

        document.applyFact({ kind: "input-delivered", inputId: "u2", text: "Offline" });
        document.accept(update("agent_message_chunk", { messageId: "a2", content: { type: "text", text: "After resume" } }));

        const secondChannels = pair();
        const secondConnection = new ChatSyncConnection(() => sync, secondChannels.server, {
            closeTimeoutMs: 200, ackTimeoutMs: 500,
        });
        const secondReplica = replica.connect(secondChannels.client);
        await waitFor(() => replica.status === "synced" && replica.read().entries.length === 4);
        const view = replica.read();
        expect(view.entries.map((entry) => entry.role)).toEqual(["user", "assistant", "user", "assistant"]);
        expect(view.entries[2]?.blocks[0]).toMatchObject({ text: "Offline" });
        expect(view.entries[3]?.blocks[0]).toMatchObject({ text: "After resume", type: "text" });

        await secondReplica.close();
        await secondConnection.close();
        await replica.dispose();
        await sync.close();
    });

    test("a sequence gap fails the replica and a fresh snapshot repairs it", async () => {
        const document = new ChatDocument({ binding: TARGET });
        document.applyFact({ kind: "input-delivered", inputId: "u1", text: "Start" });
        const sync = new SessionDocSync(document.chat, document.session);
        const frames: DocUpdate[] = [];
        const subscription = sync.subscribe((frame) => { frames.push(frame); });
        const channels = pair();
        const replica = new ChatReplica();
        const replicaConnection = replica.connect(channels.client);
        channels.server.send(encodeSyncFrame({
            type: "snapshot", delivery: 1, snapshot: subscription.snapshot,
        }));
        await waitFor(() => replica.status === "synced");

        document.accept(update("agent_message_chunk", { messageId: "a1", content: { type: "text", text: "one" } }));
        sync.flush();
        document.accept(update("agent_message_chunk", { messageId: "a1", content: { type: "text", text: "two" } }));
        sync.flush();
        expect(frames).toHaveLength(2);
        // 跳过头一帧：sequence 2 到达时副本仍停在 0。
        channels.server.send(encodeSyncFrame({ type: "update", delivery: 4, update: frames[1]! }));
        await waitFor(() => replica.status === "failed");

        await replicaConnection.close();
        const repair = pair();
        const repaired = replica.connect(repair.client);
        repair.server.send(encodeSyncFrame({
            type: "snapshot", delivery: 1, snapshot: fullSnapshot(document, "generation-1"),
        }));
        await waitFor(() => replica.status === "synced" && replica.read().entries[1]?.blocks[0]?.type === "text");
        expect(replica.read().entries[1]?.blocks[0]).toMatchObject({ text: "onetwo" });
        await repaired.close();
        await replica.dispose();
        subscription.unsubscribe();
        await sync.close();
    });

    test("generation mismatch fails the connection instead of writing mixed documents", async () => {
        const document = new ChatDocument({ binding: TARGET });
        document.applyFact({ kind: "input-delivered", inputId: "u1", text: "Start" });
        const channels = pair();
        const replica = new ChatReplica();
        const connection = replica.connect(channels.client);
        channels.server.send(encodeSyncFrame({
            type: "snapshot", delivery: 1, snapshot: fullSnapshot(document, "generation-a"),
        }));
        await waitFor(() => replica.status === "synced");

        const other = new Y.Doc();
        other.getText("x").insert(0, "other");
        channels.server.send(encodeSyncFrame({
            type: "update", delivery: 2,
            update: {
                protocol: 2, generation: "generation-b", sequence: 1,
                chat: Y.encodeStateAsUpdateV2(other),
            },
        }));
        await waitFor(() => replica.status === "failed");
        expect(replica.read().entries[0]?.blocks[0]).toMatchObject({ text: "Start" });
        await connection.close();
        await replica.dispose();
        other.destroy();
    });

    test("unsupported schema is reported as unsupported, never as empty history", async () => {
        const chat = new Y.Doc();
        const session = new Y.Doc();
        chat.getMap("root").set("schemaVersion", 99);
        session.getMap("root").set("schemaVersion", 99);
        const channels = pair();
        const replica = new ChatReplica();
        const connection = replica.connect(channels.client);
        channels.server.send(encodeSyncFrame({
            type: "snapshot", delivery: 1,
            snapshot: {
                protocol: 2, generation: "generation-1", sequence: 0, mode: "snapshot",
                chat: Y.encodeStateAsUpdateV2(chat), session: Y.encodeStateAsUpdateV2(session),
            },
        }));
        await waitFor(() => replica.status === "synced");
        const view = replica.read();
        expect(view.readiness).toBe("unsupported");
        expect(view.schemaVersions).toEqual({ chat: 99, session: 99 });
        expect(view.entries).toEqual([]);
        await connection.close();
        replica.dispose();
        chat.destroy();
        session.destroy();
    });

    test("tool reference updates invalidate only the affected block", async () => {
        const document = new ChatDocument({ binding: TARGET });
        document.applyFact({ kind: "input-delivered", inputId: "u1", text: "Read" });
        document.accept(update("tool_call", { toolCallId: "first", status: "pending" }));
        document.accept(update("tool_call", { toolCallId: "second", status: "pending" }));
        const { sync, connection, client } = serverConnection(document);
        const replica = new ChatReplica();
        const replicaConnection = replica.connect(client);
        await waitFor(() => replica.status === "synced" && replica.read().entries[1]?.blocks.length === 2);
        const before = replica.read().entries[1]!.blocks;
        document.accept(update("tool_call_update", { toolCallId: "first", status: "completed", result: { contentText: "ok" } }));
        sync.flush();
        await waitFor(() => replica.read().entries[1]?.blocks[0]?.type === "tool_call"
            && replica.read().entries[1]?.blocks[0]?.tool?.status === "completed");
        const after = replica.read().entries[1]!.blocks;
        expect(after[1]).toBe(before[1]);
        expect(after[0]).not.toBe(before[0]);
        await replicaConnection.close();
        await connection.close();
        await replica.dispose();
    });

    test("read coalesces transactions and reuses unchanged entry identities", async () => {
        const document = new ChatDocument({ binding: TARGET });
        document.applyFact({ kind: "input-delivered", inputId: "u1", text: "Stable" });
        const { sync, connection, client } = serverConnection(document);
        const replica = new ChatReplica();
        const replicaConnection = replica.connect(client);
        await waitFor(() => replica.status === "synced");
        let notifications = 0;
        replica.subscribe(() => notifications++);
        const before = replica.read();
        expect(replica.read()).toBe(before);

        document.applyFact({ kind: "run-state", running: true, executionBlocked: false });
        sync.flush();
        await waitFor(() => replica.read().running);
        const current = replica.read();
        expect(current.entries).toBe(before.entries);
        expect(current.entries[0]).toBe(before.entries[0]);
        expect(notifications).toBe(1);

        await replicaConnection.close();
        await connection.close();
        await replica.dispose();
    });
});
