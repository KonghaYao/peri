# `peri-model` 标准模型协议 crate 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 `peri-agent` 内的模型协议、HTTP/SSE、OpenAI-compatible/Anthropic adapter 和重试迁至独立的 `peri-model` crate，同时让 Agent、ACP 与 middleware 改用统一的标准协议。

**Architecture:** `peri-model` 仅依赖通用 Rust 库，拥有 model-neutral DTO、流式优先的 `Model` trait、安全的观测请求投影、取消/强制断连和 retry。`peri-agent` 保留 ReAct bridge、Agent events 与 compact；`peri-acp` 保留配置、factory、池与 Langfuse；所有旧 `peri_agent::llm` API 在单次迁移中删除。

**Tech Stack:** Rust 2021、Tokio、reqwest、futures、tokio-util、serde/serde_json、async-trait、thiserror、tracing。

---

## 迁移前提与完成条件

- 需求事实源：`spec/issues/2026-07-31-extract-peri-model-protocol-crate.md`。
- 现有 P1-7 adapter 设计仅作为历史参考；本计划以新 crate 及破坏性迁移为准。
- 迁移期间不以真实 Provider API 运行测试。HTTP/SSE 均使用本地 fake transport 或 fixture。
- `PreparedModelRequest` 默认提供受限、脱敏、可标识截断路径的观测投影；若 Langfuse 需要完整内容，必须由 ACP 明确 opt-in，且不得经日志或错误正文绕过。
- 最终工作区不能在生产代码、测试、doc comment 或 manifest 中保留旧 `peri_agent::llm` 路径或旧 LLM 类型名称。

## 文件结构

### 新建 `peri-model`

```text
peri-model/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── protocol/{mod.rs,types.rs,model.rs,types_test.rs,model_test.rs}
    ├── runtime/{mod.rs,error.rs,request.rs,retry.rs,stream.rs,*_test.rs}
    ├── transport/{mod.rs,http.rs,sse.rs,sse_test.rs}
    ├── openai_compatible/{mod.rs,request.rs,response.rs,stream.rs,mod_test.rs}
    └── anthropic/{mod.rs,cache.rs,request.rs,response.rs,stream.rs,mod_test.rs}
```

### 关键修改

- Workspace：`Cargo.toml`、`Cargo.lock`、`peri-agent/Cargo.toml`、`peri-acp/Cargo.toml`、`peri-middlewares/Cargo.toml`。
- Agent：新增 `peri-agent/src/agent/model_bridge.rs` 及测试；修改 `react.rs`、`stages/reason.rs`、compact projection/full、token、events 和错误转换；删除 `peri-agent/src/llm/`。
- ACP：修改 `provider/mod.rs`、agent builder、session pool/executor/prediction、Langfuse bridge/tracer 和 ACP event mapper。
- Middlewares：迁移 Goal、HITL 和 SubAgent v2 bridge 对底层模型的依赖。

---

### Task 1: 创建 crate 与冻结协议契约

**Files:**
- Modify: `Cargo.toml`
- Create: `peri-model/Cargo.toml`
- Create: `peri-model/src/lib.rs`
- Create: `peri-model/src/protocol/mod.rs`
- Create: `peri-model/src/protocol/types.rs`
- Create: `peri-model/src/protocol/types_test.rs`
- Create: `peri-model/src/protocol/model.rs`
- Create: `peri-model/src/protocol/model_test.rs`

- [x] **Step 1: 将 `peri-model` 加入 workspace，并建立最小依赖集**

`peri-model/Cargo.toml` 只添加协议层实际需要的通用依赖：`async-trait`、`futures`、`serde`、`serde_json`、`thiserror`、`tokio`、`tokio-util`、`chrono`、`uuid`、`reqwest`、`tracing` 和 `rand`。不得引用任何 `peri-*` crate。

- [x] **Step 2: 先写失败的标准 DTO 测试**

在 `types_test.rs` 固定以下行为：

```rust
#[test]
fn tool_call_and_result_preserve_structured_fields() {
    let call = ToolCall::new("call_1", "shell", json!({"command": "pwd"}));
    let result = ToolResult::success("call_1", "shell", "output");

    assert_eq!(call.id(), "call_1");
    assert_eq!(call.name(), "shell");
    assert!(result.is_success());
}

#[test]
fn model_response_requires_an_assistant_message() {
    assert!(ModelResponse::new(ModelMessage::user_text("x"), StopReason::EndTurn, None, None)
        .is_err());
}

#[test]
fn anthropic_redacted_reasoning_is_a_standard_content_variant() {
    assert!(matches!(
        ContentBlock::RedactedReasoning { .. },
        ContentBlock::RedactedReasoning { .. }
    ));
}
```

