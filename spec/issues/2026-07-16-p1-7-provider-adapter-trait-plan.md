# P1-7 ProviderAdapter trait 改进方案 v2

> 对 issue `2026-07-16-p1-7-provider-adapter-trait.md` 的改进设计
>
> **v2 修订**：经 2 位 verification agent 对抗验证，修复了 v1 的 7 个 FAIL 项

## 验证总结

v1 方案经架构验证师和实施可行性验证师双人对抗审查，共发现 **7 个必须修复的硬伤**：

| # | 问题 | 根因 |
|---|------|------|
| 1 | `serialize_messages` 丢失 `SystemPromptBlock` cache 元数据 | trait 方法粒度过细，Anthropic 特定类型不应进入通用 trait 签名 |
| 2 | `extract_request_id` 无法访问 HTTP headers（Anthropic `x-request-id`） | 方法签名只接收 `&Value`，丢掉了 headers 信息 |
| 3 | `GenericInvoker` 持有 adapter 导致 `ChatAnthropic` 双重实例 | 静态分发用 owned 值，builder 模式同步成本高 |
| 4 | `extract_error_message` 未定义却调用 | 方案伪代码遗漏 |
| 5 | `stream.rs` 无法真正"不动"（import 路径断裂） | `build_request_body` 移入 adapter 后 streaming 引用失效 |
| 6 | Provider 特有日志字段丢失（cache 指标、500 请求体 dump） | GenericInvoker 统一日志太薄 |
| 7 | `parse_response_content` 与 `stop_reason` 调用顺序依赖 | OpenAI 的 `parse_assistant_message` 以 `&StopReason` 为分支参数 |

**v2 方案全部修复。**

---

## 0. 问题诊断

`peri-agent/src/llm/anthropic/invoke.rs`(~632 行) 和 `peri-agent/src/llm/openai/invoke.rs`(~587 行) 共享约 70% 的结构：

| 共享逻辑 | Anthropic 位置 | OpenAI 位置 |
|----------|---------------|-------------|
| ContentBlock → Provider JSON 序列化 | `block_to_anthropic` (L16-77) | `block_to_openai_part` (L18-48) |
| MessageContent 序列化 | `content_to_anthropic` (L79-88) | `content_to_openai` (L50-89) |
| BaseMessage 列表 → Provider 消息格式 | `messages_to_anthropic` (L98-227) | `messages_to_json` (L105-186) |
| System prompt 提取与缓存 | 同上函数内 | 同上函数内 |
| ToolDefinition 序列化 | `build_request_body` 内 L302-312 | `build_request_body` 内 L348-361 |
| HTTP 请求发送/响应读取/错误处理 | `invoke()` + `handle_anthropic_response` | `invoke()` 内 |
| 响应 JSON → ContentBlock+ToolCallRequest | `parse_content_blocks` (L231-272) | `parse_assistant_message` (L188-301) |
| TokenUsage 提取 | `handle_anthropic_response` 内 L518-544 | `extract_openai_usage` (L429-448) |
| BaseModel trait impl 样板代码 | L553-631 | L450-587 |
| Streaming SSE 入口 URL/auth 构建 | `do_invoke_streaming` | `do_invoke_streaming` |

---

## 1. 设计原则 (v2 强化)

1. **只抽象真正共享的东西**：不强行统一 streaming（SSE 格式差异太大）
2. **零破坏性**：`BaseModel` trait 和 `ReactLLM` trait 保持不动，`BaseModelReactLLM` / `RetryableLLM` 零改动
3. **GenericInvoker 为无状态辅助器**：`invoke(adapter: &A, client: &Client, request)`，不持有状态
4. **System prompt 提取不进 trait**：由各 adapter 的 `build_request_body` 内部自行处理
5. **Streaming 路径复用 adapter 的 URL/auth/tool 序列化方法**：消除 invoke/stream 的 URL/auth 重复
6. **新增 Provider 只需实现一个 trait**：未来加 Google Gemini、Cohere 等只需实现 `ProviderAdapter`

---

## 2. 方案 v2：ProviderAdapter trait + GenericInvoker 辅助器

### 2.1 `ProviderAdapter` trait — 修订版

