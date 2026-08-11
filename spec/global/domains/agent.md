# Agent / ReAct 循环领域

## 领域综述

Peri Agent 的 ReAct 循环、工具系统、Context 管理、SubAgent 构建。核心数据流为消息构建 → ReAct 循环（Receive → Compact → Reason → Act）→ 工具分发 → 响应生成。LLM Provider 层独立为 peri-model crate（`Model` trait 统一 OpenAI/Anthropic，流式优先），由 `peri-agent/src/agent/model_bridge.rs`（`AgentModelBridge`）桥接进 ReAct 循环。

## 核心流程

- **ReAct 循环**：`run_react_loop`（peri-agent/src/agent/stages/mod.rs），每轮 Receive（循环入口与退出判断）→ Compact → Reason（before_model → LLM → after_model）→ Act（工具分发）；`max_iterations` 参数化（主 agent 500，SubAgent 默认 200），超限返回 MaxIterationsExceeded
- **消息构建**：`BaseMessage`（Human/Ai/System/Tool）与 `ContentBlock`（Text/Image/Document/ToolUse/ToolResult/Reasoning/Unknown）位于 peri-agent/src/messages/（message.rs / content.rs），跨 crate 共享（peri-acp 复用）
- **Provider 适配**：peri-model crate 提供 `Model` trait（流式优先——`stream()` 为唯一调用路径，`complete()` 聚合事件实现非流式），`AnthropicModel` / `OpenAiModel` 为实现；peri-agent 侧 `AgentModelBridge`（model_bridge.rs）实现 `ReactLLM` 供 Reason 阶段调用
- **SubAgent 构建**：fork/non-fork 双模式，共享 `build_v2_subagent_context` 入口（peri-middlewares/src/subagent/v2_bridge.rs），`parent_messages` 注入 transcript

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 循环引擎 | ReAct Loop（`run_react_loop`，peri-agent/src/agent/stages/mod.rs），`max_iterations` 参数化（主 agent 500 / SubAgent 默认 200） |
| 消息类型 | `BaseMessage` + `ContentBlock` 枚举（peri-agent/src/messages/，ACP 协议对齐） |
| LLM Provider | peri-model crate：`Model` trait（`stream()` 流式优先 + `complete()` 聚合非流式），`AnthropicModel` / `OpenAiModel`；`AgentModelBridge`（model_bridge.rs）接入 ReAct |
| SubAgent | fork 模式（隔离构建）+ 非 fork 模式（后台/同步），v2_bridge（peri-middlewares/src/subagent/v2_bridge.rs）统一入口 |
| 工具分发 | 批量审批 → 并发执行 → 聚合（错误延迟）→ 统一原子写入 transcript，`tool_dispatch.rs`（阶段 A/B/C） |

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
**涉及文件:** peri-model/src/openai_compatible/stream.rs（原 peri-agent/src/llm/openai/stream.rs，已迁至 peri-model）
**CLAUDE.md 链接:** false

### issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn
**摘要:** Agent 工具（background 模式）启动 bg agent 后 loading 不停止
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** background agent, loading 生命周期, TurnDone, ReAct 循环
**问题本质:** AI 调用 Agent 工具（background 模式）后本轮 turn 正常结束（TurnDone），但 bg callback 触发的 `SyntheticUserMessage` 在 agent End 阶段 emit，导致连续 loading 周期——首轮 TurnDone 和 bg callback 的 TurnStart 之间没有 loading=false 间隙。与双通道 flush-then-push 修复关联。
**通用模式:** ReAct 循环中 background agent 的 loading 生命周期需要显式的 TurnDone→TurnStart 过渡：background agent 启动后当前 turn 应正常 TurnDone（loading 停止），callback 唤醒时创建新的 TurnStart（loading 重新开始）。二者必须独立发送，不能合并为连续 loading。
**涉及文件:** peri-agent/src/agent/stages/mod.rs, peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）
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
**涉及文件:** peri-model/src/anthropic/request.rs（原 peri-agent/src/llm/anthropic/adapter.rs，请求序列化部分已迁至 peri-model）
**CLAUDE.md 链接:** false

### issue_2026-07-17-anthropic-adapter-system-cache-split-missing
**摘要:** Anthropic adapter 未对 request.system 调用 split_system_blocks，导致 main agent 系统提示词边界标记失效
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Anthropic adapter, system prompt cache, split_system_blocks, boundary marker
**问题本质:** `build_request_body()` 对 `request.system` 直接 push 单块，未调用已存在的 `split_system_blocks()` 拆分静态/动态内容
**通用模式:** 同一功能（缓存拆分）有两条代码路径时，主路径可能漏掉已实现的辅助函数；fork agent 走 `BaseMessage::System` 路径反而正确拆分
**架构影响:** `request.system` 和 `BaseMessage::System` 是两条独立序列化路径，未统一抽象
**涉及文件:** peri-model/src/anthropic/request.rs, peri-model/src/anthropic/cache.rs（原 peri-agent/src/llm/anthropic/adapter.rs, peri-agent/src/llm/anthropic/cache.rs，已迁至 peri-model）
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
**涉及文件:** peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）
**CLAUDE.md 链接:** true

### issue_2026-07-13-sync-agent-tool-cards-not-showing
**摘要:** 同步 Agent 子工具调用卡片完全不显示
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 同步 SubAgent, 子工具卡片, or_insert_with, event_tx 关闭
**问题本质:** `builder_v2.rs` 用 `or_insert_with` 保留第一 turn 的 SubAgentTool 实例，其 `event_tx` 在第二 turn 时已被 close——所有 SubagentStarted 事件被静默丢弃
**通用模式:** `or_insert_with` 不适合需要每 turn 重建的有状态对象（含 channel/sender）。每 turn 复用时应确保内部 channel 有效，或改用 `insert` 强制替换
**涉及文件:** peri-acp/src/agent/builder.rs（原 builder_v2.rs，已并入 builder.rs）, peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）
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
**涉及文件:** peri-acp/src/agent/builder.rs（原 builder_v2.rs，已并入）, peri-agent/src/session/transcript.rs
**CLAUDE.md 链接:** true

