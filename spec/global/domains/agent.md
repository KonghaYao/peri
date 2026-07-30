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

### issue_2026-07-18-subagent-tool-cards-regression-empty
**摘要:** SubAgent 工具调用卡片回归不显示——Agent 卡片容器可见但内部为空壳
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** SubAgent 卡片回归, 工具调用卡片, 竞态条件, AgentGroup
**问题本质:** `event_tx` 的替换（or_insert_with→insert）解决了多轮复用问题，但引入了新的竞态——SubAgent 在 tool group 注册到 accumulator 之前就发送了 ToolStart 事件，卡片创建时目标 group 不存在
**通用模式:** channel 替换修复可能引入时序依赖——子线程事件可能在注册完成前到达。需要在 accumulator 中缓存早到事件，待 group 就绪后重放
**涉及文件:** peri-acp/src/agent/builder_v2.rs, peri-tui/src/kit/acp_types.rs
**CLAUDE.md 链接:** false

### issue_2026-07-18-cleanup-subagent-eventbus-dead-code
**摘要:** 清理 SubagentStarted EventBus 残留死代码和双重发送陷阱
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** EventBus, SubagentStarted, 死代码, 双重发送, 架构清理
**问题本质:** EventBus 迁改时 SubagentStarted 因时序死结保留在旧路径，但预先铺设的 v2_bridge→bridge_tx 通道未彻底清除——形成"有收有发但无双发"的死代码。若未来补齐发射端，forwarder.rs 会双路径重复发送
**通用模式:** 架构迁移中部分保留旧路径时，必须同步清理新路径中的对应接收分支——避免形成"幽灵通道"。死代码本身无害，但会让后来者误认为通道已就绪
**涉及文件:** peri-tui/src/kit/v2_bridge.rs, peri-acp/src/event/forwarder.rs
**CLAUDE.md 链接:** false

### issue_2026-07-11-hung-bg-agent-await-wake-block-forever
**摘要:** 后台 agent 在 await_wake 中永久阻塞导致 session 卡死
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** await_wake, 后台 agent, 超时防御, Notify 机制
**问题本质:** await_wake 无超时——若 bg agent hung 住永不 wake，主 agent 在 await_wake 中永久阻塞。Notify 机制依赖完成链：bg agent 完成→wake→主 agent 继续，但 hung agent 永远不触发唤醒
**通用模式:** 需两层防御——bg agent 级超时（600s，覆盖最长 SubAgent 执行）拦截 hung 任务；await_wake 空闲超时（如 180s）确保主 agent 不会永久阻塞
**涉及文件:** peri-agent/src/agent/stages/receive.rs
**CLAUDE.md 链接:** false

### issue_2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost
**摘要:** 多个 bg agent 同轮场景：最后 callback 丢失 + loading 卡死
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 多 bg agent, callback 丢失, loading 卡死, 全局追踪
**问题本质:** 多个 bg agent 同轮时，前 N-1 个 callback 正常处理，但最后一个触发时机已越过生命周期边界——多实例终结/清理逻辑不完整
**通用模式:** 多 bg agent 同轮场景需全局追踪所有活跃实例的完成状态。loading 由主 Agent TurnDone 而非 bg 完成事件驱动——bg callback 只推送结果，不清 loading
**涉及文件:** peri-middlewares/src/subagent/, peri-tui/src/kit/message_area/
**CLAUDE.md 链接:** false

### issue_2026-07-08-mq-injected-user-message-not-in-tui
**摘要:** 后台 callback 合成消息未在 TUI 显示——五次修复迭代
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 后台消息注入, 双通道 flush-then-push, TurnDone 边界
**问题本质:** bg callback 需要将合成消息注入消息区，但直接 push committed 绕过 TurnSegment→位置错乱。核心约束：TurnDone 是唯一切分点，turn 内 AI 内容不可分割
**通用模式:** 最终方案采用双通道 flush-then-push——unstable event 先 flush 当前 turn 切分边界，再用标准 session/update 通道推送气泡。Push committed 前必须先 flush current_turn
**涉及文件:** peri-tui/src/kit/acp_notifier.rs, peri-acp/src/session/
**CLAUDE.md 链接:** false

### issue_2026-07-07-bg-tasks-unified-management
**摘要:** 后台任务统一管理架构——BackgroundTaskRegistry 从 per-prompt 升级到 SessionState 级
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 后台任务, BackgroundTaskRegistry, SessionState, 跨 prompt
**问题本质:** 旧版 registry 生命周期绑定到单次 prompt——bg agent/workflow 在 prompt 结束后丢失追踪，无法统一管理
**通用模式:** 后台任务 registry 应提升到 SessionState 级别（跨 prompt 存活），统一管理 bg agent/workflow/bg shell 三类任务。共享统一 registry 抽象 + 独立并发上限
**涉及文件:** peri-acp/src/session/, peri-middlewares/src/subagent/
**CLAUDE.md 链接:** false

