# peri-model 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-19（新增 openai_responses adapter 与 ResponsesNativeHistory 载体）
> 依据：docs/standards/architecture-contracts.md、源码（无 crate 级 CLAUDE.md）

## 架构速览

- 定位：与 provider 无关的协议 DTO + 流式优先模型接口（lib.rs:1）；只产消标准 `peri-model` 协议，不引用 Agent 事件/类型（anthropic/mod.rs:3）
- 数据流：`ModelRequest → build_request（provider 适配）→ HttpTransport/SSE → provider decoder（ModelStreamEvent 流）→ ModelStream → 上层消费（peri-agent/src/agent/model_bridge.rs:246）`
- Responses 数据流（原生保真）：`response.completed.response.output → ResponsesHistoryV1（来源身份 = 实际 {base}/responses + model）→ ContentBlock::ResponsesNativeHistory → 下一轮 project_input_items（同域回放/跨域降级）`；三个 adapter 的安全 metadata `responses_history_degraded` 由 `PreparedModelRequest::history_diagnostics` 生成，Bridge 在请求边界转为一次系统 warning，不进入 provider body。Anthropic 将无签名可见 reasoning 摘要映射为 text，跨协议历史不补造空签名 thinking。
- 模型接口：`protocol/model.rs:153` 的 `Model` trait——`stream()`（:163）是唯一调用路径；`complete()`（:170）仅聚合 stream 事件，无独立非流式路径
- 稳定不变量：`ModelStream` 持有 parent 的 child token，`abort()` 只取消本流、不反向取消父 token（model.rs:81-85）；正常 provider decoder 应以单个 `Completed(ModelResponse)` 收尾，`complete()` 在首个 `Completed` 返回，EOF 未见 Completed 则报错（model.rs:212-222）；`ModelResponse::new` 强制 assistant message（types.rs:366/:374）；事实源关系：协议类型在 `protocol/`，adapter 配置在各自目录，`lib.rs:9-20` 统一 re-export；装配面（provider 工厂）在 `peri-acp/src/provider/mod.rs`（`LlmProvider::into_model`，OpenAi :271 / Anthropic :305 构造）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 加新 provider/模型适配 | 模板 `src/openai_compatible/`；trait 事实源 `src/protocol/model.rs`（`Model` :153）；装配面 `peri-acp/src/provider/mod.rs`（`LlmProvider::into_model`） | `impl Model for OpenAiModel`（openai_compatible/mod.rs:156）；`stream::decoders()`（:32）返回 `SseDecoderFactory` | 实现 `capabilities`/`prepare_request`/`stream` 三方法；stream 走 `runtime_http_sse_stream`（runtime/stream.rs:57）串 transport+retry；`prepare_request` 默认实现明确拒绝（model.rs:149-153）；`build_request` 须产出可脱敏观测的 `PreparedModelRequest`（observe，request.rs:153） |
| 改流式事件 | `src/protocol/model.rs` `ModelStreamEvent`（:17：TextDelta/ReasoningDelta/ToolCallDelta/Usage/Completed）+ `src/runtime/stream.rs`；消费方 `peri-agent/src/agent/model_bridge.rs:246`（generate_from_request） | 各 adapter `decode_event`（anthropic/stream.rs:66、openai_compatible/stream.rs:44）；`Model::complete` 聚合（model.rs:171） | 流必须以 Completed 收尾，否则 `StreamEndedWithoutCompleted`（model.rs:222）；生产 retry driver 在 Completed 后终止发射（runtime/retry.rs:311）；新增事件变体须同步改 complete 聚合 + model_bridge 消费 + ACP 映射（ARC-EVENT-001） |
| 改 TokenUsage/StopReason 语义 | 事实源 `src/protocol/types.rs`：`TokenUsage` :453（cache_creation/cache_read 可选，`new` :463）、`StopReason` :475（tagged，`Other{value}` 兜底） | 构造点：anthropic/stream.rs `current_usage`（:364）/`completed_response`（:378）；openai_compatible/response.rs 解码 | Anthropic start/delta/stop 的 input/cache 按字段最新值更新（缺失保留、显式零覆盖），完整 input 含缓存；usage 溢出（完整 input 超 u32）→ provider error 且不发 Completed（anthropic/mod_test.rs:604）；StopReason 语义消费方在 model_bridge（映射 agent 层 stop reason） |
| 改消息内容模型 | 事实源 `src/protocol/types.rs`：`ContentBlock` :72（Text/Image/Document/Reasoning/ToolUse/ToolResult/RedactedReasoning/ResponsesNativeHistory）、`ModelMessage` :241（role-tagged）、`ToolCall` :152、`ToolResult` :182 | 两端映射：`content_to_anthropic`（anthropic/request.rs:222）、`block_to_openai_part`（openai_compatible/request.rs:217）；`ContentBlock::text_content`（types.rs:122） | 新增 block 变体必须同时改两端映射 + `text_content`（否则判空/文本提取漏分支）；Anthropic tool_use 只序列化一次（mod_test.rs:205）；序列化顺序须确定（ARC-SERIAL-001）；跨协议私有 block（ResponsesNativeHistory）在两个旧 adapter 里显式丢弃，绝不翻译成文本或回放密文 |
| 改 Responses 原生历史回放 | 载体 `ContentBlock::ResponsesNativeHistory`（types.rs:105）→ `protocol/responses_history.rs`；消费点 `replay_native_history`（openai_responses/request.rs:170） | `project_input_items(endpoint, model)`（responses_history.rs:380）、`project_generic_input_items`（:394） | 同 endpoint/model 域逐项回放（含 reasoning 密文）；`SourceMismatch` 降级为通用 message/function_call，密文绝不出域；衍生文本/推理/工具调用不再重复投影 |
| 改调用用量语义 | `src/protocol/types.rs`（`TokenUsage::accumulate`）、`src/protocol/model.rs`（`Usage` 快照 / `AttemptUsage` 累加、`complete` 聚合）、`src/runtime/error.rs`（`ModelError::usage`/`with_usage`） | 三个 adapter 的 usage 解码；`ModelError` 的 usage 为观测事实，不参与错误身份比较、不进诊断投影与 wire | `None` = provider 未上报（整体缺失），`Some` 保留显式零；重试失败尝试与最终尝试各计一次；Responses 非成功终态（failed/incomplete）携 usage 时随错误保留，`real_http_failed_terminal_usage_survives_retry` / `real_http_incomplete_terminal_reports_usage_without_retry` 覆盖 |
| 改传输层（HTTP/SSE） | `src/transport/http.rs`（`HttpTransport` :46、`ReqwestTransport` :55）、`src/transport/sse.rs`（`SseEvent` :5、`SseParser` :11） | 组装点 `runtime_http_sse_stream`（runtime/stream.rs:57）→ `retrying_http_sse_stream`（:25）→ `response_to_sse_stream`（:76） | transport 是 crate-private seam（transport/mod.rs:5-10）；两个 provider 共享同步 `SseDecoderFactory`，每次 retry 新建 decoder 状态；SSE DONE 调用 completion decoder，EOF 不等于成功；公共 API 不暴露 client/headers/原始请求 |
| 改重试策略 | 事实源 `src/runtime/retry.rs`：`RetryConfig` :88（`delay_for_retry` :138）、`RetryableErrorClasses` :12、`RetryObserver` :202 | `retrying_stream`（:220）；`ModelRuntimeConfig::with_retry`（request.rs:78） | 已发出可见 delta 后传输失败 → interrupted 不重试（anthropic/mod_test.rs:928）；observer 不接收请求/响应/认证信息（request.rs:83）；失败尝试的真实 usage 在重试前以 `ModelStreamEvent::AttemptUsage` 保留一次（终态失败/耗尽随 `ModelError::usage` 上报），首 delta 门禁不变、不抑制应有重试 |
| 改观测/脱敏 | `src/runtime/request.rs`：`PreparedModelRequest` :126、`observe` :153、`ObservedProviderBody` :99；`ModelRuntimeConfig` :36（`with_full_observation` :70） | 配置显式构造，不读环境变量（request.rs:34）；`AnthropicConfig::new`（anthropic/mod.rs:45）/`OpenAiConfig::new`（openai_compatible/mod.rs:43） | 敏感键/非 ASCII 键/data URI 恒脱敏（request.rs:347）；config Debug 永不输出凭据（anthropic/mod.rs:82-97）；契约 ARC-SECRET-001 |
| 改错误类型/分类 | `src/runtime/error.rs`：`ModelError` :202、`ProtocolErrorKind` :47、`ModelError::cancelled` :234、`is_stream_interrupted` :259 | 构造点 `ModelError::protocol` :224、`http_status` :212 | error summary 走 `SafeErrorContext`（:126），不携带 secret；cancelled 与 interrupted 语义区分（重试判定依赖） |
| 改安全模型诊断 / 重试耗尽事实 | `src/runtime/error.rs` + `src/runtime/retry.rs` | `ModelErrorDiagnostic`；`ModelError::diagnostic`；`RetryObservation::diagnostic`；`retry_exhausted_with_context` | 仅保留 category/status/受限 provider/request-id/transport/protocol kind/retry attempts；provider body、headers、URL、prompt、凭据与自由文本 summary 不跨边界；无信息时为 None，serde 枚举使用 snake_case |

