/** Bun HTTP demo with one Peri Agent per Session. */
import { readFile, realpath } from "node:fs/promises";
import { resolve } from "node:path";
import { Hono } from "hono";
import { streamSSE } from "hono/streaming";
import { logger } from "hono/logger";
import {
    BareHarnessConfig,
    ManagedAgents,
    MemoryKV,
    Sandbox,
    TursoStorage,
    type Agent,
    type PeriConfig,
    type AgentOptions,
    type SessionStorage,
} from "../../src/sdk/index";
import { DemoSessionNotFoundError } from "./demo-session-not-found-error";
import { SessionEventLog } from "./session-event-log";

const workspace = await realpath(Bun.env.PERI_WORKSPACE!);
const html = await readFile(resolve(import.meta.dir, "demo.html"), "utf8");
const databaseUrl = Bun.env.PERI_DEMO_TURSO_URL ?? "http://127.0.0.1:8081";
const databaseEndpoint = new URL(databaseUrl);
if (databaseEndpoint.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(databaseEndpoint.hostname))
    throw new Error("Demo Session Store must be a local HTTP libSQL listener");
const sseClient = await Bun.build({
    entrypoints: [resolve(import.meta.dir, "../../node_modules/@microsoft/fetch-event-source/lib/esm/index.js")],
    target: "browser",
    format: "esm",
});
if (!sseClient.success) throw new Error(`Cannot bundle SSE client: ${sseClient.logs.join("; ")}`);
const sseClientJs = await sseClient.outputs[0]!.text();

const storage: SessionStorage = new TursoStorage({
    url: databaseUrl,
    engine: "libsql",
    // Peri currently requires a nonempty remote credential even for local sqld without auth.
    authToken: "local-dev",
});
const config = {
    config: {
        active_alias: "sonnet",
        providers: [
            {
                id: "anthropic",
                type: "anthropic",
                apiKey: Bun.env.ANTHROPIC_API_KEY!,
                baseUrl: Bun.env.ANTHROPIC_BASE_URL!,
                name: "anthropic",
                models: {
                    opus: "deepseek-v4-flash",
                    sonnet: "deepseek-v4-flash",
                    haiku: "deepseek-v4-flash",
                    fable: "deepseek-v4-flash",
                },
            },
        ],
        profiles: {
            fable: {
                provider: "anthropic",
                model: "deepseek-v4-flash",
            },
            opus: {
                provider: "anthropic",
                model: "deepseek-v4-flash",
            },
            sonnet: {
                provider: "anthropic",
                model: "deepseek-v4-flash",
            },
            haiku: {
                provider: "anthropic",
                model: "deepseek-v4-flash",
            },
        },
        meta_harness: {
            ...BareHarnessConfig,
            McpMiddleware: true,
            ToolSearch: true,
            WorkspaceMiddleware: false,
        },
    },
} satisfies PeriConfig;
const sandbox = new Sandbox({
    id: "demo-workspace",
    path: workspace,
    storage,
    stdio: { command: Bun.env.PERI_BIN ?? "peri", settings: config },
    workspaceProcess: {
        command: Bun.env.PERI_BIN ?? "peri",
        bind: Bun.env.PERI_WORKSPACE_BIND ?? "127.0.0.1:8765",
    },
});
const manager = new ManagedAgents({ kv: new MemoryKV() });
const agentOptions: Pick<AgentOptions, "instructions" | "mcpServers"> = {
    instructions: "你是 Peri。请用清晰、简洁的中文帮助用户完成任务。",
    mcpServers: {
        demo: {
            type: "http",
            url: "https://demo-day.mcp.cloudflare.com/mcp",
        },
    },
};
type OpenSession = { agent: Agent; events: SessionEventLog };
const sessions = new Map<string, Promise<OpenSession>>();
const creationRequests = new Map<string, Promise<OpenSession>>();

function openSession(sessionId: string | null): Promise<OpenSession> {
    if (sessionId !== null) {
        const existing = sessions.get(sessionId);
        if (existing) return existing;
    }
    const opening = (async () => {
        if (sessionId !== null && !(await sandbox.getSessions()).some((entry) => entry.id === sessionId))
            throw new DemoSessionNotFoundError();
        const agent = manager.createAgent({
            id: sessionId === null ? `demo-agent-${crypto.randomUUID()}` : `demo-session-${sessionId}`,
            sandbox,
            ...agentOptions,
        });
        try {
            const session = await agent.session.start(sessionId);
            const events = new SessionEventLog();
            void (async () => {
                for await (const notification of session.stream()) events.append(notification);
            })().catch((error) => console.error("Demo Session event stream failed", error));
            return { agent, events };
        } catch (error) {
            await manager.closeAgent(agent.id);
            throw error;
        }
    })();
    if (sessionId !== null) sessions.set(sessionId, opening);
    void opening.then(
        ({ agent }) => sessions.set(agent.session.id, opening),
        () => { if (sessionId !== null && sessions.get(sessionId) === opening) sessions.delete(sessionId); },
    );
    return opening;
}