```rust
/// ProviderAdapter — 封装 Provider 特定的数据转换差异。
///
/// 职责边界：ContentBlock ↔ Provider JSON 的双向转换 + HTTP 传输配置。
/// 不负责 System 消息的提取——该逻辑留在 build_request_body 中由各 adapter 自行处理。
///
/// [v2] GenericInvoker 是瞬态辅助器，不持有 adapter 引用。
/// ChatAnthropic/ChatOpenAI 直接持有 adapter + client。
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    // ─── 标识 ───
    fn provider_name(&self) -> &str;
    fn model_id(&self) -> &str;
    fn context_window(&self) -> u32 { 200_000 }

    // ─── 1. 消息序列化（System 不进此层，由 build_request_body 自行处理） ───
    /// 将 BaseMessage 列表（不含 System 消息）序列化为 Provider 格式的 JSON 数组。
    /// System 消息的提取和放置由 build_request_body 处理——不同 Provider 差异太大。
    /// [v2] 不再返回 SystemPromptBlock，该类型不进入通用 trait 签名。
    fn serialize_messages(&self, messages: &[BaseMessage]) -> Vec<Value>;

    /// 将单个 ContentBlock 序列化为 Provider 格式的 JSON fragment。
    /// 用于 streaming 路径中工具结果回放等场景。
    fn serialize_content_block(&self, block: &ContentBlock) -> Option<Value>;

    // ─── 2. 请求体构建 ───
    /// 将 ToolDefinition 序列化为 Provider 特定的 tool JSON。
    fn serialize_tool(&self, tool: &ToolDefinition) -> Value;

    /// 构建完整的 HTTP 请求体。
    /// 内部自行处理：System 消息提取+放置、thinking 配置、cache 标记等。
    fn build_request_body(&self, request: &LlmRequest, streaming: bool) -> Value;

    // ─── 3. 响应解析 ───
    /// 从响应 JSON 无条件解析所有 ContentBlock + ToolCallRequest。
    /// [v2] 必须不依赖 stop_reason 做分支——stop_reason 由 GenericInvoker 单独提取后决定使用方式。
    fn parse_response_content(&self, response_json: &Value) -> (Vec<ContentBlock>, Vec<ToolCallRequest>);

    /// 从响应 JSON 提取 StopReason。
    fn extract_stop_reason(&self, response_json: &Value) -> StopReason;

    /// 从响应 JSON 提取 TokenUsage。
    fn extract_usage(&self, response_json: &Value, request_id: Option<String>) -> Option<TokenUsage>;

    /// [v2 新增] 从错误响应 JSON 提取人类可读错误消息。
    fn extract_error_message(&self, response_json: &Value) -> String;

    /// [v2 新增] 从 HTTP response headers 优先提取 request_id，
    /// 由 GenericInvoker 先调用，若返回 None 则 fallback 到 extract_request_id_from_body。
    fn extract_request_id_from_headers(&self, headers: &reqwest::header::HeaderMap) -> Option<String>;

    /// [v2 新增] 从响应 JSON body 提取 request_id（作为 headers fallback）。
    fn extract_request_id_from_body(&self, response_json: &Value) -> Option<String>;

    // ─── 4. 日志 ───
    /// [v2 新增] 在 GenericInvoker 的成功日志之后，输出 Provider 特有字段（如 cache 指标）。
    /// 默认空实现——OpenAI 不需要额外日志。
    fn log_success_extra(&self, response_json: &Value, elapsed_ms: u64, msg_count: usize) {
        let _ = (response_json, elapsed_ms, msg_count);
    }

    /// [v2 新增] 在 GenericInvoker 的错误处理中，输出 Provider 特有错误上下文。
    /// Anthropic 覆盖此方法以在 500 错误时 dump 请求体。
    /// body: 请求体引用（用于 dump），仅在非流式路径可用。
    fn log_error_extra(&self, status: u16, response_json: &Value, body: Option<&Value>) {
        let _ = (status, response_json, body);
    }

    // ─── 5. HTTP 传输配置 ───
    /// 构建 API endpoint URL。
    fn build_chat_url(&self) -> String;

    /// 在 reqwest 请求上添加 Provider 特定的 headers（auth、version、beta、session-id 等）。
    fn apply_auth_headers(
        &self,
        req: reqwest::RequestBuilder,
        session_id: Option<&str>,
    ) -> reqwest::RequestBuilder;
}
```

