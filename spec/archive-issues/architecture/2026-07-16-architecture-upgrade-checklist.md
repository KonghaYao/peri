> 归档于 2026-07-30，原路径 spec/issues/2026-07-16-architecture-upgrade-checklist.md

# 架构升级清单：2026-07-16 三维护审视

**状态**：Fixed
**优先级**：高
**类型**：架构改进跟踪
**创建日期**：2026-07-16

## Problem Statement

2026-07-16 通过 3 个并行 subagent 对 workspace 进行了三维度全面架构审视（peri-agent 内部、跨 crate 拓扑、SubAgent/中间件链），识别出 **35 个待升级点**（4 P0 + 13 P1 + 18 P2）。

本 issue 作为总清单收纳全部发现，后续可拆分为独立的可执行 sub-issue。

## 来源

- `peri-agent` 内部审视 → `.tmp/devflow/architecture-audit-peri-agent.md`
- 跨 crate 拓扑审视 → `.tmp/devflow/architecture-audit-cross-crate.md`
- SubAgent/中间件链审视 → `.tmp/devflow/architecture-audit-subagent.md`
- 汇总 → `.tmp/devflow/architecture-audit-summary.md`

---

## P0（紧急 — 4 项）

| # | 发现 | 维度 | 影响 |
|---|------|------|------|
| **P0-1** | peri-tui 绕过 ACP transport 直接依赖 agent：`acp_server/acp_stdio` 多处 `use peri_agent::*`，绕过 MpscTransport 协议边界 | 跨 crate | TUI→ACP→Agent 单向依赖被破坏为网状 |
| **P0-2** | executor/helpers 拆分边界模糊：`use super::*` 耦合，InterceptRequest 14 字段 god object，build_and_execute_agent_v2 与 builder 模块边界不清 | 跨 crate | 维护性降低，新功能难定位 |
| **P0-3** | SubagentStarted/Stopped 12 处分散发射：4 文件 × 正常/错误 × 3 路径，后处理动作（lifecycle hook/deregister/thread_store）不完全一致 | subagent | 新增路径极易遗漏后处理 |
| **P0-4** | define.rs error 路径 deregister 由 RAII guard 隐式处理，其余 3 条路径显式调用——风格不一致 | subagent | 正确性风险 |

---

## P1（重要 — 13 项）

### peri-agent 内部（7 项）

| # | 发现 | 建议 |
|---|------|------|
| **P1-1** | StageContext 22 字段 god object | 拆为 SessionHandle + RuntimeServices + CompactContext + AsyncContext |
| **P1-2** | `has_tool_calls` 游离状态：CompactInput/ReasonInput 都带，run_react_loop 顶部手动维护 | 移入 TurnContext 或 explicit LoopState |
| **P1-3** | Act 阶段泄漏使用 v1 AgentContext 适配层（act.rs:31-36），StageContext 已有 token_tracker | 直接读 StageContext.token_tracker |
| **P1-4** | compact_v2.rs ~900 行未拆分 | 拆为 micro.rs / full.rs / re_inject.rs |
| **P1-5** | stages/compact 与 compact_v2 策略判断重复（都根据 budget 判定 Micro/Full） | 统一到 compact_v2::run_compact，stages 只做事件 emit + hook 调度 |
| **P1-6** | tool_calls 与 ToolUse 块双表示冗余：Ai message 同时有 `tool_calls: Vec<ToolCallRequest>` 和 ContentBlock::ToolUse | 明确 tool_calls 是从 content blocks 派生的只读缓存 |
| **P1-7** | Anthropic/OpenAI invoke 平行实现但无共享 ProviderAdapter trait | 提取 trait 封装消息序列化→请求构造→响应解析差异 |

### 跨 crate（3 项）

| # | 发现 | 建议 |
|---|------|------|
| **P1-8** | peri-agent 公共 API 过度暴露：70+ 类型无 stability 分层 | 定义 stability guarantee 层级：stable / unstable / internal |
| **P1-9** | GoalController/GoalStateView 是为破 peri-middlewares → peri-acp 循环依赖而生的"假抽象" | 方案 A：加 BRIDGE 文档标签；方案 B：创建 peri-bridge-types crate |
| **P1-10** | builder.rs 中间件链 70 行围墙式构造（490-651 行） | 按功能分组：context injectors / tool providers / conditional |

### SubAgent / 中间件（3 项）