测试还要覆盖 `StopReason`、`TokenUsage`、`ModelCapabilities` 与所有 message/tool DTO 的 serde roundtrip。

- [x] **Step 3: 实现仅属于协议层的标准 DTO**

定义：`ModelRequest`、`ModelResponse`、`ModelMessage`、`ContentBlock`、`ToolDefinition`、`ToolCall`、`ToolResult`、`TokenUsage`、`StopReason`、`ModelCapabilities` 和 `ProviderProtocol`。schema/arguments 允许使用受约束 JSON-object 新类型，禁止将整个领域对象替换为裸 `Value`。

`ModelMessage` 必须显式表达 `System`、`User`、`Assistant` 和 `ToolResult`；Assistant 必须能同时承载 content 与 tool calls，确保现有消息回放不丢失任一侧。

- [x] **Step 4: 写失败的 stream-first trait 测试**

在 `model_test.rs` 用 fake `Model` 验证 `complete()` 只聚合 `stream()`：

```rust
#[tokio::test]
async fn complete_aggregates_stream_events() {
    let response = FakeModel::with_events(vec![
        ModelStreamEvent::TextDelta { text: "hello".into() },
        ModelStreamEvent::Usage(TokenUsage::new(1, 2)),
        ModelStreamEvent::Completed(assistant_response("hello")),
    ])
    .complete(request(), CancellationToken::new())
    .await
    .unwrap();

    assert_eq!(response.assistant_text(), Some("hello"));
    assert_eq!(response.usage.unwrap().output_tokens, 2);
}
```

另测：无 `Completed` 即结束返回 `ModelError::Protocol`；取消返回 `ModelError::Cancelled`。

- [x] **Step 5: 实现 `Model`、`ModelStream` 与默认 `complete()`**

`Model::stream` 为主入口，`complete()` 仅消费 `ModelStreamEvent`。`ModelStream` 既是可消费 stream，也持有内部取消句柄；接口必须提供 `abort(&self)`。

- [x] **Step 6: 运行协议测试**

Run:

```bash
cargo check -p peri-model
cargo test -p peri-model --lib protocol::
cargo test -p peri-model --doc
```

Expected: 全部通过，且 `cargo metadata` 显示 `peri-model` 不依赖任意其他 Peri crate。

---

### Task 2: 实现安全错误、请求准备与观测投影

**Files:**
- Create: `peri-model/src/runtime/mod.rs`
- Create: `peri-model/src/runtime/error.rs`
- Create: `peri-model/src/runtime/error_test.rs`
- Create: `peri-model/src/runtime/request.rs`
- Create: `peri-model/src/runtime/request_test.rs`
- Modify: `peri-model/src/lib.rs`
- Modify: `peri-model/src/protocol/model.rs`

- [x] **Step 1: 写失败的 secret-safe `ModelError` 测试**

```rust
#[test]
fn model_error_never_formats_request_secrets_or_raw_body() {
    let error = ModelError::http_status(401, "openai", Some("request_123"));
    let rendered = format!("{error:?} {error}");

    assert!(!rendered.contains("sk-live-secret"));
    assert!(!rendered.contains("Authorization"));
    assert!(!rendered.contains("very long user prompt"));
}
```

覆盖 `Transport`、`HttpStatus`、`Protocol`、`Cancelled`、`StreamInterrupted` 与 retry exhausted。错误只可保留安全 provider/status/request-id/受限摘要。

- [x] **Step 2: 实现结构化 `ModelError` 与 crate-local `Result`**

使用 `thiserror`。禁止接收或保存完整请求 body、响应 body、headers、API key、cookie、`reqwest::Client`。对 Provider 失败正文只提取受限、脱敏的 code/message summary。

- [x] **Step 3: 写失败的 `PreparedModelRequest` 同源与脱敏测试**

测试至少断言：

```rust
#[test]
fn observed_request_has_no_credentials_or_headers() {
    let observed = prepared_request();
    let json = serde_json::to_string(&observed).unwrap();

    assert!(!json.contains("sk-live-secret"));
    assert!(!json.contains("Authorization"));
    assert!(!json.contains("Cookie"));
}

#[test]
fn oversized_tool_output_is_replaced_and_its_path_is_recorded() {
    let observed = observe_body(json!({"messages": [{"content": "x".repeat(100_000)}]}));
    assert!(observed.truncated_paths().contains(&"/messages/0/content".into()));
}
```

