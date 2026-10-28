// 详细设计用的场景伪代码；kv 等由宿主提供。
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

// 宿主持久化的 UUID，标识逻辑执行环境；不要用 workspaceId/sessionId 代替。
const machineId = "<stable-machine-uuid>";
const workspacePath = "/tmp/peri-workspace";
// Workspace 是 Chat 的边界：runtimePath 是 Agent/Session 看到的根，hostPath 是宿主路径。
const workspace = {
    id: "ws-user-request",
    hostPath: workspacePath,
    runtimePath: workspacePath,
};
// 调用方注入存储端口；unstorage 由宿主安装，SDK 不 import 其实现。
const manager = new ChatManager({
    // Manager = 部署/Store 边界：Store 与机器身份由整个 Manager 共享。
    store: {
        storage: storage.storageConfig,
        machineId,
    },
    // 只读 Session 目录：TursoStorage 直接实现 ChatDatabase。
    database: storage,
    kv: managedAgentStorage,
    chatConfig: {
        // 执行装配由调用方选择：WASM 部署用 runtime.kind="wasm"；
        // 其他部署用 runtime.kind="transport" 注入自有 transport。SDK 不启动 Peri 或 Workspace 进程。
        runtime: {
            kind: "wasm",
            moduleUrl: new URL("./peri-wasm.js", import.meta.url),
        },
        session: {
            // cwd 由 workspace.runtimePath 派生；这里只声明 instructions 与额外 MCP。
            // ACP 无标准 instructions 字段；Peri 用 session/new._meta["peri.instructions"] 传递并冻结。
            instructions: "你是一个有趣的 AI 助手，能回答用户的问题",
            // 这里是额外 MCP；HTTP Workspace 由部署方另行提供。
            mcpServers: {},
        },
    },
});

// ChatManager.createChat 同步登记句柄；不启动 ACP、不取得 Session ownership。
// workspace 必填；agent.cwd 由 SDK 注入，不能声明 storage/machineId。
const chat = manager.createChat({
    workspace,
    sessionPath: "", // 相对 workspace.runtimePath；省略时为根
    agent: {
        settings: { config: { ...config, active_alias: "sonnet" } },
    },
});
// 已有 Session 用 change_session；cwd 从 Store 读取，回退到派生的会话 cwd。
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
