# Peri TS SDK 代码索引

状态：WASM 已通过 `WasmAcpTransport` 接入现有 `peri-acp` Host，`Sandbox`、`ManagedAgents`、`Agent` 和 `Session` 沿用同一接口。TypeScript 只关联 JSON-RPC 消息，不分派 ACP 方法；Turso 会话写入、恢复由 Rust 的现有存储端口处理。

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| 公开 API | `npm-packages/@peri-sdk/src/sdk/index.ts` | 导出 Agent、Session、ManagedAgents、Sandbox 和 `WasmAcpTransport` |
| WASM 产物装配 | `src/wasm/loader.ts` + `scripts/build.ts` | 从根 workspace 编译 Emscripten release，打包 JS/WASM；懒加载并复用 `PeriWasmAcp` 模块 |
| Peri 配置类型 | `src/config/` | `PeriConfig` 描述受信 settings 启动帧，`MetaHarnessKey` 限定能力键；`BareHarnessConfig` 是冻结的全关闭预设，demo 按需重新开启 MCP 与 ToolSearch |
| 会话与输入 | `src/agent/{agent,session,send-receipt,types}.ts` | `session/new|load`、连续输入队列、交付事件、取消与事件流；未领取的输入若从 `dispatching` 退回 `queued`，按队列事件重派发一次，再失败则取回并报告错误；Agent 经 Sandbox 直接向 SessionStorage 列举会话，不走 ACP `session/list`；每个类单独成文件 |
| 并发占位 | `src/managed/` + `src/kv/` | ManagedAgents 同步声明 Agent，异步 start 时共享 KV 原子 claim，冲突报错，owner 释放；MemoryKV 仅单进程使用 |
| Sandbox 与存储 | `src/sandbox/{sandbox,workspace-mcp-process}.ts` + `src/storage/` | Sandbox 持有 SessionStorage 接口；`getSessions()` 按 Sandbox path 读数据库，远端使用官方 `@tursodatabase/serverless`；可连接已有 HTTP Workspace，或启动 `peri mcp-start workspace --http` 并完成 initialize 与 live `tools/list` 后再建 ACP Session。Workspace 由 Sandbox 独立关闭；Session start 时由 Sandbox 提供 Store 部署参数 |
| ACP 传输 | `src/transport/{json-rpc-transport,stdio-transport,wasm-transport,event-queue,types}.ts` | 共用 JSON-RPC ID、反向请求、通知和关闭语义；stdio 用 Bun 子进程与 settings 前置帧，WASM 用 `PeriWasmAcp` 的原始帧端口 |
| ACP 可运行示例 | `examples/demo/{demo.ts,demo-wasm.ts,demo.html}` + `mise.toml` | 两个 Hono/Bun 服务复用同一前端、Session 管理与 POST/SSE 路由；`demo.ts` 启动原生 Peri stdio，`demo-wasm.ts` 用 `Sandbox.transportFactory` 启动 WASM ACP Host。WASM 的文件工具经可选外部 Workspace HTTP MCP 提供，本地 builtin 不启动 |
| 验证 | `test/domain.test.ts` + `tests/*.test.ts` + `scripts/smoke-wasm-*.mjs` | Bun SDK 测试含真实 SDK→WASM ACP→sqld/模型集成；Node ACP smoke 验证 Host 重启恢复及远程 MCP 工具调用；Bun demo 实测创建、发送、SSE 回复与持久列表 |

未解决的契约缺口：SDK 不把 `Sandbox.id` 传入 Peri 作为 `machine_id`；Peri 从自己的身份来源解析该值。因此 `Sandbox.id` 只用于 SDK 侧占位作用域，尚不等于 Peri Store 的机器身份。共享 KV 适配器的崩溃恢复/租约策略也由部署方提供。