### issue_2026-07-18-subagent-tool-cards-regression-empty
**摘要:** SubAgent 工具调用卡片回归不显示——Agent 卡片容器可见但内部为空壳
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** SubAgent 卡片回归, 工具调用卡片, 竞态条件, AgentGroup
**问题本质:** `event_tx` 的替换（or_insert_with→insert）解决了多轮复用问题，但引入了新的竞态——SubAgent 在 tool group 注册到 accumulator 之前就发送了 ToolStart 事件，卡片创建时目标 group 不存在
**通用模式:** channel 替换修复可能引入时序依赖——子线程事件可能在注册完成前到达。需要在 accumulator 中缓存早到事件，待 group 就绪后重放
**涉及文件:** peri-acp/src/agent/builder.rs（原 builder_v2.rs，已并入）, peri-tui/src/kit/acp_types.rs
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
**涉及文件:** peri-agent/src/messages/（原 peri-agent/src/types/，已迁移）, peri-tui/src/kit/
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

### issue_2026-07-25-micro-compact-silently-fails-within-turn
**摘要:** Micro Compact 在同 turn 内静默失效——dry-run 与投影双重职责冲突
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** Micro Compact, dry-run 与投影冲突, truncated 标记, 显式参数消歧
**问题本质:** `plan_micro()` 在 Compact（dry-run）与 Reason（投影）两个阶段职责对立，已 truncated 消息被跳过导致同 turn Reason 阶段 LLM 看到完整原文。
**通用模式:** 同一函数被两个语义对立场景复用 → 用显式参数消歧（方案 A），而非缓存/旁路。
**涉及文件:** peri-agent/src/agent/compact_v2/{planner.rs,mod.rs,micro.rs,smart.rs}, peri-agent/src/agent/stages/reason.rs, compact_v2/planner_test.rs
**CLAUDE.md 链接:** false

### issue_2026-07-29-image-input-support
**摘要:** TUI→ACP→Agent 管线图片输入通道打通（@image 语法）
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 图片输入, @image, ImageMiddleware, ContentBlock::Image, messages_mut
**问题本质:** 数据模型与 provider 已就绪，TUI→ACP→Agent 管线图片通道未打通；ImageMiddleware 在 before_agent 将路径转 ContentBlock::Image；修复 #1 解决 messages_mut() 未同步 transcript 的桥接问题。
**通用模式:** 能力就绪但链路缺一环时，先在数据流图上定位断点，而非从端到端盲目调试。
**涉及文件:** peri-tui/src/kit/input_paste.rs, peri-middlewares/src/middleware/image.rs, peri-acp/src/session/builder.rs, peri-agent/src/session/transcript.rs, peri-agent/src/agent/agent_context.rs, peri-agent/src/agent/stages/middleware_runner.rs
**CLAUDE.md 链接:** false

### issue_2026-08-01-micro-compact-invisible-no-trigger
**摘要:** Micro Compact 无触发痕迹——field-level 重写后空 plan 跳过
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** field-level compact, 空 plan, 兜底压缩, 回归
**问题本质:** field-level micro compact 重写后普通短参数工具调用生成空 plan → 跳过 → 无触发痕迹（两现象同根因）。修复：无超长字段时对 object 根生成整条压缩兜底。
**通用模式:** 策略收紧到边界后"正常输入无输出"是回归（无触发迹象+无记录）的共性根因。
**涉及文件:** peri-agent/src/agent/compact_v2/{planner.rs,projection.rs}
**CLAUDE.md 链接:** false

### issue_2026-08-02-agent-asks-user-too-late-in-ambiguous-env
**摘要:** agent 在环境失败/症状不明时静态深挖不收敛，提问过晚
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** AskUserQuestion, 静态深挖, speculation guard, prompt 硬规则
**问题本质:** agent 把"环境失败/症状不明"当代码缺陷，静态分析不收敛不升级提问；修复 = prompt 硬规则（Ask Before Diving）+ 代码层 speculation_guard 哨兵。
**通用模式:** 运行时问题只能问用户；推测性语言连续出现且无新证据时止损；去数字阈值、去词表防 Goodhart。
**架构影响:** LoopState 扩展、MessageSource 新变体、SubAgent 用 session_id 键区分。
**涉及文件:** peri-acp/prompts/sections/{03_doing_tasks,05_using_tools}.md, prompt_test.rs, peri-agent/src/agent/stages/speculation_guard.rs, stages/mod.rs, session/queue.rs
**CLAUDE.md 链接:** false

### issue_2026-08-02-background-task-15s-timeout-kills-and-misreports
**摘要:** 后台任务受 15s 默认超时约束——只杀 wrapper 致孤儿进程存活 + 通知误报
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 进程组 kill, 超时语义, 孤儿进程, promote 续跑
**问题本质:** 后台任务受 15s 默认超时约束，超时只杀 wrapper 不杀进程组 → 孤儿存活 + 通知误报；修复 = bg 默认不超时 + 进程组 kill + 同步超时转后台续跑 + timed_out 结构化标记。
**通用模式:** 后台/超时语义必须进程组粒度；"杀 wrapper"是隐形孤儿来源。
**涉及文件:** peri-middlewares/src/process/mod.rs, subagent/background.rs, middleware/terminal.rs, events.rs, execute_bg.rs, spawner.rs, workflow/mod.rs, executor.rs, descriptions/bash.md
**CLAUDE.md 链接:** false

