# peri-acp

## Scope

`peri-acp` 负责 ACP 服务层：session 生命周期、prompt 构建、Agent 与中间件装配、事件映射/发送及 Langfuse bridge；不实现 TUI 组件。

## 数据流

`ACP request → SessionManager → frozen session data / prompt → agent builder → run_react_loop → ExecutorEvent → event mapper / event sink → SessionUpdate 或扩展通知 → client`。Langfuse 通过 `LangfuseBridge` 将可追踪事件统一交给 tracer；不改变客户端事件路径。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| session、事件、Prompt、工具、中间件、secret | `../docs/standards/architecture-contracts.md` |
| Rust、async 与 doc tests | `../docs/standards/rust.md` |
| 测试位置与覆盖要求 | `../docs/design/testing-standards.md` |
| middleware 具体链顺序 | `../peri-middlewares/CLAUDE.md` 与 `src/agent/builder.rs` |
| TUI 通知消费 | `../peri-tui/CLAUDE.md` 与 `../docs/standards/tui.md` |

不通过导入扩展默认上下文；需规则时按表显式读取。

## 稳定不变量

- `SessionManager` 在每条 session/new、load、resume 或 fork 路径注册 session caps；发送扩展事件前按该 session 的 caps 门控。
- 新增 `ExecutorEvent` 或 ACP 扩展事件时，覆盖发射、ACP mapper/forwarder、caps 门控（如适用）和客户端消费；不能只增加枚举或单一发送点。
- session 创建时构建并复用 frozen 数据；Prompt 与 SubAgent 不得在会话中途重读导致前缀漂移。
- 生产中间件顺序以 `src/agent/builder.rs` 的链构造为事实源，未经完整验证不得重排。
- Langfuse 事件只经 `LangfuseBridge` 的统一映射进入 tracer；日志、错误和遥测不得泄露 secret。

## 目标命令

```bash
cargo check -p peri-acp
cargo test -p peri-acp --lib
cargo test -p peri-acp --lib mapper
cargo test -p peri-acp --test langfuse_e2e
cargo test -p peri-acp --doc
```

## Verify

- session/caps 改动：运行相关 crate 测试，并人工检查所有创建、加载、恢复、fork 入口均在 session 就绪后注册 caps。
- 事件改动：运行 mapper 测试，并人工沿服务端发送点到 TUI/stdio 客户端检查新增事件覆盖；现有 mapper 测试不自动证明全链路完整。
- Prompt、middleware 或 Langfuse 改动：按 `ARC-FROZEN-001`、`ARC-MIDDLEWARE-001`、`ARC-SECRET-001` 逐项核对。