## 子系统

### protocol/（协议 DTO + 模型接口事实源）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 内容块/消息/请求响应 | protocol/types.rs | `JsonObject` :11（BTreeMap 承载）；`ContentBlock` :72；`ModelMessage` :241；`ModelRequest` :314；`ModelResponse` :347（`new` :378 强制 assistant）；`TokenUsage` :465；`StopReason` :487；`ModelCapabilities` :496；`ProviderProtocol` :510（含 `OpenAiResponses`） |
| 流式模型接口 | protocol/model.rs | `ModelStreamEvent` :17；`ModelStream` :60（`abort` :111、Drop 取消 :142）；`Model` trait :153；`complete` :171 |
| re-export 事实源 | lib.rs:11-26 + protocol/mod.rs:4-10 | 新增公共类型须在此两处登记（`OpenAiResponsesConfig`/`OpenAiResponsesModel` 在 lib.rs:13） |

### anthropic/（Anthropic Messages API adapter）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 配置/模型/流入口 | anthropic/mod.rs | `AnthropicConfig` :31（Debug 脱敏 :82）；`AnthropicModel` :107；`impl Model` :166（stream → runtime_http_sse_stream） |
| 请求构建 | anthropic/request.rs | `build_request` :37；`messages_endpoint` :88（拒绝 userinfo/非 http(s)，保留 base path）；`messages_to_anthropic` :111 |
| prompt cache | `prompt_cache.rs` + `anthropic/cache.rs`；consumer `anthropic/request.rs` / `openai_compatible/request.rs` | `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` / `combine_system_prompt_with_dynamic` / `strip_system_prompt_dynamic_boundaries` 是共享 transport contract；combine 对空/None frozen base 也保留 request-time dynamic seam；`split_system_blocks` 对单 token 分区、重复 token fail-closed、无 token legacy fallback；`apply_cache_to_messages` 标记首/末/倒数第二 user 的最后一个非空 text 或 tool_result |
| 流解码 | anthropic/stream.rs | `decoders` :52；`decode_event` :66（生命周期状态机：ensure_streaming :155/start_block :163/apply_delta :226/finish_block :285）；`completed_response` :378 |
| 非流式响应 | anthropic/response.rs | `token_count` :110（溢出检查） |