### issue_2026-08-02-grep-glob-path-parameter-ignored
**摘要:** Grep/Glob 的 path 参数被 normalize_params 静默重命名丢弃
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 参数归一化, schema 感知, 静默回退, 契约测试
**问题本质:** `normalize_params` 无条件把 path 重命名 file_path，Grep/Glob schema 无 file_path → 参数丢失静默回退全仓库。
**通用模式:** 参数别名归一化必须 schema 感知；静默回退是坏设计——契约测试锁定范围。
**涉及文件:** peri-agent/src/tools/invocation.rs, agent/stages/tool_dispatch.rs, peri-middlewares/src/tool_search/execute_tool.rs, tools/filesystem/{grep.rs,glob.rs}, tests/canonical_tool_invocation_contract.rs
**CLAUDE.md 链接:** false

### issue_2026-08-02-langfuse-bridge-drops-provider-request-id
**摘要:** Langfuse bridge 事件转换用 `..` 丢弃 provider request_id
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** request_id 透传, `..` 丢弃, 事件转换, generation metadata
**问题本质:** bridge 两处事件转换用 `..` 丢弃 request_id，`UnifiedLangfuseEvent` 与 `on_llm_end` 均不含 → 遥测无法关联 provider 日志。
**通用模式:** 事件转换禁止用 `..` 静默丢弃字段——enum 增字段成本低（无 serde derive）。
**涉及文件:** peri-acp/src/langfuse/bridge.rs, langfuse/tracer/mod.rs, tracer_test.rs, tests/langfuse_e2e.rs
**CLAUDE.md 链接:** false

### issue_2026-08-02-reason-rs-loses-request-id-without-usage
**摘要:** usage=None 时 request_id 被 unwrap_or 替换一并丢弃
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** usage 缺失, Option 解包, request_id 独立读取
**问题本质:** request_id 在 usage 闭包内派生，usage=None 时连 request_id 一起被 unwrap_or 替换丢弃；两字段来源独立可不同时存在。
**通用模式:** 独立来源的字段禁止打包进同一 Option 解包；`reasoning.request_id.clone()` 独立读取。
**涉及文件:** peri-agent/src/agent/stages/reason.rs
**CLAUDE.md 链接:** false

### issue_2026-08-04-cron-trigger-lost-after-turn-error
**摘要:** Cron 触发在 turn 结束后静默丢失——bridge 绑定在 per-turn V2Session 上
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** session 级 vs turn 级, V2Session, bridge 生命周期, idle 消费
**问题本质:** CronOwner bridge 绑定在每次 turn 新建的 V2Session 上，turn 结束（含 retry 失败）管道即死，sender 被 retain 清理 → 触发静默丢失；设计文档要求 session 级。
**通用模式:** 设计文档写明的生命周期语义与实现偏差 = 检查点；用已正确的 lazy-init 范例（session_inbox_for）对齐。
**架构影响:** SessionCronBridge 提升 session 级（`SessionManager::cron_bridge_for`）；idle 开新 turn 留作增强。
**涉及文件:** peri-acp/src/agent/builder.rs, peri-agent/src/agent/session/{cron_owner.rs,inbox.rs}, peri-middlewares/src/cron/mod.rs, peri-acp/src/session/mod.rs, executor_helpers.rs, peri-tui/src/app/cron_state.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-l1-bg-tasks-to-agent
**摘要:** 3.0 L1——后台任务从中间件迁入 Agent 层 async_tasks 模块
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 3.0 重构, 后台任务归位, async_tasks, 行为契约
**问题本质:** 3.0 分层 PRD 拆分子项——后台任务能力从 peri-middlewares 归位到 peri-agent 新职责模块。
**通用模式:** 大重构以"行为契约测试保持绿 + 单事实源同步迁移（ARC-MIDDLEWARE-001 禁双事实源）+ CI 依赖门"推进。
**架构影响:** 新建 peri-agent/src/agent/async_tasks.rs；ARC-* 契约 Verify 更新。
**涉及文件:** peri-middlewares/src/subagent/background.rs（迁出）, peri-agent/src/agent/async_tasks.rs（新）, session/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-l2-middleware-assembly-to-agent
**摘要:** 3.0 L2——中间件装配从 ACP builder 迁入 Agent SessionFactory
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 3.0 重构, 装配迁移, SessionFactory, ARC 契约
**问题本质:** 中间件装配逻辑从 peri-acp builder 归位到 peri-agent 统一创建入口 SessionFactory。
**通用模式:** 大重构以"行为契约测试保持绿 + 单事实源同步迁移 + CI 依赖门"推进。
**架构影响:** 新建 peri-agent/src/session/factory.rs；docs/standards/architecture-contracts.md 契约更新。
**涉及文件:** peri-acp/src/agent/builder.rs（迁出）, peri-agent/src/session/factory.rs（新）, docs/standards/architecture-contracts.md
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-l3-subagent-factory-to-agent
**摘要:** 3.0 L3——subagent 创建统一迁入 Agent 层
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 3.0 重构, subagent 创建, 统一入口
**问题本质:** subagent 的 spawner/fork/background/built_in_agents 创建入口从中间件归位到 peri-agent/src/session/subagent.rs。
**通用模式:** 大重构以"行为契约测试保持绿 + 单事实源同步迁移 + CI 依赖门"推进。
**涉及文件:** peri-middlewares/src/subagent/（spawner/fork/background/built_in_agents）, peri-agent/src/session/subagent.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-l4-langfuse-bypass-consumer
**摘要:** 3.0 L4——Langfuse 3700 行内嵌重构为 Controller 侧旁路消费者
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 旁路消费者, 采集点覆盖, Controller 宿主, 观测解耦
**问题本质:** Langfuse 3700 行内嵌 peri-acp 参与业务链路；重构为 Controller 侧旁路消费者，协议化前分支投递。
**通用模式:** 观测=旁路消费者，业务链路零观测逻辑；身份关联靠 (session_id,turn_id,agent_id)。
**架构影响:** peri-controller 新建 langfuse/ 模块；`peri-acp/src/langfuse/` 移除。
**涉及文件:** peri-acp/src/langfuse/（迁出）, peri-controller（宿主）, 各层采集点
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-l5-executor-split
**摘要:** 3.0 L5——executor 薄壳化拆分到 session/exec/
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 3.0 重构, executor 拆分, 薄壳化
**问题本质:** peri-acp session executor 薄壳化，执行职责拆分到 peri-agent/src/session/exec/。
**通用模式:** 大重构以"行为契约测试保持绿 + 单事实源同步迁移 + CI 依赖门"推进。
**架构影响:** peri-acp-types identity.rs 同步更新。
**涉及文件:** peri-acp/src/session/{executor.rs,executor_helpers.rs}（薄壳化）, peri-agent/src/session/exec/, peri-acp-types identity.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-m-event-chain-canonical
**摘要:** 3.0 M——Agent 事件三通道收敛为单链路，v2_tx 双轨退役
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** v2_tx 双轨下线, 事件三层化, 身份契约, 单事实源
**问题本质:** Agent 事件三通道 + v2_tx 双轨直连 TUI 绕过 caps 门控，身份漂移；收敛为单链路并补打 session_id/session_seq，v2 mapper 桥接退役。
**通用模式:** 双轨投递 → 单事实源；`Seq` 不实现 Default 防伪装。
**架构影响:** UnstampedEvent/EventEnvelope/SessionSeq 落位 peri-acp-types；ARC-EVENT-001 更新。
**涉及文件:** peri-agent/src/agent/events_v2.rs（退役）, peri-tui/src/kit/v2_bridge.rs（删除）, peri-acp/src/event/
**CLAUDE.md 链接:** false

