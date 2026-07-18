# Agent / ReAct 循环领域

## 领域综述

Peri Agent 的 ReAct 循环、LLM 适配器、工具系统、Context 管理、SubAgent 构建。核心数据流为消息构建 → ReAct 循环（Compact → Receive → Reason → Act → End）→ 工具分发 → 响应生成。Provider 适配层统一 OpenAI/Anthropic 流式与非流式路径。

## 核心流程

- **ReAct 循环**：每轮 `before_agent → loop(500) { before_model → LLM → after_model → [工具分发] | [回答] } → End`
- **消息构建**：`BaseMessage`（Human/Ai/System/Tool），`ContentBlock`（Text/Image/Document/ToolUse/ToolResult/Reasoning）
- **Provider 适配**：`invoke.rs`（非流式）和 `stream.rs`（流式）双路径，需产出相同的 `BaseMessage` 序列
- **SubAgent 构建**：fork/non-fork 双模式，共享 `build_v2_subagent_context` 入口，`parent_messages` 注入 transcript

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 循环引擎 | ReAct Loop（`run_react_loop`），max 500 iterations |
| 消息类型 | `BaseMessage` + `ContentBlock` 枚举（ACP 协议对齐） |
| LLM Provider | OpenAI chat completions + Anthropic Messages API，统一适配层 |
| SubAgent | fork 模式（隔离构建）+ inline 模式（共享上下文），v2_bridge 统一入口 |
| 工具分发 | 3 阶段分发（Core/Meta/Deferred），`tool_dispatch.rs` 延迟消息写入 |

---

## Issue 经验附录

### issue_2026-07-07-fork-subagent-no-parent-conversation-history
**摘要:** Fork 模式 SubAgent 收不到父对话历史
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** fork SubAgent, parent_messages, 对话历史, v2_bridge
**问题本质:** fork SubAgent 的 `parent_messages` 在 `v2_bridge.rs` 的 transcript 注入点未被正确传递，导致 fork SubAgent 只能看到 fork directive + prompt，看不到父对话历史。
**通用模式:** SubAgent 的 transcript 构建是三层叠加（frozen prompt + parent_messages + task prompt），每个注入点必须逐层验证：fork 路径和 non-fork 路径在 `parent_messages` 注入处是否走同一代码路径。
**涉及文件:** peri-middlewares/src/subagent/tool/execute_fork.rs, peri-middlewares/src/subagent/v2_bridge.rs, peri-middlewares/src/subagent/fork.rs
**CLAUDE.md 链接:** false

### issue_2026-07-05-tool-call-ai-text-invisible-after-commit
**摘要:** 消息流渲染中，AI 消息文本在多分支渲染路径下不可见
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** content_text 丢失, stream.rs ToolUse 分支, ViewCommit, OpenAI 流式
**问题本质:** `build_stream_response` 在 ToolUse 分支中，流式期间累积的 `content_text` 从未被推入 `blocks` 数组。流式路径正确（`TextChunk` → `current_turn` 可见），但 ViewCommit 后 `BaseMessage` 不含 Text block → `AssistantBubble(text="")` → TUI 只显示 ToolCard。非流式路径（`invoke.rs`）正确，仅 OpenAI 流式路径受影响。
**通用模式:** 流式路径和非流式路径在构建最终 `BaseMessage` 时必须产出相同的 `ContentBlock` 序列——这是协议层的一致性约束。新增 ContentBlock 变体时需检查所有 provider 的流式/非流式路径是否都正确填充。
**技术决策:** 修复不仅添加了 `content_text` 到 blocks，还新增了 3 个单元测试防止回归——这类"看起来很简单但影响很大的 bug"最适合用测试固化。
**涉及文件:** peri-agent/src/llm/openai/stream.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn
**摘要:** Agent 工具（background 模式）启动 bg agent 后 loading 不停止
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** background agent, loading 生命周期, TurnDone, ReAct 循环
**问题本质:** AI 调用 Agent 工具（background 模式）后本轮 turn 正常结束（TurnDone），但 bg callback 触发的 `SyntheticUserMessage` 在 agent End 阶段 emit，导致连续 loading 周期——首轮 TurnDone 和 bg callback 的 TurnStart 之间没有 loading=false 间隙。与双通道 flush-then-push 修复关联。
**通用模式:** ReAct 循环中 background agent 的 loading 生命周期需要显式的 TurnDone→TurnStart 过渡：background agent 启动后当前 turn 应正常 TurnDone（loading 停止），callback 唤醒时创建新的 TurnStart（loading 重新开始）。二者必须独立发送，不能合并为连续 loading。
**涉及文件:** peri-agent/src/agent/stages/mod.rs, peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks
**摘要:** 工具调用统一 300s 超时导致 Agent/SubAgent 正常任务被强制中断
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 工具超时, 差异化超时, BaseTool::timeout, Agent/SubAgent
**问题本质:** dispatch_concurrent 对所有工具统一使用 TOOL_CALL_TIMEOUT=300s。Agent/SubAgent 可跑 200 轮 ReAct，正常任务 >300s 时被硬性 kill。
**通用模式:** 超时策略应按工具类型差异化——BaseTool trait 提供 timeout() 方法让每个工具自声明超时需求。快工具（Read/Edit/Glob 等）默认 120s，需要长运行的工具（Agent/Bash/Workflow 等）返回 None 自管超时。新增工具时必须明确其超时策略。
**技术决策:** 从一刀切超时改为 per-tool 的 timeout() trait 方法，12 个工具覆写为 None（自管），12 个继承默认 120s。
**涉及文件:** peri-agent/src/agent/stages/tool_dispatch.rs, peri-agent/src/tools/mod.rs
**CLAUDE.md 链接:** true

