/**
 * 演示服务：把 ChatManager / ChatView 装配成 HTTP + SSE。
 *
 * 一次 Chat 句柄对应一个 Session；每个 SSE 连接通过 ChatYjsChannel 接收
 * ChatView 的 Yjs 同步帧。业务动作仍走 ChatService，不直接改 Y.Doc。
 */
import { Hono } from "hono";
import { streamSSE, type SSEStreamingApi } from "hono/streaming";
import { logger } from "hono/logger";
import {
    ChatManager,
    ChatUserAction,
    ChatView,
    PayloadReadError,
    parseInteractionAnswer,
    type ChatAgentConfig,
    type ChatDatabase,
    type ChatService,
    type ChatYjsChannel,
} from "../../src/entries/index";
import { DemoSessionNotFoundError } from "./demo-session-not-found-error";

const MAX_QUEUED_SYNC_BYTES = 4 * 1024 * 1024;

function encodeFrame(frame: Uint8Array): string {
    let binary = "";
    for (const byte of frame) binary += String.fromCharCode(byte);
    return btoa(binary);
}

function decodeFrame(encoded: string): Uint8Array {
    const binary = atob(encoded);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
    return bytes;
}

/** SSE 上的双向 Yjs 同步通道；客户端回帧走 POST `/api/session/sync`。 */
class SseChannel implements ChatYjsChannel {
    closed = false;
    readonly done: Promise<void>;
    private resolveDone!: () => void;
    private readonly messages = new Set<(frame: Uint8Array) => void>();
    private readonly closeListeners = new Set<(error?: Error) => void>();
    private pending: Uint8Array[] = [];
    private queuedBytes = 0;
    private writing: Promise<void> = Promise.resolve();

    constructor(private readonly stream: SSEStreamingApi) {
        this.done = new Promise<void>((resolve) => { this.resolveDone = resolve; });
    }

    send(frame: Uint8Array): Promise<void> {
        return this.write("sync", encodeFrame(frame), frame.byteLength);
    }

    onMessage(listener: (frame: Uint8Array) => void): () => void {
        this.messages.add(listener);
        const pending = this.pending;
        this.pending = [];
        for (const frame of pending) listener(frame);
        return () => this.messages.delete(listener);
    }

    onClose(listener: (error?: Error) => void): () => void {
        this.closeListeners.add(listener);
        return () => this.closeListeners.delete(listener);
    }

    /** 客户端回帧；ChatView 尚未挂监听时先排队。 */
    deliver(frame: Uint8Array): void {
        if (this.closed) return;
        if (this.messages.size === 0) {
            this.pending.push(frame);
            return;
        }
        for (const listener of [...this.messages]) listener(frame);
    }

    write(event: string, data: string, bytes = data.length * 2): Promise<void> {
        if (this.closed) return Promise.reject(new Error("SSE channel is closed"));
        if (this.queuedBytes + bytes > MAX_QUEUED_SYNC_BYTES) {
            this.fail(new Error("SSE sync buffer exceeded; reconnect to resume documents"));
            return Promise.reject(new Error("SSE sync buffer exceeded"));
        }
        this.queuedBytes += bytes;
        const pending = this.writing.then(() => this.stream.writeSSE({ event, data }))
            .finally(() => { this.queuedBytes -= bytes; });
        this.writing = pending.catch(() => {});
        return pending;
    }

    fail(error: Error): void {
        if (this.closed) return;
        this.closed = true;
        for (const listener of [...this.closeListeners]) listener(error);
        this.closeListeners.clear();
        this.messages.clear();
        this.resolveDone();
    }

    close(): void {
        if (this.closed) return;
        this.closed = true;
        for (const listener of [...this.closeListeners]) listener();
        this.closeListeners.clear();
        this.messages.clear();
        this.resolveDone();
    }
}

export interface DemoAppOptions {
    manager: ChatManager;
    database: ChatDatabase;
    workspace: string;
    /** 每个 Chat 的运行配置；store 由 Manager 注入。 */
    agent: ChatAgentConfig;
    workspaceMcpUrl?: string;
    html: string;
    sseClientJs: string;
    viewBundleJs: string;
}

interface OpenSession {
    chat: ChatService;
    view: ChatView;
}