### issue_2026-08-05-3.0-m-resources-layer
**摘要:** 3.0 M——新建 peri-resources 存储通道层
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 3.0 重构, Resources 层, 存储通道
**问题本质:** sqlite 存储通道从 peri-agent 迁出，新建 peri-resources crate 承载 config/sessions。
**通用模式:** 大重构以"行为契约测试保持绿 + 单事实源同步迁移"推进。
**架构影响:** 新建 peri-resources/{config,sessions}；调用方（peri-tui、acp_stdio）同步改引用。
**涉及文件:** peri-agent/src/thread/sqlite_store.rs（迁出）, peri-resources/{config,sessions}, peri-tui/src/app/mod.rs, acp_stdio/init.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-bg-cancel-abort-skips-cleanup
**摘要:** Agent 类 bg 取消仅 abort 跳过收尾——active_agents 泄漏 + 子进程孤儿化
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** cancel_token, 超时 abort 兜底, BgCleanupGuard, 收尾配对
**问题本质:** Agent 类 bg 取消仅 abort 跳过全部收尾 → active_agents 泄漏 + 子进程孤儿化；修复 = token.cancel()（走完整收尾）→ 3s 超时 abort 兜底 + 同步收尾 guard。
**通用模式:** 取消语义三层（工具层取消链 → 优雅等待 → 强制 abort），async 收尾不能放 Drop。
**涉及文件:** peri-middlewares/src/subagent/background.rs, subagent/tool/execute_bg.rs, peri-agent/src/agent/async_tasks.rs（测试）
**CLAUDE.md 链接:** false

### issue_2026-08-05-bg-command-expect-panic-via-rpc
**摘要:** BgCommand 两处 expect 可经公开 RPC 传 None 触发 panic
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** expect panic, 公开 RPC, Option 参数, 优雅降级
**问题本质:** `BgCommand` 两处 expect 可经公开 RPC `session/execute-command` 传 None 触发 panic，async 上下文崩 server task。
**通用模式:** 公开协议入口的参数校验先于内部不变量假设；`expect` 换成 `let-else` 优雅降级 + 明确错误文本。
**涉及文件:** peri-acp/src/session/command/bg.rs, dispatch/execute_command.rs, dispatch/rewind.rs, command/bg_test.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-bg-task-over-limit-still-runs
**摘要:** 超限后台任务仍实际运行——检查-注册竞态
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 检查-注册竞态, oneshot 门控, 零事件零注册, 幽灵任务
**问题本质:** 上限预检与 register_with_kind 间多个 await 非原子，超限任务 spawn 已启动仍实际运行；修复 = spawn 包装任务 + 注册结果 oneshot 门控，失败零事件零注册。
**通用模式:** 异步"检查-执行"竞态用"先注册/门控后执行"翻转；事件发射与注册顺序对齐。
**涉及文件:** peri-middlewares/src/subagent/tool/execute_bg.rs, subagent/spawner.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-bg-shell-task-id-collision
**摘要:** bg shell task_id 截断 UUID v7 前缀——同毫秒碰撞静默吞 Completed
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** UUID v7 截断, 毫秒碰撞, HashMap 覆盖, 静默跳过
**问题本质:** bg shell task_id 取 UUID v7 前 8 字符（=时间戳高 32 位），同毫秒必然碰撞 → 注册覆盖 + 幽灵完成防护静默吞 Completed。
**通用模式:** ID 生成禁止截断时间前缀类 UUID；静默跳过分支应 warn 化（本次 bug 正被静默掩盖）。
**涉及文件:** peri-middlewares/src/middleware/terminal.rs, subagent/background.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-cancel-bg-task-workflow-kind-ineffective
**摘要:** Workflow 注册固定 Kill(None)——cancel 只删条目 runner 继续跑
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** Kill(None) 死代码, kill 闭包打通, progress_store Killed, 协议契约
**问题本质:** Workflow 注册固定 `Kill(None)`，真 kill_tx 未打通 → cancel 只删条目 runner 继续跑；对抗验证改判 P1 潜在缺陷（TUI 未接线前不可复现）。
**通用模式:** 对抗 review 校准可达性（TUI 未接线 → 降级）；"报成功却无效"是协议契约错误。
**涉及文件:** peri-middlewares/src/subagent/background.rs, workflow/mod.rs, peri-workflow/src/{tool.rs,registry.rs,runner.rs}, peri-tui acp_server/requests.rs, kit/panels/{tasks.rs,workflow.rs}
**CLAUDE.md 链接:** false

