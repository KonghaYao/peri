# Anthropic adapter 未对 request.system 调用 split_system_blocks，导致 main agent 系统提示词边界标记失效

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-17

## 问题描述

`build_request_body()` 在处理 `request.system` 时直接将整段文本作为一个 `SystemPromptBlock` push，不调用 `split_system_blocks()` 拆分。导致 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 边界标记变成普通文本存在于 system block 中，未起到静态/动态内容分离的缓存优化作用。当 system prompt 的动态部分（日期、cwd、agent 列表等）发生变化时，整段 system prompt 缓存失效。

fork agent 的 system prompt 反而因走 `BaseMessage::System` → `messages_to_anthropic()` → `split_system_blocks()` 路径，正确实现了块级缓存分离。

## 症状详情

| 维度 | 期望行为 | 实际行为 |
|------|----------|----------|
| Main agent `request.system` 拆分 | `split_system_blocks()` 将 system prompt 按边界标记拆为静态块（cache_control=true）+ 动态块（无 cache_control） | 整段作为一个 block，`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 保留为文本内容 |
| Fork agent system prompt 拆分 | — | 正确拆分（走 `BaseMessage::System` 路径） |
| 动态内容变化时 main agent 缓存 | 静态缓存前缀命中 | 整段失效，prefix miss |
| 缓存命中率 | README 声称 95-99% | 实际观察 fork agent 14%（但 fork 本身拆分是正确的；main agent 也可能受影响） |

**Anthropic API 发送对比**：

Main agent（边界标记失效）：
```json
"system": [{
  "type": "text",
  "text": "静态内容...\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\n动态日期/cwd...",
  "cache_control": {"type": "ephemeral"}
}]
```

Fork agent（边界标记生效）：
```json
"system": [
  {"type": "text", "text": "静态内容...", "cache_control": {"type": "ephemeral"}},
  {"type": "text", "text": "动态内容..."}
]
```

## 涉及文件

- `peri-agent/src/llm/anthropic/adapter.rs:284-292` —— `build_request_body()` 中对 `request.system` 的处理，直接 `push` 单块而非调用 `split_system_blocks()`
- `peri-agent/src/llm/anthropic/cache.rs:17-47` —— `split_system_blocks()` 已实现正确的拆分逻辑，但未被 `request.system` 路径调用
- `peri-acp/src/prompt/mod.rs:220` —— `build_system_prompt()` 在正确位置插入边界标记，但标记在 adapter 层未产生效果
- `peri-agent/src/llm/openai/adapter.rs` —— OpenAI adapter 可能也存在同样问题（需确认）

## 附：Fork agent system prompt 每次重建

**状态**：关联优化点（非 bug）

Fork agent 的 system prompt 每次调用都通过 `system_builder` 闭包重建（`execute_fork.rs:77` → `builder.rs:374-384`），涉及：

- 每次调用 `PromptFeatures::detect()` 读环境变量
- 每次调用 `format_available_agents()` 扫描磁盘 `.claude/agents/`
- 每次调用 `PromptEnv::with_frozen_date()` 检查 `.git` 存在性

虽然 fork agent 的静态/动态缓存分离是正确的，但每次重建 system prompt 字符串本身是不必要的 I/O 开销。可以复用 main agent 的 frozen system prompt 或对 `system_builder` 闭包输出做 memoization。

涉及文件：
- `peri-acp/src/agent/builder.rs:374-384` —— `system_builder` 闭包定义
- `peri-middlewares/src/subagent/tool/execute_fork.rs:77` —— fork 同步调用处

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |

## 修复记录

（待修复后追加）
