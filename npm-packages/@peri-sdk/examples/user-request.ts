// 实际就是 meta_harness 全部关闭
const bearHarness = new BearHarness();

const config = {
    meta_harness: {
        ...bearHarness,
        McpMiddleware: true,
        ToolSearch: true,
        WorkspaceMiddleware: false, // 只关闭内置 Workspace；同名 HTTP Workspace 仍可用
    },
};
// session 的存储器配置，只需传递给 peri，peri 会自动连接
const storage = new TrusoStorage();
const machineId = Bun.env.PERI_MACHINE_ID; // 由业务管理，必须是当前执行机的稳定 UUID
if (!machineId) throw new Error("PERI_MACHINE_ID 不能为空");
// 本质是 0728 的 MCP Http 模式的 workspace mcp 配置，传递给 peri 自动连接
const workspace = new Workspace({
    path: "/app/workspace",
    machineId,
    url: "https://workspace.peri-demo.demo/mcp", // 由宿主映射到 mcpServers.workspace.url，工具以 tools/list 为准
});

// 本地 Transport 的启动输入；以后换 WS Transport 时，Agent / Session 接口不用改。
// config + model 由本地 Transport 写入 settings.json；ACP 分支目前不消费 --model。
const periLaunch = {
    command: "peri",
    settingsFile: "/app/peri/settings.json",
    settings: { config: { ...config, model: "sonnet" } }, // 本地 Transport 启动前写入 settingsFile
    args: [
        "--config-file", "/app/peri/settings.json",
        "--session-store", "env:TURSO_URL",
        "--session-store-token-env", "TURSO_AUTH_TOKEN",
        "--session-store-engine", "turso",
        "acp", "--cwd", workspace.path,
    ],
    env: { PERI_MACHINE_ID: machineId, TURSO_URL: Bun.env.TURSO_URL, TURSO_AUTH_TOKEN: Bun.env.TURSO_AUTH_TOKEN },
};
// ACP 顺序：启动进程 → initialize → session/new。
// session/new: { cwd: workspace.path, mcpServers: [{ type: "http", name: "workspace",
//   url: workspace.url, headers: [] }], _meta: { "peri.instructions": instructions } }。
// 其他 mcpServers 也在会话 setup 数组中传递；load/resume/fork 切换会话时要重传同一声明。
// initialize 只协商能力。

// 这个是批量管理 Agent 的方案
// const periManager = new ManagedAgents({
//
// })
// periManager.createAgent()

// Agent 实现一定要分清楚 Transport 层，将 TS 与 peri 的通信抽象为 Transport；遥远未来的集群方案，每个 Transport 可能还会改为 ws 通信，所以先抽象好
const agent = new Agent({
    id: "test-agent",
    transport: createTransport(periLaunch), // Agent 只依赖 Transport 接口
    config,
    model: "sonnet",
    storage,
    workspace,
    // ACP 无标准 instructions 字段；Peri 用 session/new._meta["peri.instructions"] 传递并冻结。
    instructions: "你是一个有趣的 AI 助手，能回答用户的问题",
    // 这里是额外 MCP；Workspace 自动合入 session/new.mcpServers 数组。
    mcpServers: {},
});

const session = await agent.session.start(); // 这个时候才初始化 ACP 相关的进程

(async () => {
    // 所有的 ACP 往返交互都需要封装为 PeriEvent，发送给 peri的 json 也是这个格式，

    // 监听异步的信息流
    for await (const event of session.stream()) console.log(event); //所有 Event 都是规范的 PeriEvent extends ACPEvent，Peri业务属性的自定义 Event 需要在这里支持
})();

// plan a： 查询 sessions，其实就是直接查询 storage， peri 需要提供 sessions 查询的 cli 命令行能力，从而无需开进程；
//plan b：这个函数本质是 ts 端连接 session 并查询的。
const sessions = await session.listSessions();
const sessionId = sessions[0]?.id || null; // null 是新建 session 的标志
session.changeSession(sessionId); // 这个时候才切换入会话

(async () => {
    // 本质上这里的都是 ACP 信道传递信息
    sleep(1000);
    session.send("Hello, world!"); // 只有被发送完成才结束 Promise
    console.log("session status", session.status);
    sleep(1000);
    console.log("session status", session.status);
    const command = session.send("into waiting list"); // 只有被发送完成才结束 Promise
    console.log("is sent", command.isSent);
    command.forceSend(); // 强制发送,peri 有这个特定的方式

    await session.cancel();

    // 取消后，session.status 会变为正常
})();