| # | 发现 | 建议 |
|---|------|------|
| **P1-11** | `extract_last_ai_text` 函数在 define.rs / execute_fork.rs / execute_bg.rs 重复 3 次 | 提取到 tool/mod.rs 为 pub(crate) |
| **P1-12** | register() 与 register_with_kind() 双 API 并存 | register() 标记 deprecated，统一用 register_with_kind() |
| **P1-13** | SubAgent 中间件链不含 GitAttribution/AtMention/ErrorSuggest——需确认是故意还是遗漏 | Review 并文档化 omission 原因 |

---

## P2（建议 — 18 项）

| # | 发现 | 维度 |
|---|------|------|
| **P2-1** | langfuse v2 专用 12 个 ExecutorEvent 变体路径冗余（v2→ObserveEvent→mapper→v1 往返） | agent |
| **P2-2** | v1 ExecutorEvent 变体缺 `#[deprecated]` 标记引导迁移 | agent |
| **P2-3** | `inject_source_agent_id` 是事后补丁：建议作为 mapper 参数而非独立函数 | agent |
| **P2-4** | Smart Compact 分支永远降级为 Micro，空分支污染所有 match | agent |
| **P2-5** | 12 个 Core 工具名在 peri-middlewares 硬编码，非工具自身声明 tier | agent |
| **P2-6** | `append_messages_to_transcript` 放在 stages/mod.rs（~1200 行），建议独立文件 | agent |
| **P2-7** | middleware_runner 10+ hook 函数重复 "make_context→chain.run_*→drain" 模式 | agent |
| **P2-8** | peri-acp-types 独立 crate 价值有限（DTO 几乎 1:1 对应 domain 类型，仅 2 crate 使用） | 跨 crate |
| **P2-9** | langfuse-client 反向耦合 `service.name = "peri-agent"` 硬编码 | 跨 crate |
| **P2-10** | VIEW_MODELS spinner tick（1s）绕过 push_view_models 完整路径 | 跨 crate |
| **P2-11** | 16 面板无标准化 Panel trait（生命周期 open→active→suspend→close 由宏隐式管理） | 跨 crate |
| **P2-12** | Skills 扫描无 mtime 缓存——session/new 每次全量读盘 | 跨 crate |
| **P2-13** | 中间件文件组织风格不统一（src/<name>/mod.rs vs src/middleware/<name>.rs） | 跨 crate |
| **P2-14** | bg task 缺超时机制——卡死会永久占用 registry 位置 | subagent |
| **P2-15** | bg task panic 无法检测——JoinHandle 不被 poll，registry 标记仍为 Running | subagent |
| **P2-16** | WorkflowAgentContext 与 AcpAgentConfig 15 个字段重叠 | subagent |
| **P2-17** | `inject_source_agent_id` match 穷举依赖人工——新增变体易遗漏 | subagent |
| **P2-18** | SubAgentMiddlewareConfig.with_frozen 接收 `Option<String>`，调用方被迫 clone | subagent |

---

## 建议的进攻顺序

| 批次 | 项 | 预计工作量 | 依赖 |
|------|-----|-----------|------|
| **B1** | P0-3, P0-4, P1-11 | 1 天 | 无 |
| **B2** | P1-8, P1-9 | 1 天 | 无 |
| **B3** | P1-1, P1-2, P1-3 | 2 天 | B1 (StageContext 拆分前先统一 SubAgent 生命周期) |
| **B4** | P1-4, P1-5, P1-7 | 2 天 | 无 |
| **B5** | P0-1, P0-2, P1-10 | 3 天 | B2 (API 分层后清理跨 crate 依赖) |
| **B6** | P1-6, P1-12, P1-13 | 1.5 天 | 无 |
| **B7** | P2 各项（按需选取） | 不定 | 无 |

---

## Out of Scope

- 功能新增（本清单仅限架构改善，不涉及新 feature）
- 已解决的议题（CompactStrategy 硬编码、Path D 重构等）
- 性能优化（非本次审视范围）

## Further Notes

- 推荐从 **B1（SubAgent 生命周期统一）** 开始——改动最集中、收益最直接、与后续批次无依赖
- P0-1（TUI 绕过 ACP transport）影响面最大但非阻塞——建议在 API 分层（B2）之后再处理
- 每个批次可拆为独立子 issue，走完整 devflow（explore→plan→code→review→verify）
