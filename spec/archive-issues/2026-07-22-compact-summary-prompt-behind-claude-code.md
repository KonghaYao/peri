# Full Compact 摘要 Prompt 严重落后于 Claude Code，影响压缩后 Agent 恢复质量

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-22

## 问题描述

当前 Full Compact 的 LLM 摘要 Prompt 过于简陋——system prompt 仅 1 行，user prompt 的 9 段模板每段只有 1 句描述。对比 Claude Code 的 200+ 行详细 Prompt（含反幻觉保护、安全约束保留、真实用户消息识别、代码片段完整性要求），差距巨大。这直接影响 compact 后 Agent 能否准确恢复任务上下文——摘要质量差的 compact 等于失忆。

## 现状

### System Prompt（`summary_system_prompt.md`）

```
You are a conversation context compression tool. You excel at compressing long conversations into structured summaries.
```

仅 1 行。缺少：
- 禁止调用工具的明确指令（NO_TOOLS_PREAMBLE）
- 反幻觉保护（"你已拥有所需全部上下文"）
- 输出格式约束（`<analysis>` → `<summary>` 两阶段）

### User Prompt（`summary_user_prompt.md`）

9 段模板，每段仅 1 句描述。缺少：
- **安全约束保留**：Claude Code 要求 "security-relevant instructions must be preserved VERBATIM"
- **真实用户消息识别**：Claude Code 明确要求区分 `user-role turns` 和 `model-generated quotes`
- **代码片段完整性**：Claude Code 要求每文件附 code snippets + 重要性说明
- **任务引用**：Claude Code 要求 "include direct quotes from the most recent conversation showing exactly what task you were working on"
- **示例结构**：Claude Code 在 Prompt 内提供 `<example>` 块完整示例
- **Custom Instructions 支持**：Claude Code 支持 `/compact 聚焦XX` 的自定义 compact 指令

### 调用方式（`full.rs:97-104`）

```rust
let user_content = format!(
    "Compress the following conversation history:\n<conversation>\n{}\n</conversation>\n\n{}",
    conversation_text, SUMMARY_USER_PROMPT
);
let request = LlmRequest::new(vec![BaseMessage::human(user_content)])
    .with_system(SUMMARY_SYSTEM_PROMPT.to_string())
    .with_max_tokens(config.summary_max_tokens);
```

System prompt 作为独立 system 消息发送。Claude Code 的做法是将 system prompt 内容合并到 user message 或使用不同的 prompt 结构。

## 期望改进方向

将 `summary_system_prompt.md` 和 `summary_user_prompt.md` 升级到接近 Claude Code `prompt.ts` 的详细程度，至少包含：

1. **System Prompt 升级**：加入 NO_TOOLS_PREAMBLE（"CRITICAL: Respond with TEXT ONLY. Do NOT call any tools"），明确告知摘要模型不应调用工具
2. **User Prompt 每段指令深化**：9 段各自扩充到 3-5 行详细要求，而非当前 1 句话
3. **安全约束保留**：要求关键安全指令逐字保留
4. **用户消息识别**：区分真实 user 消息 vs 模型生成的引用
5. **示例结构**：在 Prompt 内提供 `<example>` 格式示例
6. **两阶段输出**：`<analysis>` 草稿 → `<summary>` 终稿

## 涉及文件

- `peri-agent/src/agent/compact_v2/descriptions/summary_system_prompt.md`（1 行）—— 待扩充到 ~20 行
- `peri-agent/src/agent/compact_v2/descriptions/summary_user_prompt.md`（17 行）—— 待扩充到 ~120 行
- `peri-agent/src/agent/compact_v2/full.rs:97-104` —— Prompt 组装逻辑可能需要微调
- `/Users/konghayao/code/ai/claude-code/src/services/compact/prompt.ts` —— 参照目标

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建：compact 摘要 Prompt 对标 Claude Code |
| 2026-07-22 | Open | Fixed | agent | 修复：升级 System/User Prompt，参照 Claude Code prompt.ts |

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **用户原意**：将 Full Compact 摘要 Prompt 升级到接近 Claude Code 的详细程度，提升 compact 后 Agent 恢复上下文的质量
- **修复内容**：
  - `summary_system_prompt.md`：从 1 行 → 8 行，加入 NO_TOOLS_PREAMBLE（禁止工具调用）、反幻觉保护、两阶段输出约束
  - `summary_user_prompt.md`：从 17 行 → 96 行，9 段各自深化到 3-5 行详细要求，加入 `<analysis>`→`<summary>` 两阶段流程、安全约束逐字保留规则、真实用户消息识别规则、代码片段完整性要求、`<example>` 格式示例、直接引用要求
- **涉及 commit**：待提交
- **验证状态**：已验证（cargo build + 54 peri-agent tests + 32 peri-acp tests 全部通过）