- [x] **Step 4: 实现 `PreparedModelRequest`、`ObservedProviderBody` 与 observation config**

`PreparedModelRequest` 仅公开 `protocol`、`model_id`、规范化 `Url` endpoint、受控 JSON body、safe metadata、`redacted_paths` 和 `truncated_paths`。内部发送请求的 headers 与 client 必须私有。默认观测策略限长并脱敏 data URI、敏感键和过大字段；完整内容仅通过 ACP 显式 opt-in 传入 `ModelRuntimeConfig`。

- [x] **Step 5: 运行 runtime 安全测试**

Run:

```bash
cargo test -p peri-model --lib runtime::error::
cargo test -p peri-model --lib runtime::request::
```

Expected: 所有错误和观测序列化断言通过，测试 fixture 不含真实凭据。

---

### Task 3: 迁移 SSE、统一取消与协议层 retry

**Files:**
- Create: `peri-model/src/transport/mod.rs`
- Create: `peri-model/src/transport/http.rs`
- Create: `peri-model/src/transport/sse.rs`
- Create: `peri-model/src/transport/sse_test.rs`
- Create: `peri-model/src/runtime/stream.rs`
- Create: `peri-model/src/runtime/stream_test.rs`
- Create: `peri-model/src/runtime/retry.rs`
- Create: `peri-model/src/runtime/retry_test.rs`

- [x] **Step 1: 移植并扩展 SSE parser 的失败测试**

从 `peri-agent/src/llm/sse_test.rs` 迁移现有测试，再增加：CRLF、多 event 同 chunk、多行 `data:`、跨 UTF-8 code point、`[DONE]` 和不完整尾部。SSE parser 只输出 event/data 文本；JSON 错误由 provider decoder 映射为 `ModelError::Protocol`。

- [x] **Step 2: 实现独立 `SseParser` 与可 fake 的 HTTP transport seam**

`transport/http.rs` 定义 crate-private transport abstraction，用于本地测试模拟 connect、status、byte chunks 和 mid-stream failure。生产实现包装 `reqwest::Client`，但不将 client 或 headers 暴露给 protocol API。

- [x] **Step 3: 写失败的取消和 retry 行为测试**

必须覆盖：

```rust
#[tokio::test]
async fn retries_before_first_visible_event() { /* 429 -> success */ }

#[tokio::test]
async fn never_retries_after_text_delta() { /* TextDelta -> disconnect -> StreamInterrupted */ }

#[tokio::test]
async fn abort_stops_connect_read_and_backoff() { /* abort 后 attempt 不增加 */ }
```

完整矩阵包括：408/429/5xx、transport failure、400/401/403/404、首个 `TextDelta`/`ReasoningDelta`/`ToolCallDelta` 前后、`Usage`/`Completed` 后、请求建立/等待 SSE/body read/backoff 中的外部取消、`abort()`。

- [x] **Step 4: 实现 `RetryConfig`、`ModelRuntimeConfig` 和 retry observation**

retry observation 只带 `attempt`、`max_attempts`、delay 与安全错误分类；不得引用 Agent events/metrics/Langfuse。backoff 必须由 `tokio::select!` 与 cancellation 竞争，取消后不再开始 attempt。

- [x] **Step 5: 实现 `ModelStream::abort()` 的强制断连**

内部 token 必须被请求建立、response body reader、SSE decode loop、retry retry-loop/backoff 同时监听。`abort()` 触发 token、drop 在途 response/body、并让消费端收到 `ModelError::Cancelled`。禁止把取消归类为 retryable failure。

- [x] **Step 6: 运行 transport/retry 测试**

Run:

```bash
cargo test -p peri-model --lib transport::
cargo test -p peri-model --lib runtime::stream::
cargo test -p peri-model --lib runtime::retry::
```

Expected: 首可见事件前会重试；任一可见 delta 后不会发起第二次 HTTP attempt；abort 与外部取消立即结束。

---

### Task 4: 迁移 OpenAI-compatible adapter 与契约测试