let server: ReturnType<typeof Bun.serve> | undefined;
try {
    try {
        await sandbox.getSessions();
    } catch {
        throw new Error("Local Session Store is unavailable; run `bun run db:dev` or check PERI_DEMO_TURSO_URL");
    }
    const workspaceMcp = await sandbox.startWorkspace();
    const app = new Hono();
    app.use("*", logger());
    app.onError((error, c) => {
        console.error("Demo request failed", error);
        return c.json({ error: error.message }, error instanceof DemoSessionNotFoundError ? 404 : 500);
    });
    app.get("/", (c) => {
        c.header("Cache-Control", "no-store");
        return c.html(html);
    });
    app.get("/vendor/fetch-event-source.js", (c) => c.body(sseClientJs, 200, { "Content-Type": "text/javascript" }));
    app.post("/api/session/list", async (c) => c.json({ sessions: await sandbox.getSessions() }));
    app.post("/api/session/create", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body !== "object" || Array.isArray(body))
            return c.json({ error: "JSON object is required" }, 400);
        const sessionId = body.sessionId ?? null;
        const after = body.after ?? 0;
        const requestId = body.requestId;
        if (sessionId !== null && (typeof sessionId !== "string" || !sessionId))
            return c.json({ error: "sessionId must be a nonempty string or null" }, 400);
        if (typeof after !== "number" || !Number.isSafeInteger(after) || after < 0)
            return c.json({ error: "after must be a nonnegative integer" }, 400);
        if (requestId !== undefined && (typeof requestId !== "string" || !requestId))
            return c.json({ error: "requestId must be a nonempty string" }, 400);
        let opening: Promise<OpenSession>;
        if (sessionId === null && typeof requestId === "string") {
            opening = creationRequests.get(requestId) ?? openSession(null);
            creationRequests.set(requestId, opening);
            void opening.catch(() => {
                if (creationRequests.get(requestId) === opening)
                    creationRequests.delete(requestId);
            });
        } else {
            opening = openSession(sessionId);
        }
        const selected = await opening;
        return streamSSE(c, async (stream) => {
            let stop!: () => void;
            const closed = new Promise<void>((resolve) => { stop = resolve; });
            stream.onAbort(stop);
            c.req.raw.signal.addEventListener("abort", stop, { once: true });
            await stream.writeSSE({
                event: "session",
                data: JSON.stringify({
                    agentId: selected.agent.id,
                    sessionId: selected.agent.session.id,
                    workspace,
                    workspaceMcp: workspaceMcp.url,
                }),
            });
            let writes = Promise.resolve();
            const write = (event: string, data: string, id?: string) => {
                writes = writes.then(() => stream.writeSSE({ event, data, id })).catch(stop);
            };
            const unsubscribe = selected.events.subscribe(after, (entry) =>
                write("notification", JSON.stringify(entry), String(entry.id)));
            const heartbeat = setInterval(() => write("ping", "{}"), 15_000);
            try {
                await closed;
            } finally {
                clearInterval(heartbeat);
                unsubscribe();
                await writes;
            }
        });
    });
    app.post("/api/session/send", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body !== "object" || Array.isArray(body))
            return c.json({ error: "JSON object is required" }, 400);
        const { sessionId, text } = body;
        if (typeof sessionId !== "string" || !sessionId || typeof text !== "string" || !text)
            return c.json({ error: "sessionId and text must be nonempty strings" }, 400);
        const selected = await openSession(sessionId);
        const receipt = selected.agent.session.send(text);
        await receipt;
        return c.json({ inputId: receipt.inputId, delivered: receipt.isSent }, 202);
    });

    const port = Number(Bun.env.PORT ?? "3000");
    server = Bun.serve({
        hostname: "127.0.0.1",
        port,
        fetch: app.fetch,
    });
    console.log(`Peri SDK demo: http://127.0.0.1:${server.port}`);
    await new Promise<void>((done) => {
        process.once("SIGINT", done);
        process.once("SIGTERM", done);
    });
} finally {
    await server?.stop(true);
    await Promise.allSettled(sessions.values());
    await manager.closeAll();
    await sandbox.closeWorkspace();
}
