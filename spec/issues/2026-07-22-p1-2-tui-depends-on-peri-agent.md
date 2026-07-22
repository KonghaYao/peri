# P1-2：peri-tui 直接依赖 peri-agent 违反分层

**状态**：Open
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 模块化与分层维度

## Problem Statement

`peri-tui/Cargo.toml:27` 声明了 `peri-agent = { path = "../peri-agent" }`，TUI 层直接依赖 agent 框架层。理想分层为：

```
peri-tui（展现层）
  → peri-acp（服务层）
    → peri-agent（框架层）
```

实际为：

```
peri-tui → peri-agent（跨层直接依赖）
peri-tui → peri-acp
peri-acp → peri-agent
```

这导致：
- TUI 可以直接 import `peri-agent` 的内部类型（如 `AgentError`、`BaseMessage`），绕过 ACP 协议
- 分层边界模糊，未来若需替换 agent 实现，TUI 也需修改
- 新开发者可能无意中在 TUI 中引入 agent 层耦合

## 建议方案

1. 审计 `peri-tui` 中对 `peri-agent` 的引用，确认哪些是必要耦合、哪些可经 ACP 协议透传
2. 对必要的类型引用，在 `peri-acp` 中重导出或封装
3. 长期目标：移除 `peri-tui → peri-agent` 依赖，所有通信经 ACP 协议

## 涉及文件

- `peri-tui/Cargo.toml:27` — 依赖声明
- `peri-tui/src/kit/v2_bridge.rs` — `ObserveEvent → AcpEventData` 映射（需迁移到 peri-acp）
- 所有 `use peri_agent::*` 的 TUI 文件

## 风险

- **中**：需审计所有跨层引用，可能涉及类型迁移。建议分阶段执行