### issue_2026-08-06-cli-config-db-path
**摘要:** CLI 新增 --db-path/--config-file 全局参数——配置重定向与 fallback 语义
**状态:** Done
**归档日期:** 2026-08-11
**关键词:** --db-path, --config-file, 配置重定向, fallback 语义
**问题本质:** sqlite 路径与全局配置路径硬编码；新增 CLI 全局参数 + 进程级 config_path 重定向 + `Resources::open_with`；显式路径失败直接报错不再 fallback 临时目录。
**通用模式:** 配置保存必须写回重定向路径（否则静默丢数据）；env 注入在 parse 前的时序陷阱；相对路径 absolutize。
**涉及文件:** peri-tui/src/main.rs, launch.rs, app/mod.rs, cli_print.rs, peri-acp/src/provider/store.rs, host/stdio/init.rs, peri-agent/src/resources.rs, peri-resources/src/context.rs
**CLAUDE.md 链接:** false

### issue_2026-08-06-e2e-bg-task-area-entry-missing
**摘要:** L5 归位回归——工具注入早于 set_parent_session 导致 bg 静默降级同步
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** stage_builder 时序, parent_session, 静默降级, 时序契约
**问题本质:** L5 归位回归——stage_builder 工具注入发生在 set_parent_session 之前，SubAgentTool 读 parent_session 恒 None → run_in_background 静默降级同步。修复 = 工具注入移到 parent_session 之后（时序契约注释）。
**通用模式:** 重构后"静默降级"回归用日志佐证（SubagentStarted is_background=false）定位。
**涉及文件:** peri-agent/src/session/exec/stage_builder.rs, peri-middlewares/src/subagent/mod_test.rs
**CLAUDE.md 链接:** false

### issue_2026-08-06-e2e-glob-grep-match-suffix-missing
**摘要:** E2E Judge 误判 Glob 卡缺匹配数后缀——卡片被挤出视口
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** E2E 判读, 可见视口, 截图时序, flaky 信号
**问题本质:** 非产品缺陷——Glob 卡被超长回复挤出视口，Judge 误判"缺匹配数后缀"；修复 = 截图前滚动到顶部 + 头行后缀单测。
**通用模式:** Judge 基于可见屏幕判读 → 截图前先稳定可见区域；"3 次运行 2 过 1 挂"是 flaky 信号。
**涉及文件:** e2e/tests/tool-cards/header-suffix-and-error.test.ts, peri-tui/src/kit/message_area/render_test.rs
**CLAUDE.md 链接:** false

### issue_2026-08-06-e2e-workflow-not-completing
**摘要:** downcast_arc 对 trait object 调 type_id() 恒失败——完成通知无订阅者
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** downcast 陷阱, type_id, Any blanket impl, as_any
**问题本质:** `downcast_arc` 对 trait object 调 `(*ptr).type_id()` 命中 Any blanket impl 返回 trait object 自身 → downcast 恒失败 → 临时实例 registry 分离 → 完成通知无订阅者。
**通用模式:** trait 不继承 Any 时 type_id() 陷阱；`as_any().type_id()` 才是具体类型。
**涉及文件:** peri-acp-types/src/ports.rs, peri-middlewares/src/workflow/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-08-07-cron-tool-task-never-triggers
**摘要:** Cron 工具注册到临时 scheduler——downcast 恒失败静默失效
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** CronSchedulerPort downcast, type_id, 临时实例, 静默失效
**问题本质:** 与 workflow 同构——downcast 恒失败，cron 工具注册到临时 scheduler C，tick 跑在 B 上，触发完全静默。
**通用模式:** 同根因同构（McpPoolPort/ToolSearchPort 仍残留 type_id 写法，正文点名）。
**涉及文件:** peri-acp-types/src/cron.rs, peri-middlewares/src/cron/mod_test.rs, assembly.rs
**CLAUDE.md 链接:** false

### issue_2026-08-07-langfuse-v3-subagent-parent-chain-validation-memo
**摘要:** v3 重构后线上 trace subagent 父链验证备忘——先立部署切点再采样
**状态:** Closed
**归档日期:** 2026-08-11
**关键词:** 父链校验, 线上审计, trace 结构, subagent 生命周期
**问题本质:** v3 重构后线上 trace 的 subagent 父链疑似断裂（5/7 祖先链缺失），需以部署切点重采样判定是否为回归；复验确认父链完整，但新发现并行 subagent AGENT obs 关闭时序瑕疵（MissingStop/DuplicateStop）。
**通用模式:** 线上观测问题的"版本字段不足以辨别构建"陷阱——先立部署切点再采样，避免旧数据误判新构建。
**架构影响:** `peri-controller/src/langfuse/tracer/registry.rs` stop/close 两信号路径时序状态机存疑（待独立 issue）。
**涉及文件:** peri-controller/src/langfuse/tracer/registry.rs；关联 langfuse-batcher / subagent-attribution 两 issue
**CLAUDE.md 链接:** false

### issue_2026-08-09-subagent-resume-mechanism
**摘要:** subagent 中断后无法找回现场——新增 resume_thread_id 恢复机制
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** resume_thread_id, transcript 重建, thread 所有权, 容错模式
**问题本质:** subagent 中断后无法找回现场（返回值不带 child_thread_id）；新增 `SessionFactory::resume_subagent` + 工具参数，从 thread_store 重建现场续跑；错误文案二次修复，最终互斥报错改容错模式（resume 存在时静默忽略 subagent_type/fork）。
**通用模式:** 恢复=新执行单元（事件配对、thread_id 不变）；LLM 按 schema 惯性传参 → 容错优于报错+文案。
**涉及文件:** peri-agent/src/session/subagent.rs, peri-middlewares/src/subagent/tool/define.rs, execute_bg.rs, peri-acp-types/src/event.rs, descriptions/agent.md, subagent_test.rs
**CLAUDE.md 链接:** false

