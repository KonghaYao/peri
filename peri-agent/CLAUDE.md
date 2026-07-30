# peri-agent

## Scope

`peri-agent` 提供会话、消息、RCRA 执行阶段、LLM 抽象、工具 trait 与中间件接口；不承担 ACP transport 或 TUI 状态。

## 数据流

`MessageQueue → Receive → Compact → Reason → Act → MessageQueue`。`Receive` 排空队列并决定循环是否退出；`Reason` 投影消息和可见工具；`Act` 分发工具并把后续工作送回队列。会话的 frozen 数据在创建后保持不变。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| RCRA、Prompt frozen、工具 direct/deferred | `../docs/standards/architecture-contracts.md` |
| Rust、async、文本宽度、doc tests | `../docs/standards/rust.md` |
| 测试位置与覆盖要求 | `../docs/design/testing-standards.md` |
| compact 阈值与环境覆盖 | `src/agent/compact_v2/config.rs` 的 `CompactConfig` |

不通过导入扩展默认上下文；需规则时按表显式读取。

## 稳定不变量

- `run_react_loop` 是阶段循环入口；退出判断保留在 Receive。
- `StageContext` 是阶段依赖边界；阶段间通过输入/输出和上下文传递，不绕过为全局状态。
- `FrozenContext` 的 prompt、指引、skills 与日期在会话内不可漂移；SubAgent 复用上游冻结数据。
- `BaseTool::is_direct()` 是工具可见性事实源；deferred 工具经搜索/执行代理访问。
- Compact 行为、阈值和环境覆盖仅引用 `CompactConfig`，本文不复制数值。

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