### issue_2026-07-16-eventbus-unified-emission
**摘要:** 统一事件发射路径：所有 Agent 事件走 v2 EventBus
**状态:** Done
**归档日期:** 2026-07-17
**关键词:** EventBus, 事件路径统一, 三层EventBus, CompactStrategy, ObserveEvent
**问题本质:** v2 EventBus 仅覆盖 ReAct 5 阶段，LLM 流式/SubAgent/ACP Turn/斜杠命令等路径直接构造 ExecutorEvent 绕过 EventBus——三条独立发射路径并存。
**通用模式:** 事件发射应有单一入口（EventBus），避免分散的 ExecutorEvent 直接构造。新增 AgentEvent 变体时只需在 events_v2.rs 加变体 + events_v2_mapper.rs 加映射。CompactStrategy 等枚举只保留一份定义，通过 EventBus 传递真实策略值而非 hardcode。
**架构影响:** EventBus 是 peri-agent 内部的事件优化层，ExecutorEvent 保留为稳定跨 crate 边界类型。统一事件路径让 Langfuse 等观测层只订阅一个 EventBus 即可获取完整 trace 数据。
**涉及文件:** peri-agent/src/agent/events_v2.rs, peri-agent/src/agent/stages/*
**CLAUDE.md 链接:** true

### issue_2026-07-18-anthropic-adapter-tool-result-order-reversed
**摘要:** Anthropic adapter 合并连续 Tool 消息时 tool_result 顺序反转
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** Anthropic adapter, tool_result 顺序, insert(0) vs push, 并行工具调用
**问题本质:** `arr.insert(0, x)` 将每个新结果插到数组最前面，导致多 tool_use 对应的结果顺序反转
**通用模式:** 序列化合并时优先用 `push()` 保持原始顺序；数组插入位置选择影响下游语义正确性
**涉及文件:** peri-agent/src/llm/anthropic/adapter.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-anthropic-adapter-system-cache-split-missing
**摘要:** Anthropic adapter 未对 request.system 调用 split_system_blocks，导致 main agent 系统提示词边界标记失效
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Anthropic adapter, system prompt cache, split_system_blocks, boundary marker
**问题本质:** `build_request_body()` 对 `request.system` 直接 push 单块，未调用已存在的 `split_system_blocks()` 拆分静态/动态内容
**通用模式:** 同一功能（缓存拆分）有两条代码路径时，主路径可能漏掉已实现的辅助函数；fork agent 走 `BaseMessage::System` 路径反而正确拆分
**架构影响:** `request.system` 和 `BaseMessage::System` 是两条独立序列化路径，未统一抽象
**涉及文件:** peri-agent/src/llm/anthropic/adapter.rs, peri-agent/src/llm/anthropic/cache.rs
**CLAUDE.md 链接:** false

### issue_2026-07-18-tools-hashmap-order-breaks-prompt-cache
**摘要:** tools 数组顺序随 HashMap 迭代顺序，新增工具触发 rehash 后 prompt cache 前缀全断
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** HashMap 顺序, Prompt Cache, 确定性序列化, BTreeMap
**问题本质:** `HashMap.values().collect()` 迭代顺序不确定（RandomState），跨进程 resume 或运行期新增 key 触发 rehash 后 tools 数组顺序变化，Anthropic 前缀缓存全断
**通用模式:** 所有需要跨进程/跨请求复用的序列化内容必须保证顺序稳定——按名称排序（BTreeMap）或固定收集顺序
**技术决策:** `SharedToolMap` 应考虑从 `HashMap` 改为 `BTreeMap`（按名排序天然确定）
**涉及文件:** peri-agent/src/agent/stages/reason.rs, peri-agent/src/agent/stages/mod.rs
**CLAUDE.md 链接:** true

### issue_2026-07-15-goal-continuation-loop-broken-in-v2
**摘要:** Goal 自驱续跑在 v2 架构下完全断裂
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** goal 续跑, MessageKind::Defer, drain_for_end, block_continue
**问题本质:** GoalMiddleware 注入的 steering 消息使用 `MessageKind::Info`（定义永不唤醒循环），ActOutput 丢弃 `block_continue` 字段——两处断裂导致续跑完全失效
**通用模式:** 注入控制消息时需理解 MessageKind 语义——Info（不唤醒）vs Defer（唤醒续跑）。ActOutput 结构不完整导致中间件设置的信号在 act 阶段被吞掉
**架构影响:** 消息队列的 MessageKind 语义是控制流程的关键契约，需文档化各 Kind 的行为差异
**涉及文件:** peri-middlewares/src/goal_middleware.rs, peri-agent/src/agent/stages/act.rs, peri-agent/src/session/queue.rs
**CLAUDE.md 链接:** true

### issue_2026-07-16-subagent-tool-alias-not-resolved
**摘要:** 子 agent 工具别名解析失败
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 工具别名, SubAgent, resolve_tool, tool wrapper
**问题本质:** 子 agent 的工具包装器（ArcToolWrapper/BoxToolWrapper）未实现 `aliases()` 透传，导致子 agent 内部 `resolve_tool()` 找不到别名
**通用模式:** 工具包装/过滤层必须完整透传 BaseTool trait 的所有方法（name, description, aliases, timeout 等），遗漏一个就会导致子 agent 行为断裂
**技术决策:** 工具别名通过 `BaseTool::aliases()` trait 自声明——包装器需委托而非忽略
**涉及文件:** peri-middlewares/src/tools/mod.rs, peri-agent/src/agent/stages/tool_dispatch.rs
**CLAUDE.md 链接:** true

### issue_2026-07-07-bg-agent-complete-no-resume
**摘要:** bg agent 完成后主 agent 永久卡死、合成消息未注入主消息区
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** bg agent, 续跑, MessageQueue, Defer 消息
**问题本质:** bg agent 完成事件已到达 TUI（通知条正常），但合成消息未通过 MessageQueue 注入主 agent inbox 触发续跑
**通用模式:** 后台任务完成→合成消息注入→主 agent 续跑 是完整链路，任一段断裂都不行。事件到达 TUI 不等于 agent 收到了续跑信号
**涉及文件:** peri-acp/src/session/executor.rs (bg event pump)
**CLAUDE.md 链接:** false

### issue_2026-07-18-subagent-write-sandbox-tool
**摘要:** SubAgent 沙箱写工具（WriteSandbox）：让 readonly agent 能输出交接文件
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** WriteSandbox, subagent, 沙箱写, allowedWriteDirs
**问题本质:** readonly subagent 被 disallowedTools 禁用 Write/Edit/Bash，无法落盘产出交接文件
**通用模式:** 能力最小化设计——allowedWriteDirs 声明目录白名单，per-agent 实例化工具；安全逻辑（路径穿越校验）在工具层而非调度层
**技术决策:** frontmatter 新增 `allowedWriteDirs` 字段 + `can_mutate` 忽略沙箱目录（保持 [readonly] 标签并行调度）
**涉及文件:** peri-middlewares/src/tools/filesystem/write_sandbox.rs, peri-middlewares/src/subagent/
**CLAUDE.md 链接:** false

### issue_2026-07-18-ask-user-migration
**摘要:** ask_user → interaction 类型迁移——消除 24 个 deprecation 警告
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** 类型迁移, deprecation, interaction 统一, 技术债清理
**问题本质:** AskUserQuestion → interaction 统一方案后旧类型未迁移，24 个 deprecation 警告
**通用模式:** 大规模类型迁移应分步：先标记 deprecated → 逐个消费方迁移 → 最终删除旧类型
**涉及文件:** peri-middlewares, peri-tui, peri-agent 多处 re-export 和消费方
**CLAUDE.md 链接:** false

### issue_2026-07-07-subagent-group-header-shows-agent-instead-of-task-description
**摘要:** SubAgent 卡片完全不显示（SubagentStarted 事件被 notifier 丢弃）
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** SubagentStarted, acp_notifier, 事件丢弃, AgentEvent 通道
**问题本质:** Phase 2.6 kit 迁移后 `acp_notifier.rs` 的 `AgentEvent` 变体被标记为"暂未处理"静默丢弃，SubagentStarted 永不到达 TUI
**通用模式:** 架构迁移时新增事件通道必须同步更新 notifier 分发，否则静默丢弃难以排查。单元测试绕过 notifier 层直接调用 downstream 掩盖了上游断层
**涉及文件:** peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** true

### issue_2026-07-13-sync-agent-tool-cards-not-showing
**摘要:** 同步 Agent 子工具调用卡片完全不显示
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 同步 SubAgent, 子工具卡片, or_insert_with, event_tx 关闭
**问题本质:** `builder_v2.rs` 用 `or_insert_with` 保留第一 turn 的 SubAgentTool 实例，其 `event_tx` 在第二 turn 时已被 close——所有 SubagentStarted 事件被静默丢弃
**通用模式:** `or_insert_with` 不适合需要每 turn 重建的有状态对象（含 channel/sender）。每 turn 复用时应确保内部 channel 有效，或改用 `insert` 强制替换
**涉及文件:** peri-acp/src/agent/builder_v2.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** true

### issue_2026-07-17-compact-flags-lost-on-session-restore
**摘要:** Compact 标记（truncated/excluded）在 Session 恢复后丢失
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** compact, 标记持久化, session 恢复, MessageFlags, cached_context
**问题本质:** 两个独立根因——DB 加载路径只 SELECT content 不读 flags 列；cached_context JSON 格式不支持 flags（BaseMessage 不含 truncated/excluded 字段）
**通用模式:** 持久化标记（metadata）必须独立于内容字段存储和恢复。cached_context 缓存命中时跳过 DB 查询会漏掉标记
**架构影响:** 需要在 trait 层增加 `load_message_flags()` 方法，transcript 恢复后（Phase 5.5）从 DB 恢复标记
**涉及文件:** peri-agent/src/thread/sqlite_store.rs, peri-agent/src/session/transcript.rs, peri-acp/src/session/executor_helpers.rs
**CLAUDE.md 链接:** true

### issue_2026-07-18-compact-effect-lost-between-prompts-v2
**摘要:** Compact 效果在 v2 路径中跨 prompt 丢失
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** compact, persist_tx=None, V2Session, flags 持久化
**问题本质:** v2 主路径创建 V2Session 时 `persist_tx` 始终为 None——compact 的 flag 写入通过 `send_persist` 通道为 no-op。turn 结束后 transcript 销毁，flags 全丢
**通用模式:** 已有基础设施（`with_persistence`、`load_message_flags` 等）就位但未被上游调用——架构迁移时需确认新路径是否激活了所有持久化通道
**涉及文件:** peri-acp/src/agent/builder_v2.rs, peri-agent/src/session/transcript.rs
**CLAUDE.md 链接:** true