**Files:**
- Create: `peri-model/src/openai_compatible/mod.rs`
- Create: `peri-model/src/openai_compatible/request.rs`
- Create: `peri-model/src/openai_compatible/response.rs`
- Create: `peri-model/src/openai_compatible/stream.rs`
- Create: `peri-model/src/openai_compatible/mod_test.rs`
- Modify: `peri-model/src/lib.rs`
- Source: `peri-agent/src/llm/openai/{mod.rs,adapter.rs,invoke.rs,stream.rs}`
- Source tests: `peri-agent/src/llm/openai_test.rs`, `peri-agent/src/llm/openai/stream_test.rs`

- [x] **Step 1: 先迁移 OpenAI request/response contract tests**

保留现有对 system placement、tool schema、tool result、`reasoning_content`/`reasoning`、Qwen `stream_options.include_usage`、Kimi thinking 与 LiteLLM session metadata 的精确断言。fixture 语义不得在迁移时“顺手简化”。

- [x] **Step 2: 写 stream-first OpenAI adapter 的失败测试**

测试 `OpenAiModel::new(OpenAiConfig)`：`prepare_request()` 的 observed body 与 fake transport 捕获的实际 JSON 同源；`stream()` 输出标准 text/reasoning/tool call/usage/completed 事件；多 tool index 交错时 arguments 按 index 正确累积。

- [x] **Step 3: 实现 `OpenAiConfig` 与 `OpenAiModel`**

config 包含 endpoint、model、认证凭据载体、thinking/max token 配置和 `ModelRuntimeConfig`；禁止不安全 `Debug`。adapter 内部构建完整 private prepared request；公共 `prepare_request()` 仅返回安全 projection；`stream()` 使用同一内部结果。

- [x] **Step 4: 实现 response/SSE 标准化与错误脱敏**

SSE decoder 输出 `ModelStreamEvent`，而不是 Agent event。`ToolCallDelta` 采用稳定 index accumulator。解析失败不得输出原始 arguments 或 response body；返回带受限摘要的 `ModelError`。

- [x] **Step 5: 运行 OpenAI-compatible 契约测试**

Run:

```bash
cargo test -p peri-model --lib openai_compatible::
```

Expected: 所有既有 OpenAI-compatible payload 与 streaming 语义保持，且无 Agent 类型进入 crate。

---

### Task 5: 迁移 Anthropic adapter、cache 与契约测试

**Files:**
- Create: `peri-model/src/anthropic/mod.rs`
- Create: `peri-model/src/anthropic/cache.rs`
- Create: `peri-model/src/anthropic/request.rs`
- Create: `peri-model/src/anthropic/response.rs`
- Create: `peri-model/src/anthropic/stream.rs`
- Create: `peri-model/src/anthropic/mod_test.rs`
- Source: `peri-agent/src/llm/anthropic/{mod.rs,adapter.rs,cache.rs,invoke.rs,stream.rs}`
- Source tests: `peri-agent/src/llm/anthropic_test.rs`

- [x] **Step 1: 先迁移 Anthropic payload contract tests**

固定顶层 system placement、cache control、`SYSTEM_PROMPT_DYNAMIC_BOUNDARY`、extended thinking、signature、`redacted_thinking`、连续 tool result 合并和 token usage/request-id 语义。

- [x] **Step 2: 写 Anthropic stream/取消的失败测试**

覆盖 `message_start` usage、thinking/text/tool deltas、block finalize、message stop、header 优先 request id、取消和首可见事件后的 `StreamInterrupted`。

- [x] **Step 3: 实现 `AnthropicConfig` 与 `AnthropicModel`**

从 `AnthropicConfig` 构建 private requests；保留 cache 与 thinking 所需的 provider-specific serializer，但只输出标准 `ModelMessage`/`ContentBlock`/`ModelStreamEvent`。`redacted_thinking` 映射为协议 DTO 的显式变体。

- [x] **Step 4: 删除不安全 provider 请求 dump**

不得迁移现有 Anthropic 500 路径对 request messages/body 的 debug dump。日志只包含 provider、model、HTTP status、request id 与受限错误类别。

- [x] **Step 5: 运行 Anthropic 契约测试和 crate 检查**

Run:

```bash
cargo test -p peri-model --lib anthropic::
cargo test -p peri-model --lib
cargo test -p peri-model --doc
cargo clippy -p peri-model --all-targets -- -D warnings
```

Expected: 协议 crate 自身完整通过；无真实网络调用。

---

### Task 6: 在 `peri-agent` 建立标准协议到 ReAct 的 bridge

