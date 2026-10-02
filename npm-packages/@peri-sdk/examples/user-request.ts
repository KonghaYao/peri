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
const storage = new TursoStorage({ url: "libsql://<database>.turso.io", authToken: "<secret>" });

const workspaceId = "workspaceId";
const sandbox = new Sandbox({
    id: workspaceId,
    path: "/tmp/peri-workspace",
    storage,
    stdio: {
        command: "peri",
        settings: { config: { ...config, active_alias: "sonnet" } },
    },
    workspace: { url: "https://<workspace-host>/mcp" },
});
// 本质是 0728 的 MCP Http 模式的 workspace mcp 配置，传递给 peri 自动连接
const workspace = sandbox.getWorkspace()
// 本地 Transport 的启动输入；以后换 WS Transport 时，Agent / Session 接口不用改。
// 本地 Transport 在 ACP 开始前，经子进程 stdin 写入 4 字节大端长度 + settings JSON。
// 现有 Peri 不接受 workspaceId 作为 machineId；此 id 目前只用于 SDK 的 KV 占位作用域。
// Sandbox 在 Session.start 时启动 Transport，并使用构造时传入的 Storage。
// ACP 顺序：启动进程 → initialize → session/new 或 session/load。
// session/new: { cwd: workspace.path, mcpServers: [{ type: "http", name: "workspace",
//   url: workspace.url, headers: [] }], _meta: { "peri.instructions": instructions } }。
// 其他 mcpServers 也在会话 setup 数组中传递；load/resume/fork 切换会话时要重传同一声明。
// initialize 只协商能力。

// ManagedAgents 同步登记 Agent 声明；一个 Agent 只绑定一个 Session。
// 宿主注入共享 unjs KV；其占位适配器必须提供原子 claim-if-absent。
// createAgent 不占位；异步 start 才按 Agent/Session 身份占位，先到先得，后来者报错。
// Transport 工厂也只在 start 时调用；Sandbox 的进程重试由 Sandbox 自己负责。
const managedAgents = new ManagedAgents({ kv: managedAgentKv });
const requestedSessionId: string | null = null; // 业务请求传已有 ID 时恢复，否则新建。
const agent = managedAgents.createAgent({
    id: "test-agent",
    sandbox,
    // ACP 无标准 instructions 字段；Peri 用 session/new._meta["peri.instructions"] 传递并冻结。
    instructions: "你是一个有趣的 AI 助手，能回答用户的问题",
    // 这里是额外 MCP；Workspace 自动合入 session/new.mcpServers 数组。
    mcpServers: {},
});

const session = await agent.session.start(requestedSessionId); // 原子占位成功后才创建 Transport、初始化 ACP 并绑定会话。

// 场景 1：持续消费当前连接的事件；SDK 把 ACP 更新转为 PeriEvent。
void (async () => {
    for await (const event of session.stream()) console.log(event);
})();

// 场景 2：Agent 经 Sandbox 直接查询 Session Store，不经过 ACP；供下一次创建 Agent 时选择。
const sessions = await agent.getSessions();
console.log("available sessions", sessions);
const firstMessage = session.send("Hello, world!");
await firstMessage; // 只等待这次用户输入被投递；Run 仍可接收后续输入。

// 场景 3：同一个 Run 中继续插入用户输入；忙碌时可先进入待发送队列。
const followup = session.send("再看看测试覆盖");
await followup;
const command = session.send("into waiting list");
console.log("is sent", command.isSent);
command.forceSend(); // 请求立即派发；是否执行及何时执行由 Peri 裁决。
await command;

// 场景 4：请求中断当前执行，不关闭 Run 或 Session；仍能插入新输入。
await session.cancel();
await session.send("换一个更小的问题");

// 场景 5：另一实例同时 start 同一 Agent/Session 时，KV 占位失败并明确报冲突。
// 场景 6：服务不再需要此 Agent 时显式关闭；Session 与 Sandbox 仍由各自的 owner 保留。
async function releaseAgent() {
    await managedAgents.closeAgent(agent.id); // 仅释放自己持有的 KV 占位。
}
