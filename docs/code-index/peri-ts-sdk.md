# Peri TS SDK 代码索引

状态：WASM 已通过 `WasmAcpTransport` 接入现有 `peri-acp` Host，`Sandbox`、`ManagedAgents`、`Agent` 和 `Session` 沿用同一接口。TypeScript transport 只关联 JSON-RPC 消息；SDK 的 `SessionDocs` 从 ACP 通知投影实时 Yjs 状态。Turso 会话数据写入、恢复由 Rust 的现有存储端口处理；会话执行唯一性、进程代际协调与接管唯一由 `peri-sdk` 负责，schema 当前为 14，Rust Store/ACP/Agent 不再维持会话执行 owner、租约或 fencing，仅保留 binding/path 校验与任务资源生命周期。SDK Session-only 占位与可重试清理已实现，契约见 [SDK README](../../npm-packages/@peri-sdk/README.md) 与 [会话身份设计](../design/session-id-environment.md)；完整跨实例进程接管与生产崩溃恢复仍未实现。

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| 公开 API | `npm-packages/@peri-sdk/src/sdk/index.ts` | 导出 Agent、Session、ManagedAgents、Sandbox 和 `WasmAcpTransport` |
| WASM 产物装配 | `src/wasm/loader.ts` + `scripts/build.ts` | 从根 workspace 编译 Emscripten release，打包 JS/WASM；懒加载并复用 `PeriWasmAcp` 模块 |
| Peri 配置类型 | `src/config/` | `PeriConfig` 描述受信 settings 启动帧，`MetaHarnessKey` 限定能力键；`BareHarnessConfig` 是冻结的全关闭预设，demo 按需重新开启 MCP 与 ToolSearch |
| 会话与输入 | `src/agent/{agent,session,send-receipt,types}.ts` | `session/new|load`、连续输入队列、交付事件、取消与事件流；未领取的输入若从 `dispatching` 退回 `queued`，按队列事件重派发一次，再失败则取回并报告错误；Agent 按自身或已加载 Session 的 path 经 Sandbox 向 SessionStorage 列举会话，不走 ACP `session/list`；每个类单独成文件 |
| 实时 Yjs 投影 | `src/state/` + `src/agent/{agent,session,send-receipt}.ts` | `session-docs.ts` 分发 ACP 通知，`protocol.ts` 解码白名单，`doc-model.ts` 管理两个 Y.Doc 的 schema；`tool-payloads.ts` 把超过 4 KiB 的工具 JSON 以版本引用隔离，`SessionDocs.readPayload` 按需读完整内容；`doc-model.ts` 保留工具归属和按 turn 的索引，`turn-machine.ts` 统一轮次终止，chat/session/task/input-queue/interaction projection 分别聚合聊天、会话、任务、输入队列和反向权限/提问。真实输入交付时写入 Chat Doc；后台任务修订缺口经 `session/bg-tasks` 补快照。Agent/Session 暴露同一组文档；冷 `session/load` 的历史通知进入相同投影。Y.Doc 承载 SDK 实时展示状态；Rust Session Store 继续负责可恢复的 canonical 会话数据 |
| 前端会话读模型 | `src/view/{session-view,index}.ts` | 从授权的 Chat/Session Y.Doc 对读取有序消息块、工具、轮次、任务、计划、输入队列和待处理交互；`read-cache.ts` 沿 Yjs 所有权和工具引用失效缓存，`SessionViewStore` 复用未改变的不可变值、批量通知并成对切换文档，DOM/React/传输均在外层 |
| Yjs 复制 | `src/sync/{session-doc-sync,session-doc-replica,peer,index}.ts` | 二进制版本 2；有界批处理、按状态向量补齐和 generation 换代。单订阅者字节预算/异步写入控制慢消费者；每个 adapter 拥有其 frame buffer；关闭排空尾部，错误或缺帧重连恢复 |
| 并发占位 | `src/managed/` + `src/kv/{agent-claims,types,memory-kv}.ts` | ManagedAgents 同步声明 Agent，异步 start 时共享 KV 原子 claim，冲突报错，按 owner 释放；`sessionClaimKey` 仅 Session ID，`agentClaimKey` 保持 Sandbox + Agent；MemoryKV 仅单进程使用 |
| Sandbox 与存储 | `src/sandbox/{sandbox,workspace-mcp-process,process-supervisor}.ts` + `src/storage/` | Sandbox 持有 SessionStorage 接口；`getSessions(path)` 按 Agent path 读数据库；`getSession(id)` 为恢复读取持久 cwd，远端使用官方 `@tursodatabase/serverless`。可连接已有 HTTP Workspace，或启动独立 Workspace 并确认 MCP readiness；任务 scope 由部署可信连接承载；Workspace 不再写入 owner marker 或通过遗留 marker 阻断启动。私有 supervisor 只为本 Sandbox 的精确 ACP 进程代际提供退出证明；Session start 时由 Sandbox 提供 Store 部署参数 |
| ACP 传输 | `src/transport/{json-rpc-transport,stdio-transport,wasm-transport,process-broker,event-queue,types}.ts` | 共用 JSON-RPC ID、反向请求、通知和关闭语义；`event-queue.ts` 为可选原始事件流提供有界缓冲和显式 overflow，状态投影独立继续；stdio 用 Bun 子进程与 settings 前置帧，每个 ACP generation 的 broker 登记本地子进程组，进程收敛失败须由 SDK 保留未结清状态，不向 ACP 提交接管 proof；WASM 用 `PeriWasmAcp` 的原始帧端口 |
| 可运行示例 | `examples/demo/{demo.ts,demo-wasm.ts,demo.html,session-doc-stream.ts}` + `mise.toml` | 两个 Hono/Bun 服务复用同一前端、Session 管理与 POST/SSE 路由；loopback demo 的 `session-doc-stream.ts` 适配 SDK 二进制复制为 SSE，`session-sse.ts` 限制写队列；页面按需获取工具全文、复用 DOM 并限制初始可见历史，原始 ACP 诊断为可选有界日志。反向交互由 `InteractionResponder` 关联请求 ID 后通过 POST 回复。`demo.ts` 启动原生 Peri stdio，`demo-wasm.ts` 用 `Sandbox.transportFactory` 启动 WASM ACP Host。WASM 的文件工具经可选外部 Workspace HTTP MCP 提供，本地 builtin 不启动；侧栏从 SessionStorage 查询会话 |
| 验证 | `test/domain.test.ts` + `tests/*.test.ts` + `scripts/smoke-wasm-*.mjs` | Bun SDK 测试覆盖生命周期、真实 stdio 帧和冷恢复；`session-sync.test.ts`、`tool-payloads.test.ts`、`session-view-incremental.test.ts` 和 demo 的真实 HTTP SSE 测试覆盖复制、按需全文及缓存失效；`benchmarks/README.md` 路由到可复现压力场景；WASM 集成与 Node ACP smoke 验证 Host 重启恢复及远程 MCP 工具调用 |
| 执行占位与清理验证 | `test/{agent-claims,session-start-cleanup,transport-close-retry}.test.ts` | 跨 Sandbox 的同 Session 竞争与 owner 释放、Agent 作用域与独立 KV；启动原错误与清理错误聚合、claims 保留、并发 close 共享事务及失败重试；JSON-RPC wire close 失败重试且不重开准入 |