**Files:**
- Create: `peri-agent/src/agent/model_bridge.rs`
- Create: `peri-agent/src/agent/model_bridge_test.rs`
- Modify: `peri-agent/Cargo.toml`
- Modify: `peri-agent/src/lib.rs`
- Modify: `peri-agent/src/agent/react.rs`
- Modify: `peri-agent/src/agent/events.rs`
- Modify: `peri-agent/src/agent/stages/reason.rs`
- Modify: `peri-agent/src/agent/compact_v2/{projection.rs,full.rs}`
- Modify: `peri-agent/src/agent/token.rs`
- Modify: `peri-agent/src/error.rs`

- [x] **Step 1: 写 Agent message/tool 转换的失败测试**

Bridge tests 必须固定：assistant content 与 tool calls 同时存在；tool id/name/arguments/order/is_error 不丢失；system/tool result/连续 tool result 不重排；reasoning signature 与 redacted reasoning 由 Agent 类型到 model DTO 往返保存。不一致数据必须 fail closed，不能静默选择某一侧。

- [x] **Step 2: 实现 `AgentModelBridge`**

bridge 持有 `Arc<dyn peri_model::Model>`、session system prompt 和 session id，实现保留在 Agent 的 `ReactLLM`。它将 `BaseMessage`/`BaseTool` 显式转换为 `ModelRequest`，将 completion response 转为 `Reasoning`。

- [x] **Step 3: 实现 stream event 到 Agent event 的单向映射**

`TextDelta -> ExecutorEvent::TextChunk`，`ReasoningDelta -> ExecutorEvent::AiReasoning`。`ToolCallDelta` 仅在 bridge 内累积，直到 `Completed` 转为 `Reasoning.tool_calls`；不得提前发 Agent 工具执行事件，保持 Act stage 的事件顺序。

- [x] **Step 4: 接入 Langfuse 观测与 capability 映射**

Reason stage 只读取 bridge 提供的 `PreparedModelRequest` 观测投影，禁止再自行构造 provider body。将 `peri_model::ModelCapabilities` 单向映射为 Agent compact projection capability；projection policy 仍归 Agent。

- [x] **Step 5: 迁移 usage/stop reason 和取消错误**

`Reasoning`、TokenTracker、Agent events 改依赖 `peri_model::TokenUsage`/`StopReason`。`ModelError::Cancelled` 映射为当前 Agent interrupt 语义；其他 ModelError 映射为受限 AgentError，不暴露请求正文。

- [x] **Step 6: 运行 Agent bridge 测试**

Run:

```bash
cargo test -p peri-agent --lib model_bridge
cargo test -p peri-agent --lib agent::stages::reason::
cargo test -p peri-agent --lib agent::compact_v2::
```

Expected: Bridge 保持事件顺序、工具调用与 Langfuse request-body 同源，且取消后不再发 event。

---

### Task 7: 迁移 ACP factory、池、Langfuse 与 middleware 调用方

**Files:**
- Modify: `peri-acp/Cargo.toml`
- Modify: `peri-acp/src/provider/mod.rs`
- Modify: `peri-acp/src/agent/builder.rs`
- Modify: `peri-acp/src/session/{agent_pool.rs,executor.rs,executor_helpers.rs,prediction.rs}`
- Modify: `peri-acp/src/event/{mapper.rs,mapper_test.rs}`
- Modify: `peri-acp/src/langfuse/{bridge.rs,tracer/generation.rs,tracer/usage.rs}`
- Modify: `peri-middlewares/Cargo.toml`
- Modify: `peri-middlewares/src/{goal/tool.rs,hitl/auto_classifier.rs,subagent/v2_bridge.rs}`
- Test: `peri-acp/src/provider/config_test.rs`, `peri-acp/src/session/agent_pool_test.rs`, Langfuse tests, Goal/HITL/SubAgent tests

- [x] **Step 1: 写 ACP config 到强类型 model config 的失败测试**

保持当前 env、provider alias、thinking、base URL、model fallback 语义，但测试最终结果是 `OpenAiConfig` 或 `AnthropicConfig`，而不是 `ChatOpenAI`/`ChatAnthropic`。环境读取仍只能发生在 ACP。

- [x] **Step 2: 改写 `LlmProvider::into_model()` 与 `AgentPool`**

factory 返回 `Box<dyn peri_model::Model>`；`AgentPool`、cached/auxiliary models、HITL/Goal 模型改为 `Arc<dyn peri_model::Model>`。保留 ACP 的 provider fingerprint、cache invalidation 与实例生命周期，并补 base URL/API key/配置变化不误命中的回归测试。