### 2.2 `GenericInvoker` — 无状态辅助器 (v2 修订)

```rust
/// GenericInvoker — 无状态的共享非流式 HTTP 调用辅助器。
///
/// [v2] 设计变更：不再持有 adapter/client。invoke() 以借用方式接收 &A 和 &Client。
/// ChatAnthropic/ChatOpenAI 直接持有自己的 adapter + client，无双重实例问题。
pub struct GenericInvoker;

impl GenericInvoker {
    /// 非流式 invoke 的完整模板方法。
    ///
    /// 流程：
    /// 1. build_request_body → 2. apply_auth_headers + send → 3. 状态检查 + 错误提取 →
    /// 4. parse_response_content → 5. 成功日志 → 6. build_base_message → 7. 返回 LlmResponse
    pub async fn invoke<A: ProviderAdapter>(
        adapter: &A,
        client: &reqwest::Client,
        request: LlmRequest,
    ) -> AgentResult<LlmResponse> {
        let msg_count = request.messages.len();
        let start = std::time::Instant::now();

        // 1. 构建请求体
        let body = adapter.build_request_body(&request, false);

        // 2. 发送 HTTP 请求
        let session_id = request.session_id.as_deref();
        let req = adapter.apply_auth_headers(client.post(&adapter.build_chat_url()), session_id);
        let resp = req
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(
                    provider = adapter.provider_name(),
                    model = adapter.model_id(),
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    error = %e,
                    "LLM 网络请求失败"
                );
                AgentError::LlmError(e.to_string())
            })?;

        let status = resp.status();
        // [v2] 在消费 resp body 前保存 headers
        let resp_headers = resp.headers().clone();

        let resp_text = resp.text().await.map_err(|e| {
            tracing::error!(
                provider = adapter.provider_name(),
                model = adapter.model_id(),
                status = %status,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %e,
                "LLM 读取响应体失败"
            );
            AgentError::LlmError(format!("读取响应体失败: {e}"))
        })?;
        let resp_json: Value = serde_json::from_str(&resp_text).map_err(|e| {
            tracing::error!(
                provider = adapter.provider_name(),
                model = adapter.model_id(),
                status = %status,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %e,
                "LLM 响应解析失败"
            );
            AgentError::LlmError(format!(
                "解析响应失败: {e}\n原始响应({status}): {resp_text}"
            ))
        })?;

        // 3. 非成功状态 → 错误处理
        if !status.is_success() {
            let error_msg = adapter.extract_error_message(&resp_json);
            adapter.log_error_extra(status.as_u16(), &resp_json, Some(&body));

            tracing::error!(
                provider = adapter.provider_name(),
                model = adapter.model_id(),
                status = %status,
                error_message = %error_msg,
                elapsed_ms = start.elapsed().as_millis() as u64,
                msg_count,
                "LLM API 错误"
            );
            return Err(AgentError::LlmHttpError {
                status: status.as_u16(),
                message: format!("API 错误 {status}: {error_msg}"),
            });
        }

        // 4. 解析成功响应
        let stop_reason = adapter.extract_stop_reason(&resp_json);

        // [v2] 先提取 request_id（headers 优先 + body fallback）
        let request_id = adapter
            .extract_request_id_from_headers(&resp_headers)
            .or_else(|| adapter.extract_request_id_from_body(&resp_json));

        let (blocks, tool_calls) = adapter.parse_response_content(&resp_json);
        let usage = adapter.extract_usage(&resp_json, request_id.clone());

        // 5. 通用成功日志
        tracing::info!(
            provider = adapter.provider_name(),
            model = adapter.model_id(),
            status = %status,
            elapsed_ms = start.elapsed().as_millis() as u64,
            msg_count,
            input_tokens = usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
            output_tokens = usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
            "LLM invoke completed"
        );
        // [v2] Provider 特有日志字段（如 Anthropic cache 指标）
        adapter.log_success_extra(&resp_json, start.elapsed().as_millis() as u64, msg_count);

        // 6. 构造 BaseMessage（两个 Provider 完全相同的逻辑）
        let message = Self::build_base_message(blocks, tool_calls, &stop_reason);

        Ok(LlmResponse { message, stop_reason, usage, request_id })
    }

    /// (blocks, tool_calls) → BaseMessage（跨 Provider 共享逻辑）
    fn build_base_message(
        blocks: Vec<ContentBlock>,
        tool_calls: Vec<ToolCallRequest>,
        stop_reason: &StopReason,
    ) -> BaseMessage {
        // 移自当前两个 invoke.rs 中重复的 (blocks, tool_calls) → BaseMessage 分支逻辑
        // (~20 行，无 Provider 差异)
        todo!("从 handle_anthropic_response L495-516 和 openai invoke L544 提取")
    }
}
```