### issue_2026-08-10-bash-timeout-misdiagnosis-promotes-stalled-processes
**摘要:** Bash spawn 未设 stdin——读 stdin 进程挂死被 promote 成永不结束后台任务
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** stdin 继承, stall 分流, promote 误判, fuzzy 阈值
**问题本质:** Bash spawn 未设 stdin → 读 stdin 的进程永不 EOF 挂死到超时，被无条件 promote 成永不结束的后台任务；修复 = stdin 置 null（读 stdin 进程立即 EOF 快速失败）+ 超时按有无输出分流诊断；附带修复 Did-you-mean 无关候选（fuzzy 阈值 + 首字符约束 + 每目录配额）。
**通用模式:** 非交互 spawn 一律 `stdin(Stdio::null())`；错误文案反映不确定性，不武断承诺。
**涉及文件:** peri-middlewares/src/middleware/{terminal.rs,terminal_test.rs,descriptions/bash.md}, peri-agent/src/agent/async_tasks.rs, peri-agent/src/error_suggest/matcher.rs, bash_command_suggester.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-caps-negotiated-once-broken-second-session
**摘要:** caps 协商值只消费一次——stdio 第 2+ 个 session 事件门控错乱
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** caps 协商, take 一次性消费, unwrap_or_default, 静默错误配置
**问题本质:** `pending_caps` 被 `take()` 一次性取走，第 2+ session 取 None 走 unwrap_or_default（全 false）或 all_enabled（全 true）——同一客户端不同 session 门控行为不同。
**通用模式:** 一次性消费的协商值只服务第一个消费者；"取到 None 走默认"= 静默错误配置，应克隆保留或显式报错。
**涉及文件:** peri-acp/src/session/caps.rs（consume_pending_caps/ensure_session_caps），commits 1ff4a0ff/6e924c8b
**CLAUDE.md 链接:** false

### issue_2026-07-25-compact-decay-full-fails-micro-skipped-no-fallback
**摘要:** Compact 退化链——Micro 被跳过、Full 失败、无 fallback、计数器跨域污染
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 退化链, 多根因交织, fallback 缺失, 计数器隔离
**问题本质:** 5 条根因（判定反转/预算窗口/回收目标/计数器污染/无兜底）交织成退化链：Micro 有效判定指标反转→跳过 Micro 直走 Full→Full 失败无 fallback。
**通用模式:** 多根因交织缺陷用对抗验证收敛（6 agent 五维度）；退化链必须保留末级 fallback，计数器按作用域隔离。
**涉及文件:** peri-agent/src/agent/compact_v2/{micro.rs,full.rs,config.rs,mod.rs}
**CLAUDE.md 链接:** false

### issue_2026-07-25-has-changes-gate-blocks-compact-projection
**摘要:** has_changes() 决策门控阻断 Compact 投影——三条根因汇合
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 决策门控, 投影失效, 估算膨胀, saved=0
**问题本质:** estimate_tokens().max(50) 膨胀投影字符数 → estimated_tokens_saved=0 对短消息恒成立 → has_changes()=false 且 reclaim_target=0，truncated 标记对 LLM 实际可见内容完全无效。
**通用模式:** 决策门控的判定与投影执行脱节时，用"LLM 实际可见内容"做验收基准而非内部指标。
**涉及文件:** peri-agent/src/agent/compact_v2/{planner.rs,projection.rs}
**CLAUDE.md 链接:** false

### issue_2026-07-25-micro-compact-treadmill-reclaim-target-zero
**摘要:** Micro Compact 反复触发但压不住预算（跑步机效应 + reclaim_target=0）
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 跑步机效应, reclaim_target, 指标恒 0, 假成功
**问题本质:** reclaim_target 在 75%-93.5% 区间恒为 0，Micro 每次都"成功"（affected_count>0）但预算不降，Full 永不升级。
**通用模式:** "执行成功但指标不动"= 回收目标计算失效；恒为 0 的中间量是假成功信号，需断言回收目标 > 0。
**涉及文件:** peri-agent/src/agent/compact_v2/micro.rs
**CLAUDE.md 链接:** false

### issue_2026-07-27-rcra-simplify-agent-loop
**摘要:** Agent Loop 五阶段 CRRAE 简化为四阶段 RCRA
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 循环阶段合并, 预消费冲突, 退出判断
**问题本质:** Receive/End 职责重叠合并为 RCRA；修复 #1 处理了 loop 外预 drain 队列与 Receive 退出判断（consumed_count==0）的冲突——预消费使循环立即退出。
**通用模式:** 循环重构时检查 loop 外部是否还有消费方预取同一队列；退出判断与消费点必须同域。
**涉及文件:** peri-agent/src/agent/stages/mod.rs, peri-acp/src/session/executor_helpers.rs, workflow_agent.rs
**CLAUDE.md 链接:** false

### issue_2026-07-29-micro-compact-no-system-note
**摘要:** Micro Compact 执行后 TUI 不显示 SystemNote
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** Debug 格式 vs 字面值, 事件链完整, 文案错配
**问题本质:** 事件链（run_compact → MessagesCompacted → CompactCompleted → inject_system_note）完整无断裂，问题在 Debug 格式输出与预期文字面值错配。
**通用模式:** "链路完整但 UI 无表现"时检查格式错配（Debug 序列化 vs 字面文案），而非先怀疑事件断裂。
**涉及文件:** peri-agent/src/agent/compact_v2/、peri-tui/src/kit/acp_events/compact.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-cancel-misreported-as-llm-failure
**摘要:** 用户取消被误报为 LLM 失败——reason.rs match 两分支完全相同
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** match 分支相同, 复制粘贴缺陷, Interrupted 被吞
**问题本质:** `match &e { LlmHttpError|LlmError => LlmFailure, _ => LlmFailure }` 两个分支返回值相同，Interrupted 被吞成 LlmFailure。
**通用模式:** match 中两个分支返回相同值是复制粘贴缺陷哨兵；错误分类要有穷尽性测试。
**涉及文件:** peri-agent/src/agent/stages/reason.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-stage-ended-missing-on-error-path
**摘要:** run_stage Err 路径不 emit StageEnded——Langfuse 悬挂 span 不对称
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** Start/End 成对, Err 路径不对称, 悬挂 span
**问题本质:** StageStarted 无条件 emit、StageEnded 只在 Ok 分支 emit，LLM 失败/cancel/工具错误路径全部留下只有 Start 没有 End 的 span。
**通用模式:** 成对生命周期事件必须在所有退出路径对称 emit；compact.rs 的 Skip 不 emit Start 是正确范例。
**涉及文件:** peri-agent/src/agent/stages/mod.rs, compact.rs
**CLAUDE.md 链接:** false