- [x] **Step 3: 移除 ACP 中的 Agent-level `RetryableLLM`**

builder 使用 `AgentModelBridge` 构建 `ReactLLM`。retry 策略只由 `ModelRuntimeConfig` 决定；ACP/Langfuse 仅消费 protocol-neutral retry observation，不影响 retry 决策。

- [x] **Step 4: 迁移 Langfuse 与 ACP DTO 映射**

Langfuse generation input 使用 `PreparedModelRequest` 的受限 body，不再根据 messages/tools 重新序列化。`peri-acp-types` 不添加 `peri-model` 依赖；在 ACP mapper 中显式进行 `peri_model::{TokenUsage, StopReason}` 到 DTO 映射。

- [x] **Step 5: 迁移 middlewares**

Goal/HITL 直接使用 `Model`/`ModelRequest`/`ModelResponse`。SubAgent 继续以 `ReactLLM` 执行 Agent loop，仅在 compact helper 模型处使用 `Arc<dyn Model>`。

- [x] **Step 6: 运行跨 crate 回归测试**

Run:

```bash
cargo test -p peri-acp --lib provider::
cargo test -p peri-acp --lib agent_pool::
cargo test -p peri-acp --test langfuse_e2e
cargo test -p peri-middlewares --lib goal::
cargo test -p peri-middlewares --lib hitl::
cargo test -p peri-middlewares --lib subagent::
```

Expected: ACP 保持配置/缓存/DTO 边界；middleware 保持 policy 与子 Agent 生命周期。

---

### Task 8: 一次性删除 `peri-agent::llm`，清理旧 API 并完成 workspace 验证

**Files:**
- Delete: `peri-agent/src/llm/` 全部实现和测试文件
- Modify: `peri-agent/src/lib.rs`
- Modify: 受编译器和最终负向搜索发现的所有 workspace 引用
- Modify: `Cargo.lock`
- Modify: 与当前 API 相关的 doc comment/README，仅更新事实源

- [x] **Step 1: 删除旧 LLM facade 和 Agent retry decorator**

移除 `pub mod llm`、旧 `BaseModel`、`LlmRequest`、`LlmResponse`、`StreamingContext`、`BaseModelReactLLM`、`RetryableLLM`、`ChatOpenAI`、`ChatAnthropic`、`MockLLM` 及其 re-export。将只服务于旧 retry 的 `AgentError::is_retryable()` 删除或缩减为上层错误显示所需语义。

- [x] **Step 2: 修复编译器报告的所有旧路径**

所有调用方直接引用 `peri_model` 或 Agent bridge；不得用 type alias/re-export 重新引入旧路径。

- [x] **Step 3: 执行旧 API 负向搜索**

Run:

```bash
rg 'peri_agent::llm|crate::llm|BaseModel|LlmRequest|LlmResponse|ChatOpenAI|ChatAnthropic|RetryableLLM|BaseModelReactLLM' \
  --glob '*.rs' --glob 'Cargo.toml' --glob '*.md' \
  --glob '!spec/archive/**' --glob '!spec/issues/**'
```

Expected: 无匹配。若历史文档保留名称，必须仅存在于 `spec/issues/` 或 `spec/archive/`。

- [x] **Step 4: 执行完整验证**

Run:

```bash
cargo metadata --no-deps --format-version 1 >/dev/null
cargo check -p peri-model
cargo test -p peri-model --lib
cargo test -p peri-model --doc
cargo test -p peri-agent --lib
cargo test -p peri-agent --doc
cargo test -p peri-acp --lib
cargo test -p peri-acp --test langfuse_e2e
cargo test -p peri-middlewares --lib
cargo test --workspace --doc
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Expected: 全部退出码为 0；无旧 API 命中、无格式错误、无 Provider 真网调用、无 secret 泄漏测试失败。

---

## 覆盖审阅

- 标准 DTO、完整工具调用建模、流式优先、强制断开、首事件前 retry、请求同源观测、内建强类型协议构造器：Tasks 1–5。
- ReAct/Agent event/compact 保留且不泄漏到协议层：Task 6。
- ACP 配置/工厂/池/Langfuse 与 middleware 调用方：Task 7。
- 一次性破坏性迁移、删除旧 facade 与全 workspace 证据：Task 8。
- 风险防线：转换 fail closed、SSE/cancel/retry contract、Langfuse 同源、安全错误与观测脱敏测试贯穿 Tasks 2–8。
