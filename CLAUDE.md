<!--
ROLE: repository router。
MUST 规则 → docs/standards/；active changes → spec/issues/；history lookup → spec/global/problems.md。
根文件禁止复制模块 inventory、issue narrative、实现细节或规范正文。
-->

# CLAUDE.md — Perihelion

终端 AI 编程助手；主路径 `peri-tui → peri-acp → peri-agent::run_react_loop`。执行阶段与退出语义见 `peri-agent/CLAUDE.md`。

## 先读什么

信息优先级：代码/契约测试 > `docs/standards/` > 模块 `CLAUDE.md` > `docs/design/` > active spec > history；冲突服从更高项。`AgentsMdMiddleware` loader 不自动继承父目录：进入模块任务前，显式 Read 对应模块 `CLAUDE.md` 和适用 standard。

- 标准入口：`docs/standards/index.md`；架构契约：`docs/standards/architecture-contracts.md`。
- Rust：`docs/standards/rust.md`；TUI：`docs/standards/tui.md`；文档维护：`docs/standards/documentation.md`。
- 测试：`docs/design/testing-standards.md`；活动需求读对应 `spec/issues/`；历史仅查 `spec/global/problems.md`。

## 跨仓库契约

修改 ACP 边界、frozen prompt、事件链、工具可见性或序列化、中间件顺序、安全配置前，先读 `docs/standards/architecture-contracts.md`；根文件不复制规则正文。

## 任务路由

| 任务 | 先读 | 稳定入口 |
| --- | --- | --- |
| Agent loop、Compact、provider、tools | `peri-agent/CLAUDE.md` + architecture/rust | `peri-agent/src/agent/`、`peri-agent/src/agent/compact_v2/`、`run_react_loop` |
| session、prompt、event、caps、Langfuse、composition | `peri-acp/CLAUDE.md` + architecture/rust | `peri-acp/src/session/`、`peri-acp/src/prompt/`、`peri-acp/src/event/`、`peri-acp/src/langfuse/` |
| MCP、plugin、skills、subagent、HITL、workflow、LSP | `peri-middlewares/CLAUDE.md` + architecture/rust | `peri-middlewares/src/` |
| TUI | `peri-tui/CLAUDE.md` + tui/rust | `peri-tui/src/kit/`、`peri-tui/src/kit/acp_events/` |
| E2E | `e2e/CLAUDE.md` + testing standards | `e2e/` |
| 文档站 | `peri-cool/CLAUDE.md` + documentation | `peri-cool/`（submodule） |
| Rust 通用、测试、CLAUDE 维护 | 对应 standard | `docs/standards/`、`docs/design/testing-standards.md` |

Crate 拓扑：`peri-tui → peri-acp → peri-agent`；`peri-middlewares` 由 ACP 装配；其他 workspace crates 以 `Cargo.toml` 为事实源。

## Workspace 命令

```bash
cargo build --workspace
cargo test -p <crate> --lib -- <test_name>
cargo test --workspace --doc
cargo run -p peri-tui -- -a
lefthook run pre-commit
```

E2E 命令只在 `e2e/CLAUDE.md` 维护。

## 检查

**变更前**：读取适用 guide、standard 与 active spec；先检查邻近代码和 manifest，遵循其模式。

**完成前**：运行目标测试；跨层事件同步验证完整链路；改 doc comment 时运行 doc tests；按 `DOC-UPDATE-001` 检查路由事实源。除非用户明确要求，不 commit；临时事故只写 active spec/history，不写回根文件。
