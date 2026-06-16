# 系统提示词 领域

## 领域综述

系统提示词领域负责 Agent 系统提示词的架构设计，将单体提示词拆分为独立段落文件，支持基于功能的条件注入。

核心职责：
- 12 个 .md 段落文件按编号排序，8 个静态 + 4 个 feature-gated
- include_str! 编译时嵌入，零运行时开销
- PromptFeatures 从环境变量推断功能开关
- 动态覆盖块从 AgentOverrides 生成

## 核心流程

### 提示词合成流程

```
build_system_prompt(overrides, cwd, features)
  → 静态段落（01-08）: 始终 include_str!
  → Feature-gated 段落（10-13）: PromptFeatures 条件判断
  → 环境变量替换: {{cwd}}, {{is_git_repo}}, {{platform}}, {{os_version}}, {{date}}
  → AgentOverrides 覆盖块: persona/tone/proactiveness 注入到最前面
```

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 段落文件 | prompts/sections/ 目录，12 个 .md 文件按编号排序 |
| 静态段落 | 01_intro, 02_system, 03_doing_tasks, 04_actions, 05_using_tools, 06_tone_style, 07_communicating, 08_env |
| Feature-gated | 10_hitl, 11_subagent, 12_cron, 13_skills |
| 编译嵌入 | include_str! 宏，零运行时开销 |
| 条件注入 | PromptFeatures::detect() 从环境变量推断 |
| 环境变量 | PromptEnv::detect() 运行时环境检测 |

## Feature 附录

### feature_20260430_F001_system-prompt-restructure
**摘要:** 系统提示词拆分为独立段落文件并支持 Feature 条件注入
**关键决策:**
- 提示词从单体文件拆分为 sections/ 子目录下 12 个按编号排序的 .md 文件
- 8 个静态段落始终包含，4 个 Feature-gated 段落通过 PromptFeatures 条件注入
- 使用 include_str! 编译时嵌入，零运行时开销
- PromptFeatures::detect() 从环境变量推断，长期改为从中间件注册列表推断
- 同步 claude-code 工具 description 详细版本
- 工具名从 PascalCase 映射为 snake_case
**归档:** [链接](../../archive/feature_20260430_F001_system-prompt-restructure/)
**归档日期:** 2026-04-30

---

## Issue 经验附录

### issue_2026-05-13-system-prompt-dynamic-parts-duplicated-in-consecutive-calls

**摘要:** prepend_message 的 insert(0) 右移导致 StateSnapshot 包含 System 消息，下一轮动态内容重复注入
**状态:** Fixed
**归档日期:** 2026-05-14
**关键词:** prepend_message 右移, insert(0), StateSnapshot 泄露, System 消息泄露, agent_state_messages
**问题本质:** prepend_message 使用 insert(0) 将所有已有元素右移，导致 state.messages()[last_message_count..] 的快照范围包含被右移到该范围内的 System 消息。这些泄露的 System 消息通过 agent_state_messages 进入下一轮 history，与新的 prepend 消息合并产生重复。
**通用模式:** prepend_message 的 insert(0) 有隐式的范围扩大副作用——快照计算时必须考虑右移效应。StateSnapshot 应始终过滤 System 消息（.filter(|m| !m.is_system())），因为 System 消息由 middleware 每轮重新注入，不应持久化到历史中。
**架构影响:** 确认了 agent_state_messages 不应包含 BaseMessage::System 变体的设计原则。compact 路径也有独立的 System 消息泄露问题（直接将 compact 结果写入 agent_state_messages）。
**涉及文件:** peri-agent/src/agent/executor/final_answer.rs, peri-agent/src/agent/executor/mod.rs, peri-agent/src/agent/state.rs, peri-agent/src/llm/openai.rs, peri-tui/src/app/agent_ops.rs, peri-tui/src/app/agent_submit.rs, peri-tui/src/app/agent_compact.rs
**CLAUDE.md 链接:** true

### issue_2026-05-13-system-prompt-dynamic-cache-invalidation

**摘要:** System prompt 动态内容（date/cwd/middleware 注入）导致 Anthropic prompt cache 频繁失效，修复方案为边界标记拆分
**状态:** Fixed
**归档日期:** 2026-05-14
**关键词:** Prompt Cache, 边界标记, __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__, split_system_blocks, 动态占位符
**问题本质:** 整个 system prompt 作为单一缓存段，动态占位符（{{date}} 每日变化、{{cwd}} 跨项目变化）导致缓存前缀完全失效。边界标记方案将静态核心（01-06，~80% token）与动态内容（07_env + feature-gated + middleware，~20%）分离为独立缓存块。
**通用模式:** 缓存前缀稳定性原则——所有参与缓存前缀的数据必须保证跨请求稳定。动态内容应通过边界标记或独立块隔离到缓存段之外。此方案仿照 Claude Code 的 splitSysPromptPrefix() 模式。
**架构影响:** 引入 __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__ 标记和 split_system_blocks() 方法，但不改变 ReactLLM trait 签名。后续可进一步拆分 feature-gated 静态段落为独立缓存块（需变更 trait 签名）。
**涉及文件:** peri-tui/src/prompt.rs, peri-agent/src/llm/anthropic.rs
**CLAUDE.md 链接:** true