### issue_2026-07-07-acp-protocol-refactor
**摘要:** ACP 协议重构——废弃 11 个冗余自定义事件，复用标准 session/update
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** ACP 协议, 自定义事件废弃, 标准话, view-commit 增量
**问题本质:** 11 个自定义 ACP 事件为标准协议已覆盖的冗余——view-commit 全量推送导致无效传输
**通用模式:** "标准有的走标准，真没有的才自定义"。view-commit 全量推送改为增量 session/update。新增事件前先查 ACP 协议是否有等效机制
**涉及文件:** peri-acp/src/event/, peri-acp-types/
**CLAUDE.md 链接:** false

### issue_2026-07-08-viewmodel-elimination
**摘要:** 消灭 8 种共享 ViewModel 类型——TUI 直接从 ACP 事件派生渲染结构
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** ViewModel 消除, 中间层精简, ACP 直通 TUI
**问题本质:** peri-agent 和 peri-tui 共享 8 种 ViewModel 类型形成紧耦合，自定义事件数从 11 个精简到 2 个
**通用模式:** TUI 应从标准 ACP session/update 事件直接派生渲染结构，不要经过中间共享类型层——每层翻译都是耦合点。Agent 层只产 ACP 标准事件，TUI 层自行解释渲染
**涉及文件:** peri-agent/src/types/, peri-tui/src/kit/
**CLAUDE.md 链接:** false

### issue_2026-07-08-peri-agent-architecture-improvement
**摘要:** Agent 架构升级——统一存储、中间件排序、StageContext 聚合
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** AgentState, MessageTranscript, 中间件链, StageContext
**问题本质:** AgentState/MessageTranscript 双轨存储导致 per-hook O(n²) clone；中间件缺少声明性优先级约束
**通用模式:** 统一 AgentContext 为唯一真相源消除双轨 clone。中间件需声明性优先级而非隐式插入顺序。StageContext 聚合为子结构减少参数传递
**涉及文件:** peri-agent/src/agent/, peri-acp/src/agent/builder.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-peri-agent-comprehensive-code-quality-review
**摘要:** Agent 代码质量全面审查——panic 防护、取消令牌、版本化
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 代码质量, panic 防护, CancellationToken, schema 版本化
**问题本质:** 多处 panic!/unwrap 在异常路径触发→TUI 崩溃；重试循环无取消令牌→死循环
**通用模式:** 事件映射层禁止 panic!，用 tracing::error + 降级值。所有重试循环必须检查 CancellationToken。事件类型和 SQLite schema 需要版本号避免迁移不兼容
**涉及文件:** peri-agent/src/, peri-acp/src/event/
**CLAUDE.md 链接:** false

### issue_2026-07-08-peri-agent-code-quality-improvement
**摘要:** Agent 代码质量提升——JoinHandle 监控、concurrent 超时、serde 失败不静默
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** JoinHandle, dispatch_concurrent, serde 失败, unwrap_or_else
**问题本质:** 后台 writer task 无 JoinHandle→panick 静默。dispatch_concurrent 无 per-invoke 超时→慢工具阻塞整批。serde 反序列化失败静默丢弃
**通用模式:** 所有 spawn 必须保存 JoinHandle 检测 panic。dispatch_concurrent 必须 per-invoke 超时。serde 失败必须 tracing::error 而非静默丢弃。unwrap_or_else 需确认覆盖的是正常降级路径而非掩盖不变量
**涉及文件:** peri-agent/src/, peri-middlewares/src/
**CLAUDE.md 链接:** false

### issue_2026-07-08-peri-agent-maintainability-improvement
**摘要:** Agent 可维护性提升——集成测试、版本化、feature flag、长函数拆分
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 集成测试, 版本化, feature flag, 长函数拆分
**问题本质:** 编排函数缺少集成测试→重构高风险。事件和 schema 无版本号→跨版本迁移断裂
**通用模式:** 核心编排函数必须有集成测试。事件 enum 和 SQLite schema 需要语义化版本号（如 `V1_Event` vs `V2_Event`）。用 feature flag 分离编译依赖。单函数超过 150 行需拆分为步骤级函数
**涉及文件:** peri-agent/src/, peri-acp/src/
**CLAUDE.md 链接:** false

