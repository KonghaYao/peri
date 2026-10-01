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
// 本质是 0728 的 MCP Http 模式的 workspace mcp 配置，传递给 peri 自动连接
const workspace = new Workspace({
    machineId: null, // 机器 Id,与 Storage 有关，但是我不确定这个设计是否合理
    path: "/app/workspace",
    url: "https://workspace.peri-demo.demo/mcp", // 由宿主映射到 mcpServers.workspace.url，工具以 tools/list 为准
});

// 这个是批量管理 Agent 的方案
// const periManager = new ManagedAgents({
//
// })
// periManager.createAgent()

// Agent 实现一定要分清楚 Transport 层，将 TS 与 peri 的通信抽象为 Transport；遥远未来的集群方案，每个 Transport 可能还会改为 ws 通信，所以先抽象好
const agent = new Agent({
    id: "test-agent",
    config,
    model: "sonnet",
    storage,
    workspace,
    // peri 应该还没有实现直接穿入作为系统提示词的能力，这一块应该 ACP 由有一个传递方式
    instructions: "你是一个有趣的 AI 助手，能回答用户的问题",
    // MCP 服务器的配置，peri 可能需要提供注入方案
    // ？ ACP 是否可以在初始化的时候传递 MCP 配置？
    mcpServers: {},
});

// plan a： 查询 sessions，其实就是直接查询 storage， peri 需要提供 sessions 查询的 cli 命令行能力，从而无需开进程；
//plan b：这个函数本质是 ts 端连接 session 并查询的。
const sessions = await agent.listSessions();
const sessionId = sessions[0]?.id || null; // null 是新建 session 的标志
const session = await agent.session.start(sessionId); // 这个时候才初始化 ACP 相关的进程

(async () => {
    // 所有的 ACP 往返交互都需要封装为 PeriEvent，发送给 peri的 json 也是这个格式，

    // 监听异步的信息流
    for await (const event of session.stream()) console.log(event); //所有 Event 都是规范的 PeriEvent extends ACPEvent，Peri业务属性的自定义 Event 需要在这里支持
})();

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
