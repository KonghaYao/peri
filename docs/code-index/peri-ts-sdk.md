# Peri TS SDK 代码索引

状态：单机服务化阶段的 Bun SDK 切片。SDK 通过 Peri ACP stdio 与独立 Workspace HTTP MCP 接入；生产级服务化仍未完成。

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| 公开 API | `npm-packages/@peri-sdk/src/sdk/index.ts` | 导出 Agent、Session、ManagedAgents、MemoryKV、Sandbox、存储实现与 Transport |
| Peri 配置类型 | `src/config/` | `PeriConfig` 描述受信 settings 启动帧，`MetaHarnessKey` 限定能力键；`BareHarnessConfig` 是冻结的全关闭预设，demo 按需重新开启 MCP 与 ToolSearch |
| 会话与输入 | `src/agent/{agent,session,send-receipt,types}.ts` | `session/new|load`、连续输入队列、交付事件、取消与事件流；未领取的输入若从 `dispatching` 退回 `queued`，按队列事件重派发一次，再失败则取回并报告错误；Agent 经 Sandbox 直接向 SessionStorage 列举会话，不走 ACP `session/list`；每个类单独成文件 |
| 并发占位 | `src/managed/` + `src/kv/` | ManagedAgents 同步声明 Agent，异步 start 时共享 KV 原子 claim，冲突报错，owner 释放；MemoryKV 仅单进程使用 |
| Sandbox 与存储 | `src/sandbox/{sandbox,workspace-mcp-process,process-supervisor}.ts` + `src/storage/` | Sandbox 持有 SessionStorage 接口；可连接已有 HTTP Workspace，或启动独立 Workspace 并确认 MCP readiness。自管 Workspace 在工作目录保留异常退出 guard，重启不能以空任务目录宣称旧任务已结算；私有 supervisor 只为本 Sandbox 的精确 ACP 进程代际提供退出证明 |
| ACP stdio | `src/transport/{stdio-transport,process-broker,event-queue,types}.ts` | Bun 子进程、原始 settings 前置帧、JSON-RPC 双向消息与通知；每个 ACP generation 的 broker 登记本地子进程组，收敛失败不给关闭接管证明 |
| 可运行示例 | `examples/demo/` + `mise.toml` | Hono/Bun 服务以 POST `/api/session/list` 纯查询存储、POST `/api/session/create` 创建或加载 Session 并以 SSE 持续返回会话及原始 ACP 通知、POST `/api/session/send` 向指定 Session 发送输入；demo 直接调用 `ManagedAgents.createAgent()`、`Session.start()` 与 `Session.send()`，局部映射按 Session ID 复用打开中的 Agent；事件日志仅在 demo 进程存活期间支持断线回放，侧栏从 SessionStorage 查询会话；会话存储由本地 libSQL HTTP 服务提供（`bun run db:dev`）；Sandbox 启动独立 Workspace HTTP MCP 供 Peri 消费 |
| 验证 | `test/domain.test.ts` + `tests/*.test.ts` | 生命周期和冲突、真实 stdio 帧、真实 Peri 冷恢复 |

未解决的契约缺口：SDK 不把 `Sandbox.id` 传入 Peri 作为 `machine_id`；Peri 从自己的身份来源解析该值。因此 `Sandbox.id` 只用于 SDK 侧占位作用域，尚不等于 Peri Store 的机器身份。共享 KV 适配器的崩溃恢复/租约策略也由部署方提供。