### issue_2026-07-25-compact-consecutive-failures-reset-causes-infinite-loop
**摘要:** Compact 死机开关失效——`consecutive_failures` 提前清零导致无限 Full 重试
**状态:** Fixed
**归档日期:** 2026-07-30
**关键词:** compact, consecutive_failures, 死循环, 计数器管理
**问题本质:** Micro+Full 分支在 Full 调用前提前清零 consecutive_failures，导致振荡在 0↔1 永不触达 max=3
**通用模式:** 计数器管理只能在一个地方做——run_full_or_degrade 内部已正确管理（成功清零/失败+1），外部不应干预。修正的同时需连根删掉已失效的过期 excluded 标记清除死代码
**涉及文件:** peri-agent/src/agent/compact_v2/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-07-29-micro-compact-loses-agent-tool-context
**摘要:** Micro Compact 整体替换 tool input 导致 Agent 必填参数缺失
**状态:** Fixed
**归档日期:** 2026-07-30
**关键词:** micro compact, tool input, 字段级压缩, Agent 工具
**问题本质:** Micro Compact 将历史 tool input 整体替换为 `{"_compact_note":"tool input compacted"}`，删除了 `prompt` 等必填字段
**通用模式:** Micro Compact 只应收窄为回收明显偏长的 payload（chars > 500 的顶层字符串字段做 head/tail 截断），短内容和 JSON 结构原样保留。语义总结属于 Full Compact 职责。Planner 决定压缩对象，Projection 仅执行持久化计划
**涉及文件:** peri-agent/src/agent/compact_v2/（config, projection, planner）
**CLAUDE.md 链接:** false

### issue_2026-07-30-cancel-loses-agent-loop-context
**摘要:** 取消后下一轮 Agent loop 丢失全部前文
**状态:** Fixed
**归档日期:** 2026-07-30
**关键词:** cancel, transcript, ThreadStore, history replacement
**问题本质:** Cancel 触发的不完整 transcript 被写回 ThreadStore，后续 turn 读到的 history 为空
**通用模式:** Full Compact 事务提交成功后在 MessageTranscript 标记 history replacement；TUI 据此接受 compact 摘要快照。不完整取消结果不写回 ThreadStore
**涉及文件:** peri-agent/src/agent/, peri-acp/src/session/
**CLAUDE.md 链接:** false

### issue_2026-07-16-p1-1-stagecontext-split
**摘要:** StageContext 22 字段 god object 拆分为 SessionHandle + RuntimeServices + CompactContext + AsyncContext
**状态:** Fixed
**归档日期:** 2026-07-30
**关键词:** StageContext, god object, 职责边界, 子结构分组
**问题本质:** 22 字段全平铺在 StageContext，缺少职责边界，新字段加入时难以判断归属
**通用模式:** 按生命周期分组：会话级实体→运行时服务→Compact 系统上下文→异步传输控制。聚合根 4 个子结构，阶段函数隐式依赖变为显式访问
**涉及文件:** peri-agent/src/agent/stages/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-07-16-p1-4-compact-v2-split
**摘要:** compact_v2.rs ~900 行拆分为 micro.rs / full.rs / smart.rs + mod.rs 入口
**状态:** Fixed
**归档日期:** 2026-07-30
**关键词:** compact_v2, 文件拆分, 策略独立, 模块化
**问题本质:** 单一文件含 3 种 Compact 策略，共用部分仅 CompactResult
**通用模式:** 按策略独立拆分子模块，公开辅助函数用 pub use 重导出保持调用路径不变
**涉及文件:** peri-agent/src/agent/compact_v2/
**CLAUDE.md 链接:** false

### issue_2026-07-29-micro-compact-field-level-design
**摘要:** Micro Compact 字段级压缩设计——Planner/Projection 阶段分离、Unicode 安全截断
**状态:** Approved
**归档日期:** 2026-07-30
**关键词:** micro compact, 字段级压缩, 设计文档, Planner, Projection
**问题本质:** Micro Compact 从整体替换改为字段级 head/tail 截断，Planner 决定压缩对象，Projection 执行持久化
**通用模式:** 默认阈值 500 chars，保留头 350 + 尾 100。截断用 Unicode scalar value 计数。ToolResult 错误完整保留，成功结果超阈值才截断。工具保护名单不受影响
**涉及文件:** peri-agent/src/agent/compact_v2/
**CLAUDE.md 链接:** false

### issue_2026-07-16-architecture-upgrade-checklist
**摘要:** 架构升级总清单——三维护审视识别 35 个待升级点
**状态:** Fixed
**归档日期:** 2026-07-30
**关键词:** 架构审视, 跨 crate, god object, SubAgent 事件, 技术债
**问题本质:** 三维审视系统性盘点技术债，P0 项已拆分为独立 sub-issue 并修复
**通用模式:** 定期架构审视应成为 release 前的标准流程
**涉及文件:** 全 workspace
**CLAUDE.md 链接:** false