协调域契约：共享 `AtomicManagedAgentKv` keyspace 覆盖可能执行同一 Session 的全部实例；Session key 不含 `Sandbox.id`。不同数据库相同 ID 共用 KV 时保守拒绝，可按业务隔离 KV，不能通过不同 Sandbox 绕过同 Session claim。`Sandbox.id` 仅用于 Agent key，不是 Peri `machine_id`，SDK 不将其注入 Peri；机器归属校验与执行权协调互不替代。共享 KV 适配器的崩溃恢复/租约策略由部署方提供，claim 过期本身不证明旧执行停止。

清理实现：`src/agent/session.ts::startOnce` 启动失败且清理未确认时保留 `cleanup-pending`、claims 与 transport，以 `AggregateError` 同时报告原错误和清理错误；`close` 共享清理事务，失败后可重试，`cleanupExecution` 确认 transport 清理后才按 owner 释放 claims。`src/transport/json-rpc-transport.ts::close` 在 wire 清理失败后允许重试，不重新开放 ACP 准入。

保证边界：跨实例进程接管与生产崩溃恢复尚未实现；未知 process proof 即使永久为 false，也保留 claims，不自动接管，不以占位释放或过期推断旧执行已停止。

实时投影边界：Peri 的 `session/load` 历史逐条回放若中途发送失败，Host 当前只记录告警后仍返回成功；SDK 尚不能证明回放完整。长期会话反复 compact/rewind 会在同一 Y.Doc 内产生删除记录；当前保持根集合对象身份以供订阅者使用，尚无 Fenix Doc Hub 式文档换代与快照压缩协议。