### 2.3 Streaming 路径的变化 (v2 修订)

**Streaming 文件需要改动**，但不涉及 SSE 事件循环逻辑——仅改变 URL/auth 构建和部分序列化调用的来源。

**变化点**：

| 文件 | 当前代码 | v2 改为 |
|------|---------|---------|
| `anthropic/stream.rs:27` | `build_request_body(adapter, &request, true)` | `adapter.build_request_body(&request, true)` |
| `anthropic/stream.rs:29-47` | 内联 chat_url 构建 + auth headers | `adapter.build_chat_url()` + `adapter.apply_auth_headers(req, session_id)` |
| `anthropic/stream.rs:265` | `parse_content_blocks(&accumulated_blocks)` | `adapter.parse_response_content(&resp_json).0` |
| `openai/stream.rs:36` | `build_request_body(adapter, &request, true)` | `adapter.build_request_body(&request, true)` |
| `openai/stream.rs:38-41` | 内联 chat_url 构建 | `adapter.build_chat_url()` |
| `openai/stream.rs:219` | `extract_openai_usage(u, ...)` | `adapter.extract_usage(u, ...)` |

**streaming 函数签名变化**：`do_invoke_streaming` 第一个参数从 `&ChatAnthropic` / `&ChatOpenAI` 改为 `&A`（或通过 `ChatAnthropic` 上的方法间接委托）。

**不变的部分**：SSE 事件循环（`content_block_start/delta/stop`、`choices[0].delta` 等）、tool_call 参数累积器、增量事件 emit——这些保持 Provider 特有，不动。

### 2.4 最终类结构 (v2)

```rust
// ChatAnthropic — 持有自己的 adapter + client
pub struct ChatAnthropic {
    adapter: AnthropicAdapter,
    client: reqwest::Client,
}

impl ChatAnthropic {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self { ... }
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.adapter.base_url = Some(url.into());  // 直接操作 adapter 字段
        self
    }
    // ... builder 方法都在 ChatAnthropic 上，直接修改 self.adapter 字段
}

impl BaseModel for ChatAnthropic {
    async fn invoke(&self, request: LlmRequest) -> AgentResult<LlmResponse> {
        GenericInvoker::invoke(&self.adapter, &self.client, request).await
    }
    async fn invoke_streaming(&self, request: LlmRequest, ctx: StreamingContext) -> ... {
        stream::do_invoke_streaming(&self.adapter, &self.client, request, ctx).await
    }
    fn provider_name(&self) -> &str { self.adapter.provider_name() }
    fn model_id(&self) -> &str { self.adapter.model_id() }
    fn context_window(&self) -> u32 { self.adapter.context_window() }
    fn build_request_body(&self, request: &LlmRequest) -> Option<Value> {
        Some(self.adapter.build_request_body(request, false))
    }
}

// AnthropicAdapter — 所有 Provider 特定逻辑
struct AnthropicAdapter {
    api_key: String,
    model: String,
    base_url: Option<String>,
    extended_thinking: bool,
    thinking_budget: u32,
    thinking_effort: String,
    enable_cache: bool,
    max_tokens: u32,
}

impl ProviderAdapter for AnthropicAdapter { /* ... */ }
```

`ChatOpenAI` + `OpenAiAdapter` 完全对称。

---

## 3. 与 issue 原方案的差异

