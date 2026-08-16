# peri-model 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：docs/standards/architecture-contracts.md、源码（无 crate 级 CLAUDE.md）

## 架构速览

- 定位：与 provider 无关的协议 DTO + 流式优先模型接口（lib.rs:1）；只产消标准 `peri-model` 协议，不引用 Agent 事件/类型（anthropic/mod.rs:3）
- 数据流：`ModelRequest → build_request（provider 适配）→ HttpTransport/SSE → provider decoder（ModelStreamEvent 流）→ ModelStream → 上层消费（peri-agent/src/agent/model_bridge.rs:246）`
- 模型接口：`protocol/model.rs:150` 的 `Model` trait——`stream()`（:157）是唯一调用路径；`complete()`（:171）仅聚合 stream 事件，无独立非流式路径
- 稳定不变量：`ModelStream` 持有 parent 的 child token，`abort()` 只取消本流、不反向取消父 token（model.rs:81-85）；流必须以 `Completed(ModelResponse)` 收尾，Completed 只发一次（model.rs:134-137, :222）；`ModelResponse::new` 强制 assistant message（types.rs:366/:374）；事实源关系：协议类型在 `protocol/`，adapter 配置在各自目录，`lib.rs:9-20` 统一 re-export；装配面（provider 工厂）在 `peri-acp/src/provider/mod.rs`（`LlmProvider::into_model`，OpenAi :271 / Anthropic :305 构造）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 加新 provider/模型适配 | 模板 `src/openai_compatible/`；trait 事实源 `src/protocol/model.rs`（`Model` :153）；装配面 `peri-acp/src/provider/mod.rs`（`LlmProvider::into_model`） | `impl Model for OpenAiModel`（openai_compatible/mod.rs:156）；`stream::decoders()`（:32）返回 `SseDecoderFactory` | 实现 `capabilities`/`prepare_request`/`stream` 三方法；stream 走 `runtime_http_sse_stream`（runtime/stream.rs:68）串 transport+retry；`prepare_request` 默认实现明确拒绝（model.rs:149-153）；`build_request` 须产出可脱敏观测的 `PreparedModelRequest`（observe，request.rs:153） |
| 改流式事件 | `src/protocol/model.rs` `ModelStreamEvent`（:17：TextDelta/ReasoningDelta/ToolCallDelta/Usage/Completed）+ `src/runtime/stream.rs`；消费方 `peri-agent/src/agent/model_bridge.rs:246`（generate_from_request） | 各 adapter `decode_event`（anthropic/stream.rs:66、openai_compatible/stream.rs:44）；`Model::complete` 聚合（model.rs:171） | 流必须以 Completed 收尾，否则 `StreamEndedWithoutCompleted`（model.rs:222）；Completed 后流终止（model.rs:126-133）；新增事件变体须同步改 complete 聚合 + model_bridge 消费 + ACP 映射（ARC-EVENT-001） |
| 改 TokenUsage/StopReason 语义 | 事实源 `src/protocol/types.rs`：`TokenUsage` :453（cache_creation/cache_read 可选，`new` :463）、`StopReason` :475（tagged，`Other{value}` 兜底） | 构造点：anthropic/stream.rs `current_usage`（:364）/`completed_response`（:378）；openai_compatible/response.rs 解码 | usage 溢出（input+output 超 u32）→ provider error 且不发 Completed（anthropic/mod_test.rs:604）；StopReason 语义消费方在 model_bridge（映射 agent 层 stop reason） |
| 改消息内容模型 | 事实源 `src/protocol/types.rs`：`ContentBlock` :71（Text/Image/Document/Reasoning/ToolUse/ToolResult/RedactedReasoning）、`ModelMessage` :229（role-tagged）、`ToolCall` :152、`ToolResult` :182 | 两端映射：`content_to_anthropic`（anthropic/request.rs:222）、`block_to_openai_part`（openai_compatible/request.rs:217）；`ContentBlock::text_content`（types.rs:113） | 新增 block 变体必须同时改两端映射 + `text_content`（否则判空/文本提取漏分支）；Anthropic tool_use 只序列化一次（mod_test.rs:205）；序列化顺序须确定（ARC-SERIAL-001） |
| 改传输层（HTTP/SSE） | `src/transport/http.rs`（`HttpTransport` :46、`ReqwestTransport` :55）、`src/transport/sse.rs`（`SseEvent` :5、`SseParser` :11） | 组装点 `runtime_http_sse_stream`（runtime/stream.rs:68）、`retrying_http_sse_stream_async`（:88） | transport 是 crate-private seam（transport/mod.rs:5-10），公共 API 不暴露 client/headers/原始请求；只供 crate 内 adapter 使用 |
| 改重试策略 | 事实源 `src/runtime/retry.rs`：`RetryConfig` :88（`delay_for_retry` :138）、`RetryableErrorClasses` :12、`RetryObserver` :202 | `retrying_stream`（:220）；`ModelRuntimeConfig::with_retry`（request.rs:78） | 已发出可见 delta 后传输失败 → interrupted 不重试（anthropic/mod_test.rs:928）；observer 不接收请求/响应/认证信息（request.rs:83） |
| 改观测/脱敏 | `src/runtime/request.rs`：`PreparedModelRequest` :126、`observe` :153、`ObservedProviderBody` :99；`ModelRuntimeConfig` :36（`with_full_observation` :70） | 配置显式构造，不读环境变量（request.rs:34）；`AnthropicConfig::new`（anthropic/mod.rs:45）/`OpenAiConfig::new`（openai_compatible/mod.rs:43） | 敏感键/非 ASCII 键/data URI 恒脱敏（request.rs:347）；config Debug 永不输出凭据（anthropic/mod.rs:82-97）；契约 ARC-SECRET-001 |
| 改错误类型/分类 | `src/runtime/error.rs`：`ModelError` :202、`ProtocolErrorKind` :47、`ModelError::cancelled` :234、`is_stream_interrupted` :259 | 构造点 `ModelError::protocol` :224、`http_status` :212 | error summary 走 `SafeErrorContext`（:126），不携带 secret；cancelled 与 interrupted 语义区分（重试判定依赖） |

