/** Bun HTTP demo with one Peri Agent per Session over the WASM ACP port. */
import { readFile, realpath } from "node:fs/promises";
import { resolve } from "node:path";
import { Hono } from "hono";
import { streamSSE } from "hono/streaming";
import { logger } from "hono/logger";
import {
    BareHarnessConfig,
    ManagedAgents,
    MemoryKV,
    InteractionResponder,
    parseInteractionAnswer,
    Sandbox,
    TursoStorage,
    WasmAcpTransport,
    loadPeriWasm,
    type Agent,
    type PeriConfig,
    type AgentOptions,
    type SessionStorage,
} from "../../src/sdk/index";
import { DemoSessionNotFoundError } from "./demo-session-not-found-error";
import { SessionDocStream, decodeResume } from "./session-doc-stream";
import { streamSessionDocuments } from "./session-sse";
import { SessionEventLog } from "./session-event-log";
import { shutdownCommands } from "./session-control-command";

const workspace = await realpath(Bun.env.PERI_WORKSPACE!);
const html = await readFile(resolve(import.meta.dir, "demo.html"), "utf8");
const wasmModuleUrl = Bun.env.PERI_WASM_MODULE_URL ?? resolve(import.meta.dir, "../../dist/wasm/peri-wasm.js");
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
const sessionView = await Bun.build({
    entrypoints: [resolve(import.meta.dir, "../../src/view/index.ts")],
    target: "browser",
    format: "esm",
});
if (!sessionView.success) throw new Error(`Cannot bundle Session view: ${sessionView.logs.join("; ")}`);
const sessionViewJs = await sessionView.outputs[0]!.text();

const storage: SessionStorage = new TursoStorage({
    url: databaseUrl,
    engine: "libsql",
    // Peri currently requires a nonempty remote credential even for local sqld without auth.
    authToken: "local-dev",
});
const wasmEnv = Object.fromEntries(
    Object.entries(Bun.env).filter((entry): entry is [string, string] =>
        entry[0].startsWith("LANGFUSE_") && entry[1] !== undefined),
);
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
    storage,
    ...(Bun.env.PERI_WORKSPACE_MCP_URL
        ? { workspace: { url: Bun.env.PERI_WORKSPACE_MCP_URL } }
        : {}),
    transportFactory: (path) => WasmAcpTransport.start({
        moduleUrl: wasmModuleUrl,
        env: wasmEnv,
        configJson: JSON.stringify({
            cwd: path,
            settings: config,
            storage: { url: databaseUrl, authToken: "local-dev" },
            machineId: Bun.env.PERI_WASM_MACHINE_ID ?? "00000000-0000-4000-8000-000000000001",
        }),
    }),
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
type OpenSession = { agent: Agent; docs: SessionDocStream; events: SessionEventLog; interactions: InteractionResponder };
const sessions = new Map<string, Promise<OpenSession>>();
const creationRequests = new Map<string, Promise<OpenSession>>();

function openSession(sessionId: string | null): Promise<OpenSession> {
    if (sessionId !== null) {
        const existing = sessions.get(sessionId);
        if (existing) return existing;
    }
    const opening = (async () => {
        if (sessionId !== null && !(await sandbox.getSession(sessionId)))
            throw new DemoSessionNotFoundError();
        const interactions = new InteractionResponder();
        const agent = manager.createAgent({
            id: sessionId === null ? `demo-agent-${crypto.randomUUID()}` : `demo-session-${sessionId}`,
            sandbox,
            ...(sessionId === null ? { path: workspace } : {}),
            ...agentOptions,
            onPermissionRequest: interactions.onPermissionRequest,
            onElicitation: interactions.onElicitation,
        });
        try {
            const session = await agent.session.start(sessionId);
            const events = new SessionEventLog();
            void (async () => {
                for await (const notification of session.stream()) events.append(notification);
            })().catch((error) => console.error("Demo Session event stream failed", error));
            return { agent, docs: new SessionDocStream(session.docs.chat, session.docs.session), events, interactions };
        } catch (error) {
            interactions.close();
            await manager.cleanupStartupAgent(agent.id);
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
    const wasm = await loadPeriWasm(wasmModuleUrl, wasmEnv);
    if (!wasm.PeriWasmAcp?.start)
        throw new Error("Bundled peri-wasm lacks ACP Host; rebuild peri-wasm before running demo-wasm");
    try {
        await sandbox.getSessions(workspace);
    } catch {
        throw new Error("Local Session Store is unavailable; run `bun run db:dev` or check PERI_DEMO_TURSO_URL");
    }
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
    app.get("/vendor/peri-session-view.js", (c) => c.body(sessionViewJs, 200, { "Content-Type": "text/javascript" }));
    app.post("/api/session/list", async (c) => c.json({ sessions: await sandbox.getSessions(workspace) }));
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
        const resume = decodeResume(body.resume);
        return streamSSE(c, async (stream) => {
            await stream.writeSSE({
                event: "session",
                data: JSON.stringify({
                    agentId: selected.agent.id,
                    sessionId: selected.agent.session.id,
                    workspace,
                    workspaceMcp: sandbox.optionalWorkspace?.url ?? "",
                }),
            });
            await streamSessionDocuments(stream, c.req.raw.signal, selected, {
                after, resume, diagnostics: body.diagnostics === true,
            });
        });
    });
    app.post("/api/session/payload", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        if (!body || typeof body.sessionId !== "string" || typeof body.id !== "string")
            return c.json({ error: "sessionId and payload id are required" }, 400);
        const selected = await sessions.get(body.sessionId);
        const payload = selected?.agent.docs.readPayload(body.id);
        if (payload === undefined) return c.json({ error: "Payload version is unavailable; refresh the tool card" }, 404);
        c.header("Cache-Control", "no-store");
        return c.body(payload, 200, { "Content-Type": "application/json; charset=utf-8" });
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
    app.post("/api/session/interaction/respond", async (c) => {
        const body = await c.req.json<Record<string, unknown>>().catch(() => null);
        const sessionId = body?.sessionId;
        const interactionId = body?.interactionId;
        const answer = parseInteractionAnswer(body);
        if (typeof sessionId !== "string" || !sessionId || typeof interactionId !== "number" || !Number.isSafeInteger(interactionId) || interactionId < 1 || !answer)
            return c.json({ error: "Invalid interaction response" }, 400);
        const opened = sessions.get(sessionId);
        if (!opened) return c.json({ error: "Session is not active" }, 404);
        const selected = await opened;
        if (!selected.interactions.respond(sessionId, interactionId, answer))
            return c.json({ error: "Interaction is no longer pending" }, 409);
        return c.json({ accepted: true });
    });

    const port = Number(Bun.env.PORT ?? "3000");
    server = Bun.serve({ hostname: "127.0.0.1", port, fetch: app.fetch });
    console.log(`Peri SDK WASM demo: http://127.0.0.1:${server.port}`);
    await new Promise<void>((done) => {
        process.once("SIGINT", done);
        process.once("SIGTERM", done);
    });
} finally {
    await server?.stop(true);
    const opened = await Promise.allSettled(sessions.values());
    for (const result of opened) if (result.status === "fulfilled") {
        result.value.interactions.close();
        await result.value.docs.close();
    }
    const agents = opened.flatMap((result) => result.status === "fulfilled" ? [result.value.agent] : []);
    await manager.closeAll(await shutdownCommands(agents));
}
