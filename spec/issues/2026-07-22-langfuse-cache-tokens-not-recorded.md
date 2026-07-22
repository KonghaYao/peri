# Langfuse 提示词缓存量（10k+）未记录

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-22

## 问题描述

每次 LLM generation 的提示词缓存 token 量（`cache_read_input_tokens` / `cache_creation_input_tokens`）均为 10k+ token，但在 Langfuse 仪表盘中完全不可见。用户通过 TUI 状态栏确认缓存命中率为正值，但 Langfuse Generation 的 Usage / Cache 指标为零。

## 症状详情

- TUI 状态栏显示缓存命中率 > 0（cache 量 10k+）
- Langfuse 仪表盘中 Generation 的 Token Usage 面板：
  - Input / Output / Total tokens 正常
  - **Cache Read / Cache Creation 为 0 或不可见**
- 所有 LLM 调用均受影响（每个 turn 的第一个 reason 步必触发）

## 静态分析结论（初步）

完整追踪了缓存数据的 4 条路径，代码层面均正确：

1. **Anthropic 适配器**（`anthropic/stream.rs:264-265`）：`TokenUsage` 中 `cache_read_input_tokens` / `cache_creation_input_tokens` 始终为 `Some()`
2. **Reason 阶段**（`reason.rs:197-226`）：`ObserveEvent::LlmCallEnd { cache_read_input_tokens, cache_creation_input_tokens }` 正确透传
3. **Bridge**（`bridge.rs:352-384`）：`from_observe_event` → `TokenUsage` 缓存字段仅 >0 时置 `Some`，值正常
4. **Tracer**（`mod.rs:367 + usage.rs:14-37`）：`build_usage_details` 正确生成 `cache_read_input_tokens` / `cache_creation_input_tokens`，写入 `GenerationBody.usage_details` → 最终映射到 OTEL `gen_ai.usage.{key}` 属性

## 待诊断方向

1. **运行时日志**：在 `on_llm_end` 打印 `usage_details` 确认 cache 键值实际存在
2. **Langfuse 原生 API**：使用批处理模式直接 POST `GenerationCreate`，绕过 OTEL 转换层对比
3. **Langfuse UI 侧**：确认当前 Langfuse 版本是否支持 `gen_ai.usage.cache_read_input_tokens` OTEL 属性映射
4. **非 OTEL 路径**：检查 `try_add_or_warn_via_session` 是否直接 API POST（非 OTEL 转换）

## 涉及文件

- `peri-acp/src/langfuse/tracer/usage.rs:14-37` —— `build_usage_details` 缓存转换
- `peri-acp/src/langfuse/tracer/mod.rs:348-456` —— `on_llm_end` → `emit_generation_create`
- `peri-acp/src/langfuse/bridge.rs:352-384` —— `from_observe_event` LlmCallEnd 映射
- `peri-agent/src/agent/stages/reason.rs:197-226` —— LlmCallEnd 发射（含 cache 字段）
- `langfuse-client/src/types/conversion.rs:175-198` —— OTEL 转换：`body.usage_details` → `gen_ai.usage.*`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建，静态分析确认代码路径正确，待运行时诊断 |
| 2026-07-22 | Open | Fixed | agent | 修复：usage_map（→ body.usage → OTEL langfuse.observation.usage_details）补充 cache_read/cache_creation |

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **根因**：`on_llm_end` 中 `usage_map`（→ `GenerationBody.usage` → OTEL `langfuse.observation.usage_details`）仅含 `input/output/total`，缺失 `cache_read_input_tokens` / `cache_creation_input_tokens`。Langfuse Tokens 面板读取 `langfuse.observation.usage_details` 显示缓存量。另一路径 `body.usage_details`（→ OTEL `gen_ai.usage.*`）虽有缓存但 Tokens 面板不用它。
- **修复内容**：在 `usage_map` 构建中追加 `cache_read_input_tokens`（若 `Some`）和 `cache_creation_input_tokens`（若 `Some`）共 14 行
- **涉及文件**：`peri-acp/src/langfuse/tracer/mod.rs:369-391`
- **涉及 commit**：待提交
- **验证状态**：cargo build 通过 / cargo test -p peri-acp --lib 296 passed