## 子系统

### protocol/（协议 DTO + 模型接口事实源）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 内容块/消息/请求响应 | protocol/types.rs | `JsonObject` :11（BTreeMap 承载）；`ContentBlock` :71；`ModelMessage` :229；`ModelRequest` :302；`ModelResponse` :335（`new` :366 强制 assistant）；`TokenUsage` :453；`StopReason` :475；`ModelCapabilities` :484；`ProviderProtocol` :498 |
| 流式模型接口 | protocol/model.rs | `ModelStreamEvent` :17；`ModelStream` :60（`abort` :112、Drop 取消 :143）；`Model` trait :153；`complete` :171 |
| re-export 事实源 | lib.rs:9-20 + protocol/mod.rs:4-10 | 新增公共类型须在此两处登记 |

### anthropic/（Anthropic Messages API adapter）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 配置/模型/流入口 | anthropic/mod.rs | `AnthropicConfig` :31（Debug 脱敏 :82）；`AnthropicModel` :107；`impl Model` :166（stream → runtime_http_sse_stream） |
| 请求构建 | anthropic/request.rs | `build_request` :37；`messages_endpoint` :88（拒绝 userinfo/非 http(s)，保留 base path）；`messages_to_anthropic` :111 |
| prompt cache | anthropic/cache.rs | `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` :3；`split_system_blocks` :10；`apply_cache_to_messages` :55（首/末/倒数第二 user 消息） |
| 流解码 | anthropic/stream.rs | `decoders` :52；`decode_event` :66（生命周期状态机：ensure_streaming :155/start_block :163/apply_delta :226/finish_block :285）；`completed_response` :378 |
| 非流式响应 | anthropic/response.rs | `token_count` :110（溢出检查） |

### openai_compatible/（OpenAI-compatible adapter）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 配置/模型/流入口 | openai_compatible/mod.rs | `OpenAiConfig` :30；`OpenAiModel` :106；`impl Model` :156 |
| 请求构建 | openai_compatible/request.rs | `BuiltOpenAiRequest` :20；`extract_system_message` :109；`messages_to_json` :125 |
| 流解码 | openai_compatible/stream.rs | `decoders` :32；`decode_event` :44；`complete_stream` :135 |

### runtime/（传输无关运行时）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 运行时配置/观测投影 | runtime/request.rs | `ModelRuntimeConfig` :36；`PreparedModelRequest::observe` :153（redacted_paths/truncated_paths） |
| 流编排 | runtime/stream.rs | `cancellable_stream` :26；`retrying_http_sse_stream` :36；`runtime_http_sse_stream` :68；`retrying_http_sse_stream_async` :88 |
| 重试 | runtime/retry.rs | `retrying_stream` :220；`RetryObserver` :202；`RetryObservation` :159 |
| 错误模型 | runtime/error.rs | `ModelError` :202；`ProtocolErrorKind` :47；`TransportErrorKind` :7；`RetryErrorKind` :28 |

### transport/（HTTP/SSE seam，crate-private）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| HTTP 抽象 | transport/http.rs | `HttpRequest` :12；`HttpTransport` trait :46；`ReqwestTransport` :55 |
| SSE 解析 | transport/sse.rs | `SseEvent` :5；`SseParser` :11 |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-EVENT-001：`ModelStreamEvent` 由 model_bridge 消费并直发 v2 事件；改流式事件须覆盖 发射 → ACP 映射 → TUI 全链路，禁止 v1 中间态
- ARC-SERIAL-001：`JsonObject` 基于 `BTreeMap`（types.rs:11），provider payload 与 tools 序列化顺序须确定，不得依赖 `HashMap` 迭代序
- ARC-SECRET-001：api_key 只存于 config/模型内部；观测投影（`ObservedProviderBody`）与 config Debug 永不输出凭据；runtime 不读环境变量