export function createDemoApp(options: DemoAppOptions): {
    app: Hono;
    close(): Promise<void>;
} {
    const { manager, database, workspace, agent } = options;
    const sessions = new Map<string, Promise<OpenSession>>();
    const creationRequests = new Map<string, Promise<OpenSession>>();
    const connections = new Map<string, SseChannel>();
    const opened = new Set<OpenSession>();

    async function openSession(sessionId: string | null): Promise<OpenSession> {
        if (sessionId !== null) {
            const existing = sessions.get(sessionId);
            if (existing) return existing;
        }
        const opening = (async (): Promise<OpenSession> => {
            if (sessionId !== null && !(await database.getSession(sessionId)))
                throw new DemoSessionNotFoundError();
            const chat = manager.createChat({ path: workspace, agent });
            const view = new ChatView(chat);
            try {
                const ready = sessionId === null
                    ? await chat.execute(ChatUserAction.createNewSession({}))
                    : await chat.execute(ChatUserAction.changeSessionTo(sessionId));
                const openedSession = { chat, view };
                opened.add(openedSession);
                sessions.set(ready.sessionId, Promise.resolve(openedSession));
                return openedSession;
            } catch (error) {
                await view.dispose();
                await chat.close().catch(() => {});
                throw error;
            }
        })();
        if (sessionId !== null) sessions.set(sessionId, opening);
        void opening.catch(() => {
            if (sessionId !== null && sessions.get(sessionId) === opening) sessions.delete(sessionId);
        });
        return opening;
    }

    const app = new Hono();
    app.use("*", logger());
    app.onError((error, c) => {
        console.error("Demo request failed", error);
        if (error instanceof DemoSessionNotFoundError) return c.json({ error: error.message }, 404);
        if (error instanceof PayloadReadError) {
            return c.json({ error: error.message, code: error.code },
                error.code === "unavailable" ? 404 : error.code === "too_large" ? 413 : 409);
        }
        return c.json({ error: error instanceof Error ? error.message : String(error) }, 500);
    });
    app.get("/", (c) => {
        c.header("Cache-Control", "no-store");
        return c.html(options.html);
    });
    app.get("/vendor/fetch-event-source.js", (c) =>
        c.body(options.sseClientJs, 200, { "Content-Type": "text/javascript" }));
    app.get("/vendor/peri-session-view.js", (c) =>
        c.body(options.viewBundleJs, 200, { "Content-Type": "text/javascript" }));
    app.post("/api/session/list", async (c) => {
        const entries = await manager.listSessions({ path: workspace });
        return c.json({
            sessions: entries.map((entry) => ({
                id: entry.sessionId,
                title: entry.title,
                cwd: entry.cwd,
                messageCount: entry.messageCount,
                createdAt: entry.createdAt,
                updatedAt: entry.updatedAt,
            })),
        });
    });
    app.post("/api/session/create", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body !== "object" || Array.isArray(body))
            return c.json({ error: "JSON object is required" }, 400);
        const sessionId = body.sessionId ?? null;
        const requestId = body.requestId;
        if (sessionId !== null && (typeof sessionId !== "string" || !sessionId))
            return c.json({ error: "sessionId must be a nonempty string or null" }, 400);
        if (requestId !== undefined && (typeof requestId !== "string" || !requestId))
            return c.json({ error: "requestId must be a nonempty string" }, 400);

        let opening: Promise<OpenSession>;
        if (sessionId === null && typeof requestId === "string") {
            opening = creationRequests.get(requestId) ?? openSession(null);
            creationRequests.set(requestId, opening);
            void opening.catch(() => {
                if (creationRequests.get(requestId) === opening) creationRequests.delete(requestId);
            });
        } else {
            opening = openSession(typeof sessionId === "string" ? sessionId : null);
        }
        const selected = await opening;
        const connectionId = crypto.randomUUID();

        return streamSSE(c, async (stream) => {
            const channel = new SseChannel(stream);
            connections.set(connectionId, channel);
            let sync: { close(): Promise<void> } | undefined;
            let unsubscribeFacts: (() => void) | undefined;
            let heartbeat: ReturnType<typeof setInterval> | undefined;
            const stop = (): void => {
                if (heartbeat) clearInterval(heartbeat);
                unsubscribeFacts?.();
                connections.delete(connectionId);
                channel.close();
                void sync?.close().catch(() => {});
                stream.abort();
            };
            stream.onAbort(stop);
            c.req.raw.signal.addEventListener("abort", stop, { once: true });
            try {
                await stream.writeSSE({
                    event: "session",
                    data: JSON.stringify({
                        chatId: selected.chat.id,
                        sessionId: selected.chat.sessionId,
                        connectionId,
                        workspace,
                        workspaceMcp: options.workspaceMcpUrl,
                    }),
                });
                sync = selected.view.sync(channel);
                // 诊断面板：事实是只读输出，不参与动作。
                unsubscribeFacts = selected.chat.subscribeFacts({
                    onBinding() {},
                    onFact(envelope) {
                        void channel.write("fact", JSON.stringify(envelope)).catch((error) => {
                            channel.fail(error instanceof Error ? error : new Error(String(error)));
                        });
                    },
                    onClosed() {},
                });
                heartbeat = setInterval(() => {
                    void channel.write("ping", "{}", 0).catch(() => {});
                }, 15_000);
                await channel.done;
            } finally {
                if (heartbeat) clearInterval(heartbeat);
                unsubscribeFacts?.();
                c.req.raw.signal.removeEventListener("abort", stop);
                connections.delete(connectionId);
                channel.close();
                await sync?.close().catch(() => {});
            }
        });
    });
    app.post("/api/session/sync", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body.connectionId !== "string" || typeof body.frame !== "string")
            return c.json({ error: "connectionId and frame are required" }, 400);
        const channel = connections.get(body.connectionId);
        if (!channel) return c.json({ error: "Sync connection is not active" }, 404);
        try {
            channel.deliver(decodeFrame(body.frame));
        } catch {
            return c.json({ error: "frame must be base64" }, 400);
        }
        return c.json({ accepted: true });
    });
    app.post("/api/session/payload", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body.sessionId !== "string" || typeof body.id !== "string"
            || typeof body.bytes !== "number" || !Number.isSafeInteger(body.bytes))
            return c.json({ error: "sessionId, payload id and bytes are required" }, 400);
        const selected = await sessions.get(body.sessionId);
        if (!selected) return c.json({ error: "Session is not active" }, 404);
        const payload = await selected.chat.readPayload({ id: body.id, bytes: body.bytes });
        c.header("Cache-Control", "no-store");
        return c.body(payload.json, 200, { "Content-Type": "application/json; charset=utf-8" });
    });
    app.post("/api/session/send", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body !== "object" || Array.isArray(body))
            return c.json({ error: "JSON object is required" }, 400);
        const { sessionId, text } = body;
        if (typeof sessionId !== "string" || !sessionId || typeof text !== "string" || !text)
            return c.json({ error: "sessionId and text must be nonempty strings" }, 400);
        const selected = await sessions.get(sessionId);
        if (!selected) return c.json({ error: "Session is not active" }, 404);
        const receipt = selected.chat.execute(ChatUserAction.send({ text }));
        await receipt;
        return c.json({ inputId: receipt.inputId, delivered: receipt.isSent }, 202);
    });
    app.post("/api/session/interaction/respond", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        const sessionId = body?.sessionId;
        const interactionId = body?.interactionId;
        const answer = parseInteractionAnswer(body);
        if (typeof sessionId !== "string" || !sessionId || typeof interactionId !== "string" || !interactionId || !answer)
            return c.json({ error: "Invalid interaction response" }, 400);
        const selected = await sessions.get(sessionId);
        if (!selected) return c.json({ error: "Session is not active" }, 404);
        await selected.chat.execute(ChatUserAction.answer({ interactionId, answer }));
        return c.json({ accepted: true });
    });

    return {
        app,
        async close() {
            for (const channel of connections.values()) channel.close();
            connections.clear();
            const failures: unknown[] = [];
            for (const session of opened) {
                try {
                    await session.view.dispose();
                    await session.chat.close();
                } catch (error) {
                    failures.push(error);
                }
            }
            opened.clear();
            sessions.clear();
            creationRequests.clear();
            try { await manager.shutdown(); } catch (error) { failures.push(error); }
            if (failures.length) throw new AggregateError(failures, "Demo chat cleanup is incomplete");
        },
    };
}
