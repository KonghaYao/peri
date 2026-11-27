/** Bun HTTP demo：ChatManager + 宿主注入的 ACP transport，每个 Session 独立装配。 */
import { readFile, realpath } from "node:fs/promises";
import { resolve } from "node:path";
import {
    BareHarnessConfig,
    ChatManager,
    PostgresStorage,
    type ChatTransportFactoryOptions,
    type PeriConfig,
    type Transport,
} from "../../src/entries/index";
import { createDemoApp } from "./chat-demo";
import { DemoKvStorage } from "./demo-storage";

const configuredWorkspace = Bun.env.PERI_WORKSPACE;
if (!configuredWorkspace) throw new Error("PERI_WORKSPACE is required");
const workspace = await realpath(configuredWorkspace);
const workspaceMcpUrl = Bun.env.PERI_WORKSPACE_MCP_URL;
if (!workspaceMcpUrl) throw new Error("PERI_WORKSPACE_MCP_URL is required; provide an external HTTP Workspace");
// transport 由宿主提供：模块需导出 createTransport(path, options)，返回一条已连接的 ACP transport。
const transportModuleSpecifier = Bun.env.PERI_ACP_TRANSPORT_MODULE;
if (!transportModuleSpecifier) {
    throw new Error(
        "PERI_ACP_TRANSPORT_MODULE is required; export createTransport(path, options) from a host transport module",
    );
}
const transportModule = await import(transportModuleSpecifier) as {
    createTransport?: (
        path: string,
        options?: ChatTransportFactoryOptions,
    ) => Transport | Promise<Transport>;
};
const createTransport = transportModule.createTransport;
if (typeof createTransport !== "function") {
    throw new Error(`${transportModuleSpecifier} must export createTransport(path, options)`);
}
const html = await readFile(resolve(import.meta.dir, "demo.html"), "utf8");
const databaseUrl = Bun.env.PERI_STORAGE_URL;
if (!databaseUrl) throw new Error("PERI_STORAGE_URL must identify the PostgreSQL database");
const sseClient = await Bun.build({
    entrypoints: [resolve(import.meta.dir, "../../node_modules/@microsoft/fetch-event-source/lib/esm/index.js")],
    target: "browser",
    format: "esm",
});
if (!sseClient.success) throw new Error(`Cannot bundle SSE client: ${sseClient.logs.join("; ")}`);
const sseClientJs = await sseClient.outputs[0]!.text();
const sessionView = await Bun.build({
    entrypoints: [resolve(import.meta.dir, "../../src/entries/view.ts")],
    target: "browser",
    format: "esm",
});
if (!sessionView.success) throw new Error(`Cannot bundle Session view: ${sessionView.logs.join("; ")}`);
const sessionViewJs = await sessionView.outputs[0]!.text();

const database = new PostgresStorage({
    url: databaseUrl,
    httpEndpoint: Bun.env.PERI_STORAGE_HTTP_ENDPOINT,
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
            fable: { provider: "anthropic", model: "deepseek-v4-flash" },
            opus: { provider: "anthropic", model: "deepseek-v4-flash" },
            sonnet: { provider: "anthropic", model: "deepseek-v4-flash" },
            haiku: { provider: "anthropic", model: "deepseek-v4-flash" },
        },
        meta_harness: {
            ...BareHarnessConfig,
            McpMiddleware: true,
            ToolSearch: true,
            WorkspaceMiddleware: false,
        },
    },
} satisfies PeriConfig;

const manager = new ChatManager({
    store: {
        storage: database.storageConfig,
        // 本地 demo 固定 UUID；生产必须持久化并复用，不要每次启动随机。
        machineId: Bun.env.PERI_MACHINE_ID ?? "00000000-0000-4000-8000-000000000001",
    },
    database,
    kv: new DemoKvStorage(),
    chatConfig: {
        runtime: { kind: "transport", factory: createTransport },
        session: {
            instructions: "你是 Peri。请用清晰、简洁的中文帮助用户完成任务。",
            mcpServers: {
                demo: { type: "http", url: "https://demo-day.mcp.cloudflare.com/mcp" },
            },
        },
    },
});

let server: ReturnType<typeof Bun.serve> | undefined;
const demo = createDemoApp({
    manager,
    database,
    workspace,
    agent: { cwd: workspace, settings: config },
    workspaceMcpUrl,
    html,
    sseClientJs,
    viewBundleJs: sessionViewJs,
});
try {
    try {
        await database.listSessions(workspace);
    } catch {
        throw new Error("Local Session Store is unavailable; run `bun run db:dev` or check PERI_STORAGE_URL and PERI_STORAGE_HTTP_ENDPOINT");
    }
    const port = Number(Bun.env.PORT ?? "3000");
    server = Bun.serve({
        hostname: "127.0.0.1",
        port,
        fetch: demo.app.fetch,
    });
    console.log(`Peri SDK demo: http://127.0.0.1:${server.port}`);
    await new Promise<void>((done) => {
        process.once("SIGINT", done);
        process.once("SIGTERM", done);
    });
} finally {
    try {
        await server?.stop(true);
        await demo.close();
    } finally {
        // ChatManager.shutdown 不关闭注入的数据库；连接归宿主所有。
        await database.close();
    }
}
