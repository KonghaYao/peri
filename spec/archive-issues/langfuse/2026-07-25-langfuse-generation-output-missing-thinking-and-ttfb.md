
# Langfuse trace 数据质量——GENERATION 缺 thinking output、缺 TTFB、tool-batch 空字段

**状态**：Archived
**优先级**：中
**创建日期**：2026-07-25
**类型**：技术债

## 问题描述

通过分析 trace `019f9818d3a17ea2b4c1ba64623d7683`（同步 subagent 场景），发现多个 Langfuse 数据上报质量问题，影响 trace 可观测性和诊断能力。

## 症状详情

### 症状 1：GENERATION output 为纯文本字符串，丢失 LLM 思考内容

两个 GENERATION 节点的 output 只保存了最终 `content` 文本，`reasoning_content`（DeepSeek 的思考过程）未写入 output，只在 input messages 中保留。

**实际数据**：

| Generation | output（当前） | reasoning_content（丢失） |
|---|---|---|
| 子 agent step-2 | `"命令执行完毕，输出为 \`hello\`。"` | `"The user wants me to run a shell command that sleeps for 10 seconds and then echoes "hello". I'll run it with an appropriate timeout since it takes about 10 seconds."` |
| 主 agent step-2 | `"子 agent 执行完毕：先 sleep 了 10 秒，然后输出 \`hello\`。"` | `"The user wants me to use a synchronous subagent that runs a shell command to say "hello" but first sleeps for 10 seconds. They want me to use \`run_in_background: false\` (synchronous).\n\nLet me launch a subagent with the coder agent type to do this."` |

期望 output 应为结构化对象：
```json
{
  "text": "...",
  "thinking": "...",
  "tool_calls": [...],
  "stop_reason": "end_turn"
}
```

### 症状 2：GENERATION 缺失 `completionStartTime` / `timeToFirstToken`

两个 GENERATION 节点的这两个字段均为 null。Langfuse UI 的"首 token 延迟"和"完成耗时"面板无法展示。

### 症状 3：tool-batch span 的 input / output 为 null

两个 `tool-batch` span 完全没有输入输出：

| tool-batch span | input | output | 建议补充 |
|---|---|---|---|
| 主 agent 的 tool-batch | null | null | `{"tool_count": 1, "tools": ["Agent"]}` |
| 子 agent 的 tool-batch | null | null | `{"tool_count": 1, "tools": ["Bash"]}` |

### 症状 4：subagent observation 的 output 混入内部元数据

subagent observation output 包含了 `child_thread_id`：
```
child_thread_id: 019f9818-debb-7e92-bebd-ab59a40da6ae
命令执行完毕，输出为 `hello`。
```

`child_thread_id` 是内部实现细节，不应出现在用户可见的 output 字段中。

## 复现条件

- **复现频率**：必现
- **触发步骤**：任意 subagent 场景在 Langfuse 中查看 trace 即可观察到
- **环境**：deepseek-v4-pro 模型

## 涉及文件

- `peri-acp/src/langfuse/tracer/mod.rs` —— `on_llm_end`（GENERATION output 构造，行 399-528）
- `peri-acp/src/langfuse/tracer/generation.rs` —— GenerationTracker（input 收集逻辑）
- `peri-acp/src/langfuse/tracer/tool_batch.rs` —— ToolBatch（batch span input/output）
- `peri-acp/src/langfuse/bridge.rs` —— SubagentStart/Stop 事件处理（subagent output 构造）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建——trace 019f9818d3a17ea2b4c1ba64623d7683 分析发现 |
| 2026-07-25 | Open | Fixed | agent | 修复：4 个症状全部修复，12 个文件，+166/-18 行，1005 tests pass |
| 2026-07-25 | — | Archived | agent | 归档

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）

### 修复 #1（2026-07-25）

- **操作人**：agent（auto-devflow: explore → plan → code → review → fix-review-issues → verify）
- **用户原意**：修复 4 个 Langfuse trace 数据质量问题
- **修复内容**：
  - **Fix 1**（结构化 output）：`reason.rs` 构建 JSON string（text/thinking/tool_calls/stop_reason），thinking 从 source_message Reasoning block 提取；`mod.rs` 添加 `parse_output()` 解析 + 降级
  - **Fix 2**（TTFB）：`types.rs` TokenUsage 加 `first_token_time` 字段；`openai/stream.rs` + `anthropic/stream.rs` 记录首 token 时间戳；`mod.rs` 填充 completion_start_time 和 TTFB metadata
  - **Fix 3**（tool-batch input/output）：`mod.rs` emit_tools_flush 中 batch 添加 tool_count/tools/duration_ms/tool_results
  - **Fix 4**（child_thread_id）：`mod.rs` 添加 `strip_child_thread_id()` 在 Langfuse 输出侧剥离前缀
  - 测试适配：`state_test.rs`、`token_test.rs`、`stream_test.rs`、`mapper_test.rs` 添加 `first_token_time: None`
  - Review 修复：(1) thinking 改为从 source_message.content_blocks 的 Reasoning block 提取 (2) output_preview 用 chars().take(200) 防 UTF-8 panic
- **涉及文件**：12 个（+166/-18 行）
- **验证状态**：已验证（build ✅ / peri-agent 688 ✅ / peri-acp 317 ✅ / review issues fixed ✅）