| 方面 | issue 原方案 | 本方案 v2 |
|------|-------------|-----------|
| Streaming 抽象 | `parse_chunk` 试图统一 | streaming 保持 Provider 特有，不纳入 trait |
| `BaseModelReactLLM` | 改为泛型 `<A: ProviderAdapter>` | 不动——桥接 `BaseModel` → `ReactLLM`，本改动透明 |
| GenericInvoker 状态 | 持有 `adapter: A`（owned） | 无状态辅助器，`invoke(&A, &Client, request)` |
| System 消息处理 | `serialize_messages` 负责 System 提取 | `build_request_body` 自行处理，不进 trait |
| request_id 提取 | 仅从 body | headers 优先 + body fallback |
| 错误日志 | 统一格式，无 Provider 特有字段 | 通过 `log_success_extra` / `log_error_extra` 可扩展 |
| Streaming 路径 | 标记为"不动" | 明确需要改动 URL/auth 构建（约 20 行/文件），SSE 循环不变 |

---

## 4. 文件变更清单 (v2 修订)

| 操作 | 文件 | 说明 | 预估行数 |
|------|------|------|---------|
| **新增** | `peri-agent/src/llm/provider_adapter.rs` | `ProviderAdapter` trait + `GenericInvoker` | ~150 |
| **新增** | `peri-agent/src/llm/anthropic/adapter.rs` | `AnthropicAdapter` struct + `ProviderAdapter` impl | ~350 |
| **新增** | `peri-agent/src/llm/openai/adapter.rs` | `OpenAiAdapter` struct + `ProviderAdapter` impl | ~320 |
| **重构** | `peri-agent/src/llm/anthropic/mod.rs` | `ChatAnthropic` 持有 `AnthropicAdapter` 而非裸字段 | ~60（减半） |
| **重构** | `peri-agent/src/llm/anthropic/invoke.rs` | 缩减为 `BaseModel` impl（~40 行委托） | -450 |
| **重构** | `peri-agent/src/llm/openai/mod.rs` | `ChatOpenAI` 持有 `OpenAiAdapter` | ~60（减半） |
| **重构** | `peri-agent/src/llm/openai/invoke.rs` | 缩减为 `BaseModel` impl（~40 行委托） | -420 |
| **修改** | `peri-agent/src/llm/anthropic/stream.rs` | URL/auth 改用 adapter 方法，parse_content_blocks 改用 adapter | ~30 |
| **修改** | `peri-agent/src/llm/openai/stream.rs` | URL/auth 改用 adapter 方法，extract_usage 改用 adapter | ~20 |
| **不动** | `peri-agent/src/llm/anthropic/cache.rs` | 零改动——被 AnthropicAdapter::build_request_body 调用 | 0 |
| **不动** | `peri-agent/src/llm/react_adapter.rs` | 零改动 | 0 |
| **不动** | `peri-agent/src/llm/retry.rs` | 零改动 | 0 |
| **不动** | `peri-agent/src/llm/types.rs` | 零改动 | 0 |
| **不动** | `peri-agent/src/llm/adapter.rs` (MockLLM) | 零改动 | 0 |
| **不动** | `peri-agent/src/llm/sse/` | 零改动 | 0 |
| **更新** | `peri-agent/src/llm/mod.rs` | 新增 `provider_adapter` 模块导出 | 1 |
| **不动** | `peri-acp/src/agent/builder.rs` | 零改动 | 0 |
| **不动** | `peri-acp/src/provider/mod.rs` | 零改动 | 0 |
| **更新** | `peri-agent/src/llm/anthropic_test.rs` | ~18 个测试改 import/调用路径 | ~80 |
| **更新** | `peri-agent/src/llm/openai_test.rs` | ~20 个测试改 import/调用路径 | ~60 |
| **不动** | `peri-agent/src/llm/retry_test.rs` | 零改动 | 0 |
| **不动** | `peri-agent/src/llm/react_adapter_test.rs` | 零改动 | 0 |
| **更新** | `docs/design/peri-agent-llm-adapter-v2.md` | 反映 v2 架构 | ~40 |

**总行数变化**：净增约 +200 行（新增 trait + adapter 约 820 行，删除 invoke.rs 重复代码约 870 行，修改约 200 行）

---

## 5. 实施步骤