### openai_compatible/（OpenAI-compatible adapter）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 配置/模型/流入口 | openai_compatible/mod.rs | `OpenAiConfig` :30；`OpenAiModel` :106；`impl Model` :156 |
| 请求构建 | openai_compatible/request.rs | `BuiltOpenAiRequest` :20；`extract_system_message` :109；`messages_to_json` :125 |
| 流解码 | openai_compatible/stream.rs | `decoders` :32；`decode_event` :44；`complete_stream` :135 |

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 配置/模型/流入口 | openai_responses/mod.rs | `OpenAiResponsesConfig` :34（Debug 脱敏 :76）；`OpenAiResponsesModel` :97；`impl Model` :147（stream → runtime_http_sse_stream） |
| 请求构建 | openai_responses/request.rs | `BuiltResponsesRequest` :18；`build_request` :40（store:false + include reasoning.encrypted_content，system → `instructions`）；`responses_endpoint` :80（`{base}/responses`，拒绝 userinfo）；`strict:false` 保持项目 schema 语义 |
| 流解码 | openai_responses/stream.rs | `decoders` 携带实际 endpoint/model；`decode_event` 按 payload type 分派。`terminal_failure` 仅将 `server_error`/`rate_limit_exceeded` 归为可重试 Provider，永久/未知码归 Other；`incomplete_failure` 仅保留 `incomplete.output_limit` / `incomplete.content_filter`，未知为 `incomplete`，均不重试、不产 Completed。`complete_stream` 未见成功终态时报错；`mod_test.rs` 使用默认策略的本地 HTTP 回归证明永久一次、暂态恢复 |
| 终态解码 | openai_responses/response.rs | `decode_completed_response` :16（`status == completed` 门禁、output item 白名单、构造 `ResponsesHistoryV1`、派生文本/推理/工具调用、stop_reason）；`decode_usage` :83（缺失 vs 零、cache 明细不并入 input） |

