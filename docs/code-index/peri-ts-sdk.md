# Peri TS SDK 代码索引

状态：WASM 已通过 `WasmAcpTransport` 接入现有 `peri-acp` Host，`Sandbox`、`ManagedAgents`、`Agent` 和 `Session` 沿用同一接口。TypeScript transport 只关联 JSON-RPC 消息；SDK 的 `SessionDocs` 从 ACP 通知投影实时 Yjs 状态。Turso 会话写入、恢复由 Rust 的现有存储端口处理。

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| 公开 API | `npm-packages/@peri-sdk/src/sdk/index.ts` | 导出 Agent、Session、ManagedAgents、Sandbox 和 `WasmAcpTransport` |
| WASM 产物装配 | `src/wasm/loader.ts` + `scripts/build.ts` | 从根 workspace 编译 Emscripten release，打包 JS/WASM；懒加载并复用 `PeriWasmAcp` 模块 |
| Peri 配置类型 | `src/config/` | `PeriConfig` 描述受信 settings 启动帧，`MetaHarnessKey` 限定能力键；`BareHarnessConfig` 是冻结的全关闭预设，demo 按需重新开启 MCP 与 ToolSearch |
| 会话与输入 | `src/agent/{agent,session,send-receipt,types}.ts` | `session/new|load`、连续输入队列、交付事件、取消与事件流；未领取的输入若从 `dispatching` 退回 `queued`，按队列事件重派发一次，再失败则取回并报告错误；Agent 按自身或已加载 Session 的 path 经 Sandbox 向 SessionStorage 列举会话，不走 ACP `session/list`；每个类单独成文件 |
| 实时 Yjs 投影 | `src/state/` + `src/agent/{agent,session,send-receipt}.ts` | `session-docs.ts` 分发 ACP 通知，`protocol.ts` 解码白名单，`doc-model.ts` 管理两个 Y.Doc 的 schema；`turn-machine.ts` 统一轮次终止，chat/session/task/input-queue/interaction projection 分别聚合聊天、会话、任务、输入队列和反向权限/提问。真实输入交付时写入 Chat Doc；后台任务修订缺口经 `session/bg-tasks` 补快照。Agent/Session 暴露同一组文档；冷 `session/load` 的历史通知进入相同投影。Y.Doc 承载 SDK 实时展示状态；Rust Session Store 继续负责可恢复的 canonical 会话数据 |
| 前端会话读模型 | `src/view/{session-view,index}.ts` | 从授权的 Chat/Session Y.Doc 对读取有序消息块、工具、轮次、任务、计划、输入队列和待处理交互；`SessionViewStore` 批量通知并成对切换文档，DOM/React/传输均在外层 |
| 并发占位 | `src/managed/` + `src/kv/` | ManagedAgents 同步声明 Agent，异步 start 时共享 KV 原子 claim，冲突报错，owner 释放；MemoryKV 仅单进程使用 |
| Sandbox 与存储 | `src/sandbox/{sandbox,workspace-mcp-process,process-supervisor}.ts` + `src/storage/` | Sandbox 持有 SessionStorage 接口；`getSessions(path)` 按 Agent path 读数据库；`getSession(id)` 为恢复读取持久 cwd，远端使用官方 `@tursodatabase/serverless`。可连接已有 HTTP Workspace，或启动独立 Workspace 并确认 MCP readiness；自管 Workspace 在工作目录保留异常退出 guard，任务 scope 由部署可信连接承载。私有 supervisor 只为本 Sandbox 的精确 ACP 进程代际提供退出证明；Session start 时由 Sandbox 提供 Store 部署参数 |
| ACP 传输 | `src/transport/{json-rpc-transport,stdio-transport,wasm-transport,process-broker,event-queue,types}.ts` | 共用 JSON-RPC ID、反向请求、通知和关闭语义；stdio 用 Bun 子进程与 settings 前置帧，每个 ACP generation 的 broker 登记本地子进程组，收敛失败不给关闭接管证明；WASM 用 `PeriWasmAcp` 的原始帧端口 |
| 可运行示例 | `examples/demo/{demo.ts,demo-wasm.ts,demo.html,session-doc-stream.ts}` + `mise.toml` | 两个 Hono/Bun 服务复用同一前端、Session 管理与 POST/SSE 路由；loopback demo 用双 Doc 快照与有序 Yjs 增量向浏览器接力，页面通过共享读模型展示状态，原始 ACP 事件仅作诊断。反向交互由 `InteractionResponder` 关联请求 ID 后通过 POST 回复。`demo.ts` 启动原生 Peri stdio，`demo-wasm.ts` 用 `Sandbox.transportFactory` 启动 WASM ACP Host。WASM 的文件工具经可选外部 Workspace HTTP MCP 提供，本地 builtin 不启动；侧栏从 SessionStorage 查询会话 |
| 验证 | `test/domain.test.ts` + `tests/*.test.ts` + `scripts/smoke-wasm-*.mjs` | Bun SDK 测试覆盖生命周期、真实 stdio 帧和冷恢复；WASM 集成与 Node ACP smoke 验证 Host 重启恢复及远程 MCP 工具调用 |


未解决的契约缺口：SDK 不把 `Sandbox.id` 传入 Peri 作为 `machine_id`；Peri 从自己的身份来源解析该值。因此 `Sandbox.id` 只用于 SDK 侧占位作用域，尚不等于 Peri Store 的机器身份。共享 KV 适配器的崩溃恢复/租约策略也由部署方提供。

实时投影边界：Peri 的 `session/load` 历史逐条回放若中途发送失败，Host 当前只记录告警后仍返回成功；SDK 尚不能证明回放完整。长期会话反复 compact/rewind 会在同一 Y.Doc 内产生删除记录；当前保持根集合对象身份以供订阅者使用，尚无 Fenix Doc Hub 式文档换代与快照压缩协议。