### issue_2026-08-05-transcript-drop-loses-final-messages
**摘要:** agent 正常结束时 transcript 最后一批消息丢失落库（Drop abort writer）
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 批量写窗口, Drop abort, flush 缺失, 持久化不一致
**问题本质:** transcript writer 用 ≤100ms 窗口批量落库，loop 退出前从不 flush_persistence，drop 时 abort() 丢弃 pending_appends——最终回答几乎总在窗口内未落库。
**通用模式:** 批处理 writer 的 abort 会丢积压；所有正常退出路径必须显式 flush，不能依赖 Drop。
**涉及文件:** peri-agent/src/agent/session/transcript.rs（flush_persistence）
**CLAUDE.md 链接:** false

### issue_2026-08-05-after-agent-failure-missing-turn-completed
**摘要:** run_after_agent 失败：最终回答已写 transcript 但无 TurnCompleted
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** Err 路径不提交迭代边界, transcript 与 UI 不一致
**问题本质:** act.rs 先 append(ai_msg) 后 run_after_agent，后者 Err 时 loop 直接结束，TurnCompleted 永不执行——TUI 与持久化/恢复视图不一致。
**通用模式:** 失败路径也要提交迭代边界（从 transcript 读快照 emit TurnCompleted 再传播错误）；"Err 路径不提交边界"是系统性缺口清单项。
**涉及文件:** peri-agent/src/agent/stages/act.rs
**CLAUDE.md 链接:** false

### issue_2026-07-25-event-identity-diverges-across-dual-delivery-paths
**摘要:** 同一 Agent 事件经双轨投递后身份与语义可能不一致
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 双轨投递, 身份字段, mapper 临时补齐, 单事实源
**问题本质:** v2 事件跨 Render/State/Observe 与 ACP transport 双路径时 message_id/source_agent_id 被替换为默认值或 None，两条路径各自维护 mapper 与 suppress 规则。
**通用模式:** 双轨投递 → 收敛单链路；身份字段禁止由各 mapper 临时补齐，用类型契约（EventEnvelope/Seq）固化。
**涉及文件:** peri-acp-types/identity.rs（UnstampedEvent/EventEnvelope），v2_bridge.rs 退役
**CLAUDE.md 链接:** false

### issue_2026-07-25-stale-v2-events-bypass-session-filter
**摘要:** 旧会话的 v2 直连事件可能写入当前 TUI
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 空 session_id, 过滤守卫缺口, 双轨退役
**问题本质:** v2 直连路径 active_session_id 置空字符串，过滤逻辑只检查非空——旧 turn 事件绕过 stale-session 过滤写入当前 BridgeState。
**通用模式:** 过滤守卫"空值跳过检查"= 缺口；事件身份必须不可为空（类型层面保证）。
**涉及文件:** peri-tui/src/kit/v2_bridge.rs（删除），acp_bridge.rs
**CLAUDE.md 链接:** false

### issue_2026-07-31-extract-peri-model-protocol-crate
**摘要:** 抽取 peri-model 标准模型协议 crate
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** provider 协议分层, 独立 crate, goose-providers 借鉴
**问题本质:** 模型协议/厂商格式/HTTP-SSE/重试职责从 peri-agent 抽取为独立 peri-model crate，向上提供与 Agent 无关的标准协议。
**通用模式:** 协议核心独立于运行时（Goose goose-providers 分层）；上层组合与应用配置留在调用 crate。
**涉及文件:** peri-model/{protocol,runtime,transport,openai_compatible,anthropic}
**CLAUDE.md 链接:** false

### issue_2026-08-02-prompt-security-runtime-contracts
**摘要:** Prompt 安全边界与运行时契约收敛（PRD）
**状态:** Closed
**归档日期:** 2026-08-11
**关键词:** prompt 分层, 单一事实源, prompt_mode full, 能力声明漂移
**问题本质:** system prompt 将安全/工程/能力/状态/persona 拼成可整体替换文本，绝对性断言与运行时机制漂移；方案 = 五层模型（安全层不可移除）+ 能力声明与注册同一事实源 + tag 可信度边界。
**通用模式:** prompt 是运行时机制的平行副本必然漂移；关键断言（审批/工具可见性/模式）由同一运行时数据或 feature gate 生成。
**涉及文件:** peri-acp/prompts/sections/, prompt_test.rs, docs/design/prompt-sections-audit.md
**CLAUDE.md 链接:** false

### issue_2026-08-05-langfuse-subagent-attribution-stack-lifetime
**摘要:** Langfuse 上报中 subagent 内容整体错挂到主 agent——SubagentStack 生命周期错配
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** LIFO 栈归属, 消费顺序, 身份注册表, 空壳 observation
**问题本质:** 归属依赖无身份的 SubagentStack 栈顶近似，栈顶 has_started 标志被无关事件污染、双 forwarder 消费顺序无保证 → subagent 内容错挂主 agent；重构为身份注册表按 agent_id 路由。
**通用模式:** 归属/配对不能依赖消费顺序近似（栈/LIFO），用事件自带身份做注册表路由。
**涉及文件:** peri-controller/src/langfuse/tracer/registry.rs（commit 8cadefbe）
**CLAUDE.md 链接:** false

