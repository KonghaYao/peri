# Model / LLM Provider 领域

## 领域综述

peri-model crate：与模型提供商无关的协议 DTO 与流式优先模型接口，统一 OpenAI / Anthropic / 兼容端点的适配层。`Model` trait 只定义流式入口 `stream()`，非流式 `complete()` 由事件聚合的默认实现提供（无独立非流式 HTTP 路径）；`openai_compatible/` 与 `anthropic/` 两个 adapter 只产生和消费标准 `peri-model` 协议，不引用 Agent 事件或类型。runtime 层统一提供 HTTP/SSE 传输、重试与安全观测（`PreparedModelRequest` 脱敏投影）；Anthropic 路径内置 prompt cache（system 静态/动态拆分 + messages 缓存点打标）。Agent 侧经 `peri-agent/src/agent/model_bridge.rs` 的 `AgentModelBridge` 接入 ReAct（实现 `ReactLLM`），ACP 侧经 `peri-acp/src/provider/mod.rs` 的 `ProviderConfig::into_model()` 装配配置与模型。

## 核心流程

- **Provider 请求构建**：上层构造 `ModelRequest`（`ModelMessage` 序列 + `ToolDefinition` 列表）→ adapter 的 `build_request()` 生成 provider 原生 body（`BuiltOpenAiRequest` / `BuiltAnthropicRequest`）→ `Model::stream()` 发出 HTTP 请求
- **适配**：`HttpTransport` seam + `SseParser` 字节级解析 → provider decoder 将 SSE event 转为标准 `ModelStreamEvent`（TextDelta / ReasoningDelta / ToolCallDelta / Usage / Completed）
- **BaseMessage 序列**：`ModelStreamEvent::Completed(ModelResponse)` 携带完整 Assistant 消息；`complete()` 聚合增量事件并按需回填（`set_text_if_empty` / `set_tool_calls_if_empty` 等），流式与非流式产出同一 `ModelResponse`
- **流式/非流式一致性约束**：无独立非流式路径，非流式 = 流式事件聚合，天然同源；`ModelResponse` 只允许 Assistant 消息（否则 `AssistantMessageRequired`）；ToolCall 增量按 index 累积，缺 id/name 或 arguments 非法报 `ProtocolErrorKind::ToolCall*`

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 适配架构 | `Model` trait（stream-first，`prepare_request` + `stream` + 默认 `complete`）；`openai_compatible/` 与 `anthropic/` 双 adapter，仅产出/消费 `peri-model` 协议 |
| 协议 | OpenAI Chat Completions（`reasoning_content`/`reasoning` delta、qwen 特判 `stream_options.include_usage`、kimi 特判移除 `reasoning_effort`）；Anthropic Messages API（`x-api-key` + `anthropic-version: 2023-06-01`） |
| 流式实现 | SSE 字节级 parser → provider decoder → 标准事件流；`CancellationToken` child token 取消，`ModelStream::abort()` 只取消本流，不反取消父级 |
| 错误与重试 | `ModelError`（Transport / HttpStatus / Protocol / Cancelled / StreamInterrupted / RetryExhausted）；`RetryConfig` 默认 max_attempts=6（含首次）、base 500ms、max 32s、jitter；可重试类：transport、408/429、5xx、协议瞬态子集（Provider / InvalidJsonObject / StreamEndedWithoutCompleted） |
| 缓存策略 | 仅 Anthropic prompt cache：`AnthropicConfig.enable_cache`（默认 true）+ header `anthropic-beta: prompt-caching-2024-07-31`；system 按 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 拆静态/动态块，messages 首、倒数第 1/2 条 user 消息打 `cache_control: ephemeral` |
| token 计量 | `TokenUsage`（input / output / cache_creation_input_tokens / cache_read_input_tokens）；Anthropic 于 message_start / message_delta / message_stop 更新，OpenAI 于 chunk usage 更新 |
| 安全观测 | `prepare_request()` → `PreparedModelRequest`（endpoint 路径、敏感键、data URI 脱敏 + redacted/truncated_paths）；凭据仅存于 Config 内部，Debug 输出 `[REDACTED]`（ARC-SECRET-001） |
| 序列化确定性 | 工具/消息序列化顺序稳定（BTreeMap / 固定注册顺序），保护 prompt cache 前缀（ARC-SERIAL-001） |

## 稳定入口

| 路径 | 职责 |
|------|------|
| `peri-model/src/protocol/model.rs` | `Model` trait、`ModelStream`（child token 取消）、`ModelStreamEvent`、`complete()` 聚合 |
| `peri-model/src/protocol/types.rs` | 协议 DTO：`ModelMessage` / `ModelRequest` / `ModelResponse` / `ContentBlock` / `ToolCall` / `ToolResult` / `TokenUsage` / `StopReason` / `ProviderProtocol` 等 |
| `peri-model/src/runtime/` | `ModelRuntimeConfig`（观测 + 重试 + observer）、`PreparedModelRequest` 脱敏投影、`retrying_http_sse_stream` 链路、`RetryConfig` / `RetryableErrorClasses`、`ModelError` |
| `peri-model/src/transport/` | `HttpTransport` seam、`ReqwestTransport`、`SseParser`（crate 内部使用，公共 API 不暴露 client/headers） |
| `peri-model/src/openai_compatible/` | `OpenAiConfig` / `OpenAiModel`（Chat Completions adapter） |
| `peri-model/src/anthropic/` | `AnthropicConfig` / `AnthropicModel` + `cache.rs`（prompt cache 拆分与打标） |
| `peri-agent/src/agent/model_bridge.rs` | `AgentModelBridge`（实现 `ReactLLM`，trait 见 `peri-agent/src/agent/react.rs`）：`BaseMessage` ↔ `ModelMessage` 转换、流事件转发为 TextChunk / AiReasoning 并组装 `Reasoning`、`generate_reasoning_with_observed_body` 单次构建 request 复用观测体、`provider_capabilities()` 由观测投影判定协议身份（Anthropic 签名 reasoning 必须整体保留，供 compact 投影） |
| `peri-acp/src/provider/mod.rs` | `ProviderConfig::into_model()`：profile 配置 → `OpenAiConfig` / `AnthropicConfig` + Model 装配，注入 retry observer 与 `with_full_observation()` runtime |

---

## Issue 经验附录

相关历史 issue 见 domains/agent.md，不迁移条目。