### runtime/（传输无关运行时）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 运行时配置/观测投影 | runtime/request.rs | `ModelRuntimeConfig` :36；`PreparedModelRequest::observe` :153（redacted_paths/truncated_paths） |
| 流编排 | runtime/stream.rs | `SseDecoderFactory`（:20）；`retrying_http_sse_stream`（:25）；`runtime_http_sse_stream`（:57）；`response_to_sse_stream`（:76）；HTTP、parser、同步 provider decoder 与 retry 只有一条实现路径 |
| 取消 / 首字节 / SSE 终态回归 | runtime/stream_test.rs + openai_compatible/mod_test.rs | runtime 测试经 HttpResponse → 生产 SSE reader 验证首字节未成帧时取消、已解码 delta 后 abort 与 connect/body/backoff/drop；OpenAiModel::stream 回归验证 DONE 只产生一个 Completed，并忽略其后同 chunk/后续 chunk 的帧 |
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

### protocol/responses_history.rs（Responses 原生记录与来源身份，W2-1a 纯协议，2026-09-19；W2-2 adapter 已接线）

- 事实源：`src/protocol/responses_history.rs`；测试 `responses_history_test.rs`；`protocol/mod.rs` + `lib.rs` re-export（`ResponsesHistoryV1`、`ResponsesHistoryItem`、`ResponsesHistoryItemKind`、`ResponsesSourceIdentity`、`HistoryError`、`AssistantPhase`、`RESPONSES_HISTORY_VERSION`）
- 契约：私有字段 + 校验构造/反序列化（`version == 1`）；只接受已完成 message/reasoning/function_call item——未知语义 item、未完成/缺失 status、重复 item id 或 call_id、arguments 非 JSON 对象、非法 phase 一律 `HistoryError` fail closed；内部按原 JSON 保存未知非语义字段
- 来源身份：`ResponsesSourceIdentity` 只持久化 16B 随机 nonce（hex）与域分隔 SHA-256 摘要；endpoint 用 `Url` 规范化结果（默认端口折叠、实际 path/query 参与比较），model 原样；`verify/matches` 以存档 nonce 重算，跨进程稳定，不落盘 URL/凭据；Debug 全脱敏；nonce 不是 secret，不提供保密或防篡改
- 投影与派生：`visible_text`/`refusals`/`reasoning_summary_text`/`tool_calls`（`ToolCall::id` 取 call_id，arguments 复用 `JsonObject`，每项只出现一次）；`project_input_items(endpoint, model)` 非同域返回 `SourceMismatch`，`project_generic_input_items` 是只含 message/function_call 的降级投影；wire 只含官方 input 字段
- 生产接线（openai_responses adapter）：`response.completed` 成功终态才构造记录并派生 `ModelResponse`；下一轮经 `ContentBlock::ResponsesNativeHistory` 回放（同域含密文、跨域降级）；`failed`/`incomplete`/EOF/取消/未知事件与未知 output item 一律不产出 Completed
