# peri-agent

> 工作区绑定与执行环境遵循 `ARC-WORKSPACE-001`。外部执行调度由宿主负责，当前 run 的队列与取消由 Agent 负责，Agent 不申请执行 owner/lease、不做 Store owner CAS 或 fencing。Session 异步任务的资源生命周期 owner、内存目录与关闭边界见 `../docs/design/session-async-tasks.md`；代码入口见 `../docs/code-index/peri-agent.md`。

## Scope

`peri-agent` 提供会话、消息、RCRA 执行阶段、LLM 抽象、工具 trait 与中间件接口；不承担 ACP transport 或 TUI 状态。

## 数据流

`MessageQueue → Receive → Compact → Reason → Act → MessageQueue`。`Receive` 排空队列并决定循环是否退出；`Reason` 投影消息和可见工具；`Act` 分发工具并把后续工作送回队列。会话的 frozen 数据在创建后保持不变。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| RCRA、消息归属、Task/MQ、loop 激活 | `../docs/standards/architecture-contracts.md` + `../docs/design/rcra-message-activation.md`（已批准目标/实施验收中；主/子 Agent 同构，持久执行恢复已撤销，剥离状态查 active issue） |
| Prompt frozen、工具 direct/deferred | `../docs/standards/architecture-contracts.md` |
| 中间件链装配入口与链序蓝本（ARC-MIDDLEWARE-001） | `src/session/factory.rs`；装配实现在 `../peri-middlewares/src/assembly.rs` |
| hook 状态能力与回写（ARC-MIDDLEWARE-CAPABILITY-001） | `../docs/standards/architecture-contracts.md`；`src/middleware/capabilities.rs` 与 `src/agent/stages/middleware_runner.rs` |
| Rust、async、文本宽度、doc tests | `../docs/standards/rust.md` |
| 测试位置与覆盖要求 | `../docs/standards/testing.md` |
| compact 阈值与环境覆盖 | `src/agent/compact_v2/config.rs` 的 `CompactConfig` |

不通过导入扩展默认上下文；需规则时按表显式读取。

## 稳定不变量

- `run_react_loop` 是阶段循环入口；退出判断保留在 Receive。
- 只有当前 MQ 输入驱动执行；历史加载不重放旧模型请求、工具调用或委托。`resume_subagent` 是用户显式加载子会话历史并开始新 run。
- 工具 `invocation_id` 是当前调用唯一身份，`tool_call_id` 是模型原始调用身份；子事件直接使用后者，不查持久执行账本。
- 消息、模型回答与工具结果继续写 transcript；普通 flush 失败必须返回执行错误，不以队列接纳替代落库确认。
- 启动闸门 `before_react_start` 在首批 `before_agent`/`before_input` 之后、Compact 之前只执行一次；它的 Err 不降级（`Interrupted` → `LoopResult::Interrupted`，其它 → `LoopResult::Error`），候选工具只活在本次 gate 的 `StartupState` 里、随 state 丢弃，而既有 `before_agent` 的软失败降级不受影响。
- `StageContext` 是阶段依赖边界；阶段间通过输入/输出和上下文传递，不绕过为全局状态。
- `FrozenContext` 的 prompt、指引、skills 与日期在会话内不可漂移；SubAgent 复用上游冻结数据。
- `BaseTool::is_direct()` 是工具可见性事实源（builtin MCP 实例工具例外：直连性由 `peri-acp-types/src/builtin_mcp.rs` 的逐工具 `direct` 声明决定，见 ARC-TOOLS-001）；deferred 工具经搜索/执行代理访问。
- Compact 行为、阈值和环境覆盖仅引用 `CompactConfig`，本文不复制数值。
- 中间件链装配在 session 初始化时经 `src/session/factory.rs` 构建（`production_blueprint` 是链序事实源，ARC-MIDDLEWARE-001；装配实现当前位于 `../peri-middlewares/src/assembly.rs`，依赖反转完成后物理迁入本层）。

## 目标命令

```bash
cargo check -p peri-agent
cargo test -p peri-agent --lib
cargo test -p peri-agent --lib <test_name>
cargo test -p peri-agent --doc
```

## Verify

- RCRA 或 compact 变更：运行目标测试；无对应自动测试时，人工追踪 `run_react_loop` 到各 stage 的输入、输出和队列回流。
- frozen 或工具可见性变更：同时按 `ARC-FROZEN-001`、`ARC-TOOLS-001` 检查调用方与包装层。
