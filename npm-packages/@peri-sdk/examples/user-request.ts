// 详细设计用的场景伪代码；managedAgentKv 等由宿主提供。
// 实际就是 meta_harness 全部关闭
const bearHarness = BareHarnessConfig;

const config = {
    meta_harness: {
        ...bearHarness,
        McpMiddleware: true,
        ToolSearch: true,
        WorkspaceMiddleware: false, // 只关闭内置 Workspace；同名 HTTP Workspace 仍可用
    },
};
// session 的存储器配置，只需传递给 peri，peri 会自动连接
const storage = new TursoStorage({ url: "turso://<database>.turso.io", authToken: "<secret>" });

const workspaceId = "workspaceId";
const sandbox = new Sandbox({
    id: workspaceId,
    storage,
    workspace: { url: "https://<workspace-host>/mcp" },
    // 执行装配由宿主提供：WASM 部署用 startPeriWasmHost，其他部署注入自有 transport。
    // SDK 不启动 Peri 或 Workspace 进程，也不再接受 stdio / workspaceProcess 选项。
    transportFactory: (path) => startPeriWasmHost({
        moduleUrl: new URL("./peri-wasm.js", import.meta.url),
        configJson: JSON.stringify({ cwd: path, settings: { config: { ...config, active_alias: "sonnet" } } }),
    }),
});
// HTTP Workspace 的会话级 MCP 声明由 Sandbox 合入，Peri 作为 MCP client 连接。
const workspace = sandbox.getWorkspace()
// ACP 顺序：装配 WASM Host → initialize → session/new 或 session/load。
// settings 与 storage 经 WASM 启动对象一次性交给 Peri；凭证由调用方显式传入。
// 现有 Peri 不接受 workspaceId 作为 machineId；此 id 只用于 SDK 的 KV 占位作用域。
// session/new: { cwd: agent.path, mcpServers: [{ type: "http", name: "workspace",
//   url: workspace.url, headers: [] }], _meta: { "peri.instructions": instructions } }。
// 其他 mcpServers 也在会话 setup 数组中传递；加载已有会话时会重传同一声明。
// initialize 只协商能力。

// ManagedAgents 同步登记 Agent 声明；一个 Agent 只绑定一个 Session。
// 宿主注入共享 unjs KV；其占位适配器必须提供原子 claim-if-absent。
// createAgent 不占位；异步 start 才按 Agent/Session 身份占位，先到先得，后来者报错。
// transportFactory 只在 start 时调用一次；每个 Session 装配一个独立 WASM 实例。
const managedAgents = new ManagedAgents({ kv: managedAgentKv });
const requestedSessionId: string | null = null; // 业务请求传已有 ID 时恢复，否则新建。
const agent = managedAgents.createAgent({
    id: "test-agent",
    sandbox,
    path: "/tmp/peri-workspace", // 新建会话需要；加载已有会话时从 Store 按 Session ID 取 cwd。
    // ACP 无标准 instructions 字段；Peri 用 session/new._meta["peri.instructions"] 传递并冻结。
    instructions: "你是一个有趣的 AI 助手，能回答用户的问题",
    // 这里是额外 MCP；Workspace 自动合入 session/new.mcpServers 数组。
    mcpServers: {},
});

const session = await agent.session.start(requestedSessionId); // 原子占位成功后才创建 Transport、初始化 ACP 并绑定会话。

// 场景 1：持续消费当前连接的原始 ACP session/update 与 peri/agent_event 通知。
void (async () => {
    for await (const event of session.stream()) console.log(event);
})();

// 场景 2：Agent 经 Sandbox 直接查询 Session Store，不经过 ACP；供下一次创建 Agent 时选择。
const sessions = await agent.getSessions();
console.log("available sessions", sessions);
const firstMessage = session.send("Hello, world!");
await firstMessage; // 只等待这次用户输入被投递；Session 仍可接收后续输入。

// 场景 3：同一个 Session 中继续插入用户输入；忙碌时可先进入待发送队列。
const followup = session.send("再看看测试覆盖");
await followup;
const command = session.send("into waiting list");
console.log("is sent", command.isSent);
command.forceSend(); // 请求立即派发；是否执行及何时执行由 Peri 裁决。
await command;

// 场景 4：取消是会话级 ACP 通知；未完成 turn 的收敛由 Peri 决定。
// SDK 不再暴露 stop/pause/resume/reopen 或执行准入：这些方法对应的 ACP 协议已移除。
await session.cancel();
const inbox = session.docs.session.getMap<unknown>("root").get("session") as { get(key: string): unknown } | undefined;
console.log("queue state", inbox?.get("inputQueue"));
// 队列中尚未派发的输入可以被撤回；已派发的输入只能等 Peri 收敛。
await session.takeBack(followup.inputId).catch(() => {});
await session.send("换一个更小的问题");

// 场景 5：另一实例同时 start 同一 Agent/Session 时，KV 占位失败并明确报冲突。
// 场景 6：服务不再需要此 Agent 时显式关闭。close 发送 session/close 并在预算内重放，
// 直到 Peri 的 CloseCoordinator 结算；超时抛 SessionCloseIncompleteError 并保留 claims。
// Store 中的会话数据仍保留，Workspace 由部署方独立管理。
async function releaseAgent() {
    await managedAgents.closeAgent(agent.id, { drainTimeoutMs: 10_000 });
}