### issue_2026-08-05-langfuse-batcher-drops-during-slow-flush
**摘要:** Langfuse batcher 命令通道容量 = max_events + DropNew——慢 flush 期间静默丢事件
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** DropNew 静默丢, 通道容量耦合, 阻塞 select
**问题本质:** 命令通道容量与 buffer 上限共用 max_events（默认 50），do_flush 阻塞在 select 内时 Add 命令被 DropNew 丢弃——子 span 静默丢失出现悬挂孤儿 span。
**通用模式:** 控制通道容量与业务上限共用一个配置= 耦合缺陷；静默丢事件（DropNew）必须有观测计数。
**涉及文件:** langfuse-client/src/batcher.rs, peri-acp（后迁 peri-controller）langfuse/session.rs
**CLAUDE.md 链接:** false

### issue_2026-08-03-langfuse-trace-step-order-shuffled-with-parallel-subagents
**摘要:** Langfuse trace 中并行 subagent 的 step 顺序错乱
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 并行 step 顺序, 时间线错乱, 序号契约
**问题本质:** 并行 subagent 下 step 编号重复、与时间顺序不一致、step-28 被渲染为最后——trace 结构依赖并发完成序。
**通用模式:** 观测图的 step/span 顺序不能依赖并发完成序；需要显式序号/时间契约。
**涉及文件:** langfuse tracer 相关（trace 渲染侧 + 事件发射侧）
**CLAUDE.md 链接:** false

### issue_2026-08-05-background-task-completed-event-dead-path
**摘要:** BackgroundTaskCompleted 事件在 EventSink 无映射——注释声称的 Path A 是死路径
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 死路径, 注释声称, 冗余掩盖, 消费方验证
**问题本质:** event_sink.rs match 落入 `_ => None`，两个生产 send 点白推；TUI 实际由 registry 通道通知，死路径被冗余掩盖。
**通用模式:** "注释声称的路径"必须 grep 验证消费方；静默 `_ => None` 分支是死代码温床。
**涉及文件:** peri-acp/src/session/event_sink.rs, executor.rs
**CLAUDE.md 链接:** false

### issue_2026-07-22-p1-3-unstructured-error-cleanup
**摘要:** 非结构化错误清理——anyhow 自动转换绕过结构化变体
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** #[from] anyhow, 结构化错误, 裸字符串 Result
**问题本质:** AgentError::Other(#[from] anyhow) 自动转换吸收一切错误；register_with_kind 返回 Result<(), String>。修复 = 高频错误提升独立变体（CompactNoLlm/CompactEmptyResponse/BackgroundRegistryError）。
**通用模式:** `#[from] anyhow::Error` 是结构化错误的天敌；裸字符串 Result 无法分级处理。
**涉及文件:** peri-agent/src/error.rs, peri-middlewares/src/subagent/background.rs
**CLAUDE.md 链接:** false

### issue_2026-08-01-model-profiles-independent-config
**摘要:** Model Profile 独立配置——每档独立持有 provider/model/effort/max_tokens/context_1m
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** Profile 唯一事实源, 档位独立配置, 整体替换合并
**问题本质:** 四档共享一套 thinking/context 配置改为每档独立 ProfileConfig，请求参数唯一事实源；merge_overrides 按 Profile 整体替换（不做字段级合并）。
**通用模式:** 配置唯一事实源 + 按 Profile 整体替换，避免字段级合并的隐式优先级。
**涉及文件:** peri-acp/src/provider/config.rs（Profiles/ProfileConfig）
**CLAUDE.md 链接:** false

### issue_2026-08-02-openai-compatible-empty-tool-arguments-rejected
**摘要:** OpenAI-compatible 响应中空字符串工具参数导致整个响应被拒
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 宽松解析, 厂商差异, 空字符串参数
**问题本质:** 部分 OpenAI-compatible provider 对无参数工具返回 `"arguments": ""`，serde_json::from_str("") 失败把本可成功的调用当协议错误。
**通用模式:** 多厂商兼容解析对"空/缺失"边界取宽松语义（空串等价空对象），而非拒绝整个响应。
**涉及文件:** peri-model/src/openai_compatible/
**CLAUDE.md 链接:** false

### issue_2026-08-02-context-1m-missing-profile-silently-noop
**摘要:** context_1m 在 active profile 缺失时静默 no-op 却上报"已持久化"
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 静默 no-op, 假成功上报, 配置校验
**问题本质:** profiles.get_mut(&active_alias) 写入被静默跳过，仍调 persist_config 并打印成功——客户端看到陈旧值且无从得知失败。
**通用模式:** 写操作被跳过时禁止上报成功；配置入口键（active_alias）合法性先于字段校验。
**涉及文件:** peri-acp/src/host/requests.rs（configOption context_1m 分支）
**CLAUDE.md 链接:** false

### issue_2026-08-02-langfuse-ts-tooling-args-robustness
**摘要:** langfuse TS 分析脚本参数健壮性五连修复（analyze-ts/lib-ts/prompt-breakdown/trace-messages/trace-tokens）
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** 参数解析, parseInt||1, 负索引, 成本下限
**问题本质:** 五处 code-review Minor：--days 覆盖位置参数、cacheRead 超 input 负成本、--index 接受 0/负数/非数字（undefined.id 崩溃）、--detail 解析未使用。
**通用模式:** `parseInt(x) || 1` 把 0/NaN 静默归一；负索引越界前必须显式校验；独立字段禁止同一表达式求差出负。纯工具脚本小 bug，无额外认知。
**涉及文件:** langfuse 分析脚本（analyze.ts/lib.ts/prompt-breakdown.ts/trace-messages.ts/trace-tokens.ts）
**CLAUDE.md 链接:** false
