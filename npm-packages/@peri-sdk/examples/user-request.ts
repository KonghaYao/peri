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
const workspacePath = "/tmp/peri-workspace";
// 调用方注入共享 KV；其占位适配器必须提供原子 acquire-if-absent。
const manager = new ChatManager({
    agentConfig: {
        cwd: workspacePath,
        settings: { config: { ...config, active_alias: "sonnet" } },
        storage: { url: "turso://<database>.turso.io", authToken: "<secret>" },
        machineId: workspaceId,
    },
    database: {
        // 只读目录：业务侧按 Store 的 threads 表实现。
        listSessions: (path) => storage.getSessions(path ?? workspacePath),
        getSession: (id) => storage.getSession(id),
    },
    sessionOwnership: new AtomicSessionOwnership(managedAgentKv),
    // 执行装配由调用方选择：WASM 部署用 runtime.kind="wasm"；
    // 其他部署用 runtime.kind="transport" 注入自有 transport。SDK 不启动 Peri 或 Workspace 进程。
    runtime: {
        kind: "wasm",
        moduleUrl: new URL("./peri-wasm.js", import.meta.url),
    },
    defaults: {
        path: workspacePath,
        // ACP 无标准 instructions 字段；Peri 用 session/new._meta["peri.instructions"] 传递并冻结。
        instructions: "你是一个有趣的 AI 助手，能回答用户的问题",
        // 这里是额外 MCP；HTTP Workspace 由部署方另行提供。
        mcpServers: {},
    },
});

// ChatManager.createChat 同步登记句柄；不启动 ACP、不取得 Session ownership。
const chat = manager.createChat({ path: workspacePath });
// 已有 Session 用 change_session；cwd 从 Store 读取，不回退句柄 path。
const requestedSessionId: string | null = null; // 业务请求传已有 ID 时恢复，否则新建。
const ready = requestedSessionId === null
    ? await chat.execute(ChatUserAction.createNewSession({}))
    : await chat.execute(ChatUserAction.changeSessionTo(requestedSessionId));
console.log("bound session", ready.sessionId);

// 场景 1：只读展示层。ChatView 消费事实并维护 Y.Doc；浏览器/服务端通过 ChatYjsChannel 同步。
const view = new ChatView(chat);
view.sync(channel); // channel 由部署方提供鉴权与物理连接。

// 场景 2：经 Manager 读取 Session 目录，不启动运行实例。
const sessions = await manager.listSessions({ path: workspacePath });
console.log("available sessions", sessions);

// 场景 3：发送与队列控制。send 同步返回 receipt，accepted 不等于已投递。
const firstMessage = chat.execute(ChatUserAction.send({ text: "Hello, world!" }));
await firstMessage; // 等待这次用户输入被投递；Chat 仍可接收后续输入。
const followup = chat.execute(ChatUserAction.send({ text: "再看看测试覆盖" }));
await followup;
const command = chat.execute(ChatUserAction.send({ text: "into waiting list" }));
command.forceSend(); // 请求立即派发；是否执行及何时执行由 Peri 裁决。
await command;
await chat.execute(ChatUserAction.takeBack(followup.inputId)).catch(() => {});

// 场景 4：取消是会话级 ACP 通知；未完成 turn 的收敛由 Peri 决定。
await chat.execute(ChatUserAction.cancel({}));

// 场景 5：另一实例同时 change_session 同一 Session 时，ownership 获取失败并明确报冲突。
// 场景 6：服务不再需要此 Chat 时先关闭展示，再关闭句柄。
// close 发送 session/close 并在预算内重放；超时抛 ChatCloseIncompleteError 并保留 ownership。
// Store 中的会话数据仍保留，Workspace 由部署方独立管理。
async function releaseChat() {
    await view.dispose();
    await chat.close();
    await manager.shutdown();
}