### issue_2026-05-13-missing-skillpreload-in-main-agent

**摘要:** 主 Agent 中间件链缺少 SkillPreloadMiddleware，/skill-name 预加载功能完全不可用
**状态:** Fixed
**归档日期:** 2026-05-14
**关键词:** SkillPreloadMiddleware, 中间件链缺失, /skill-name, 预加载
**问题本质:** agent.rs 和 agent_assembler.rs 两个构建路径均未注册 SkillPreloadMiddleware，导致系统提示声称支持 skill 预加载但实际不生效。子 Agent 路径通过 tool.rs 正确注册。
**通用模式:** 中间件链的完整性需要同时检查所有构建路径。功能声明（系统提示）与实际实现（中间件注册）必须保持一致。空参数的 SkillPreloadMiddleware（无 skill 列表）会 early return 无性能损耗，可安全始终注册。
**涉及文件:** peri-tui/src/app/agent.rs, peri-tui/src/app/agent_assembler.rs, peri-middlewares/src/subagent/skill_preload.rs, peri-middlewares/src/subagent/tool.rs
**CLAUDE.md 链接:** false

### issue_2026-05-13-askuserquestion-cache-hit-rate-drop

**摘要:** AskUserQuestion 导致缓存命中率极速下降
**状态:** Fixed
**归档日期:** 2026-05-14
**关键词:** AskUserQuestion, 缓存下降, system prompt 动态占位符
**问题本质:** 此 issue 是 system-prompt-dynamic-cache-invalidation 的子集表现。AskUserQuestion 调用时缓存下降的根因是 system prompt 动态占位符导致整段缓存失效，而非 AskUserQuestion 本身的问题。
**通用模式:** 表面症状（特定工具调用导致缓存下降）的根因可能在更底层（system prompt 架构）。修复底层问题后上层症状自动消除。
**涉及文件:** peri-middlewares/src/tool_search/middleware.rs, peri-agent/src/llm/anthropic.rs
**CLAUDE.md 链接:** false

### issue_2026-05-20-rapid-context-expansion
**摘要:** System Prompt 每轮重复注入导致上下文膨胀
**状态:** Fixed
**归档日期:** 2026-05-20
**关键词:** Frozen Session Data, system prompt 膨胀, prepend_message, StateSnapshot, 上下文爆炸
**问题本质:** 每轮 Agent 执行时重新构建 system prompt（含 MCP 工具注册），prepend_message 向消息头部插入导致 StateSnapshot 快照范围扩大，system prompt 内容跨轮次累积形成倍数膨胀
**通用模式:** 会话内不可变数据（system prompt、CLAUDE.md、skill summary、frozen_date）必须在 session/new 时构建一次并冻结，后续所有轮次直接使用已冻结值，禁止跨轮次重建
**架构影响:** FrozenSessionData 模式统一了所有会话内不可变数据的管理，成为系统提示词稳定性的基础设施
**技术决策:** session/new → 冻结全部不可变数据 → 后续轮次直接注入，消除重建开销和累积风险
**涉及文件:** peri-acp/src/session/executor.rs, peri-tui/src/acp_server/prompt.rs, peri-tui/src/acp_server/requests.rs
**CLAUDE.md 链接:** true

### issue_2026-05-23-mcp-tools-instability-breaks-anthropic-cache

**摘要:** Deferred Tools 段注入 system prompt 动态区域导致跨会话 Cache 部分失效
**状态:** Fixed
**归档日期:** 2026-05-24
**关键词:** Deferred Tools, Prompt Cache, cache_control断点, 动态区域
**问题本质:** MCP 连接状态不同 → Deferred Tools 内容不同 → 动态 system block 变化 → 第二个 cache breakpoint（`i == last_idx` fallback）失效，浪费缓存
**通用模式:** 动态区域的 cache_control 断点是不必要的——只有静态区域需要缓存；`i == last_idx` 的 fallback 逻辑会在动态 block 上加断点，应该去掉
**技术决策:** 动态 block 不加 cache_control 断点，确保静态段的缓存前缀
**涉及文件:** peri-middlewares/src/tool_search/tool_index.rs:284, peri-middlewares/src/tool_search/middleware.rs:79, peri-agent/src/llm/anthropic/invoke.rs:202-222, peri-agent/src/llm/anthropic/invoke.rs:331-334
**CLAUDE.md 链接:** true

### issue_2026-05-27-system-prompt-missing-language-instruction
**摘要:** 系统提示词缺少语言指示段落，AI 多轮对话后漂移至英文
**状态:** Fixed
**归档日期:** 2026-05-27
**关键词:** Language instruction, system prompt, LLM language drift, frozen_language
**问题本质:** 系统提示词中无任何语言偏好指令，LLM 在多轮对话后逐渐从中文漂移到英文。UI i18n 系统已存在但未传递给 LLM
**通用模式:** 语言偏好与 cwd/date 同属「在 session/new 时冻结、注入系统提示词动态区域」的数据。任何影响 LLM 行为的用户偏好都应进入系统提示词
**技术决策:** Language 段落放入 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 之后（动态区域），参考 Claude Code 的 `# Language` 格式
**涉及文件:** peri-acp/src/prompt/mod.rs, peri-acp/src/provider/config.rs, peri-acp/src/session/executor.rs, peri-tui/src/acp_server/prompt.rs
**CLAUDE.md 链接:** false