### Step 1: 定义 trait（不改现有代码）✅
1. 创建 `peri-agent/src/llm/provider_adapter.rs`
2. 定义 `ProviderAdapter` trait（含所有方法签名，含 v2 新增的 `extract_error_message`、`extract_request_id_from_headers`、`extract_request_id_from_body`、`log_success_extra`、`log_error_extra`）
3. 实现 `GenericInvoker::invoke()` 模板方法 + `build_base_message()` 静态方法
4. `cargo build -p peri-agent` 验证编译

### Step 2: 实现 `AnthropicAdapter` ✅
1. 创建 `peri-agent/src/llm/anthropic/adapter.rs`
2. 从 `invoke.rs` 提取函数为 `ProviderAdapter` 方法：
   - `block_to_anthropic` → `serialize_content_block`
   - `messages_to_anthropic` → `serialize_messages`（仅处理非 System 消息）
   - `build_request_body` → `build_request_body`（保留完整逻辑，含 System 提取 + cache 处理）
   - `parse_content_blocks` → `parse_response_content`
   - `handle_anthropic_response` 内部逻辑 → `extract_stop_reason` + `extract_usage` + `extract_request_id_from_body` + `extract_error_message`
   - 新增 `extract_request_id_from_headers`（从 `x-request-id` header 提取）
   - 新增 `log_success_extra`（cache 指标日志）
   - 新增 `log_error_extra`（500 错误时 dump 请求体）
   - `build_chat_url` + `apply_auth_headers`
3. `cargo build -p peri-agent` 验证编译

### Step 3: 实现 `OpenAiAdapter` ✅
按 Step 2 对称处理。**特别注意**：`parse_response_content` 必须不依赖 `stop_reason` 做分支——无条件解析 tool_calls，即使是 `role: "assistant"` 无 `tool_calls` 的消息也返回空数组。

### Step 4: 重构 `ChatAnthropic`/`ChatOpenAI` + streaming ✅
1. `ChatAnthropic` 改为持有 `AnthropicAdapter` + `client`
2. `BaseModel::invoke()` → `GenericInvoker::invoke(&self.adapter, &self.client, request).await`
3. 删除 `invoke.rs` 中的响应处理代码
4. Streaming 路径改为通过 adapter 构建 URL/auth/body
5. `cargo build -p peri-agent` 验证编译

### Step 5: 测试迁移 ✅
1. `anthropic_test.rs`：约 18 个测试改 import/调用路径
2. `openai_test.rs`：约 20 个测试改 import/调用路径
3. 为 `AnthropicAdapter` / `OpenAiAdapter` 新增独立测试文件
4. `cargo test -p peri-agent --lib` 全部通过

### Step 6: 全面验证 ✅
1. `cargo test -p peri-agent --lib -- anthropic` / `openai`
2. `cargo build --workspace`
3. `lefthook run pre-commit`

---

## 6. 不可改动的约束

- **BaseModel trait**: 公开 API，签名完全不变
- **ReactLLM trait**: 完全不碰
- **builder.rs (peri-acp)**: 零改动——`ChatAnthropic` 仍 impl `BaseModel`
- **provider/mod.rs (peri-acp)**: 零改动
- **prompt cache 前缀稳定性**: `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` / `split_system_blocks` / `ensure_thinking_blocks` 逻辑完全不动
- **reasoning block 处理**: `redacted_thinking` 回传 + `reasoning_content` 回传逻辑不动
- **Langfuse**: `build_request_body` 调用路径不变
- **retry.rs**: 完全不碰

---

## 7. 风险评估 (v2 更新)

| 风险 | 等级 | 缓解 |
|------|------|------|
| LLM 调用为核心热路径 | 中 | GenericInvoker 通过 `fn invoke<A>` 静态分发，零 trait object 开销 |
| 大量代码移动可能引入回归 | 中 | 函数逐行迁移（不改逻辑），改一行测一行 |
| 测试迁移量大 | 中 | ~38 个测试改 import，其余不变；adapter 新增独立测试 |
| Streaming 改动范围 | 低 | 每 Provider streaming 仅改 URL/auth 构建 (~20 行)，SSE 循环不动 |
| `SystemPromptBlock` 类型消失 | 无 | 它只存在于 `AnthropicAdapter::build_request_body` 内部（private），不进 trait |
| 无状态 GenericInvoker 增加参数 | 无 | 编译时 monomorphization，无运行时开销 |