### issue_2026-05-27-language-injection-subagent-drift-cache-isolation
**摘要:** 语言段落注入导致 SubAgent 语言漂移和缓存隔离失效
**状态:** Fixed
**归档日期:** 2026-05-27
**关键词:** SubAgent language drift, frozen_language, cache isolation, last_idx fallback
**问题本质:** (1) SubAgent 从 peri_config.config.language 实时读取语言而非 frozen_language；(2) `i == last_idx` fallback 给动态 block 错误添加 cache_control；(3) session/load/resume/fork 丢失 frozen_language
**通用模式:** session/new 时冻结的所有配置必须通过 AcpAgentConfig 链传递到 SubAgent 构建路径。冻结值和实时配置是两条独立的数据通道，不能混用
**涉及文件:** peri-agent/src/llm/anthropic/invoke.rs, peri-acp/src/agent/builder.rs, peri-acp/src/session/executor.rs, peri-tui/src/acp_server/requests.rs
**CLAUDE.md 链接:** true

---

### issue_2026-06-17-mid-conversation-system-message-breaks-frozen-prompt
**摘要:** 中途通过 `add_message(BaseMessage::system(...))` 注入的工具失败警告和 stop_hook_feedback 被 LLM adapter hoist 到顶层 system prompt，破坏 frozen_system_prompt 稳定性（第一优先级）
**状态:** Fixed
**归档日期:** 2026-06-17
**关键词:** mid-conversation correction, system-reminder, frozen prompt stability, invoke hoisting, prompt cache
**问题本质:** `anthropic/invoke.rs` 和 `openai/invoke.rs` 在构造请求时遍历整个 `messages` 数组，把所有 `BaseMessage::System` 消息（不分位置）合并到顶层 system prompt。中途用 `add_message(System)` 注入"工具连续失败警告"或"stop_hook_feedback"会让这些内容被 hoist 进 system prompt，每轮都变化，导致 Prompt Cache 失效、模型行为漂移。
**通用模式:** 中途纠正/警告类消息（工具失败提示、hook 反馈、goal steering、compact 续接）必须用 `BaseMessage::human("<system-reminder>...</system-reminder>")` 注入，禁止 `BaseMessage::system(...)`。Human 消息进入对话历史，LLM 可见但不污染 system prompt 缓存前缀。已采用此模式的实现：`goal_middleware.rs`、`compact_middleware.rs`、`tool_dispatch.rs`、`hooks/middleware.rs`。
**涉及文件:** peri-agent/src/agent/executor/tool_dispatch.rs, peri-middlewares/src/hooks/middleware.rs, peri-agent/src/llm/anthropic/invoke.rs, peri-agent/src/llm/openai/invoke.rs
**CLAUDE.md 链接:** true

---

### issue_2026-06-17-subagent-ignores-frozen-claude-md
**摘要:** SubAgent 中间件链不复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md/Skills，每次 spawn 重新读盘，会话中文件变更导致 SubAgent 与 main agent 行为漂移
**状态:** Fixed
**归档日期:** 2026-06-17
**关键词:** SubAgent, frozen data, CLAUDE.md, skill summary, prompt drift, defense in depth
**问题本质:** `build_subagent_middlewares` 直接 `AgentsMdMiddleware::new()` 和 `SkillsMiddleware::new().with_global_config()`，未调用 `with_frozen_content` / `with_frozen_summary`。SubAgent spawn 时从磁盘重新读取 CLAUDE.md/skills 文件——若文件中途被修改（用户编辑、hook 改写、git checkout），SubAgent 看到与 main agent 不同的内容。
**通用模式:** session/new 时冻结的所有内容（CLAUDE.md、CLAUDE.local.md、skill summary、system prompt、date、language）必须沿调用链显式透传到所有子 Agent 构建路径。冻结值不能依赖磁盘重读。`Option<Arc<String>>` 共享避免高频路径（如 `build_tool`）的字符串 clone 开销。
**涉及文件:** peri-middlewares/src/subagent/mod.rs, peri-middlewares/src/subagent/tool/mod.rs, peri-middlewares/src/subagent/tool/define.rs, peri-middlewares/src/subagent/tool/build_agent.rs, peri-middlewares/src/subagent/tool/execute_fork.rs, peri-middlewares/src/subagent/tool/execute_bg.rs, peri-middlewares/src/subagent/spawner.rs, peri-acp/src/agent/builder.rs, peri-acp/src/session/executor.rs, peri-acp/src/session/command/mod.rs, peri-acp/src/session/command/bg.rs
**CLAUDE.md 链接:** true

---

## 相关 Feature
- → [agent.md](./agent.md) — ReActAgent.with_system_prompt() 注入
- → [tui.md](./tui.md) — TUI 层 build_system_prompt() 调用
