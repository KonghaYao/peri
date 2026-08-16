# langfuse-client 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：langfuse-client/src 源码、Doc.md（x-langfuse-ingestion-version: 4）

## 架构速览

- 数据流：`调用方构造 IngestionEvent → Batcher（mpsc 队列 + 后台 task）→ LangfuseClient::ingest（OTLP 转换 + HTTP 重试）→ Langfuse API`
- 入口：`LangfuseClient::new`（client.rs:31）；`Batcher::new`（batcher.rs:44，同时启动后台 task）；`ClientConfig::from_env`（config.rs:25，读 LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY / LANGFUSE_BASE_URL）
- 稳定不变量：发送走 OTLP 端点 `POST /api/public/otel/v1/traces`，必带 `x-langfuse-ingestion-version: 4` 头与 Basic auth（base64(public_key:secret_key)，client.rs:32-34）；4xx 不重试，网络错误/5xx 按 max_retries 指数退避（1s, 2s, 4s…）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 HTTP 发送/重试 | `src/client.rs` | `LangfuseClient::new`（:31）；`from_config`（:52）；`ingest`（:73） | 空事件直接 Ok；4xx 返回 `LangfuseError::IngestionApi`；5xx/网络错误重试 `max_retries` 次指数退避；reqwest 连接超时 5s / 请求超时 30s（:37-40） |
| 改批量/背压策略 | `src/batcher.rs` + `src/config.rs` | `Batcher::new`（:44）；`run_loop`（:75）；`add`（:189）/`try_add`（:225）/`flush`（:249，oneshot ack）；`BackpressurePolicy`（config.rs:47） | mpsc channel 容量 = max_events；定量（buffer ≥ max_events）或定时（flush_interval）触发 `do_flush`（:146）；DropOldest 满时弹最旧事件；Shutdown 先 flush 剩余再退出（Drop impl :269）；丢弃计数 `dropped_count`（:263） |
| 改遥测事件类型 | `src/types/mod.rs` | `IngestionEvent`（:330，12 变体）；`ObservationType`（:35）；`event_timestamp`（:14） | 所有变体必带 `id` + `timestamp` + `body`；body 结构 `deny_unknown_fields`；`SessionCreate/Update` 用 `session::SessionBody`（session.rs:6） |
| 改 Ingestion→OTLP 映射 | `src/types/conversion.rs` | `ingestion_events_to_otel`（:26，`pub(crate)`，经 types/mod.rs:7 与 client.rs:79 调用） | TraceCreate→root span（`langfuse.observation.type` 省略）；GenerationCreate 附 model/usage 属性；ID 去 dash 派生 OTel span/trace ID（`build_span_id` :12）；时间戳 rfc3339→nano（:590） |
| 改 OTLP 载荷结构 | `src/types/otlp.rs` | `OtelTraceExportRequest`（:10）；`OtelSpan`（:56）；`OtelAttributeValue::string/int/bool`（:118/127/136） | 直接对应 OTLP JSON wire 格式；属性值只支持 string/int/bool 三种 |
| 改错误类型 | `src/error.rs` | `LangfuseError`（Http / JsonSerialize / IngestionApi / QueueFull / ChannelClosed / Config） | 队列满 → QueueFull（add）；通道关闭 → ChannelClosed（try_add）；消费方据此映射丢弃原因 |
| 改配置读取 | `src/config.rs` | `ClientConfig::from_env`（:25）；`BatcherConfig::from_client`（:79） | 采样率 `trace_sampling`（默认 1.0 全报）；默认 batch_max_events=50 / flush_interval=10s / backpressure=DropNew / max_retries=3 |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| HTTP 客户端 | src/client.rs | `LangfuseClient`（:17，持 reqwest::Client + auth_header + max_retries） |
| 批量聚合 | src/batcher.rs | `Batcher`（:34）；`BatcherCommand`（:21，Add/Flush/Shutdown）；`report_dropped`（:172） |
| 配置 | src/config.rs | `ClientConfig`（:5）；`BatcherConfig`（:59）；`BackpressurePolicy`（:47） |
| 事件/载荷类型 | src/types/mod.rs | `TraceBody`（:95）/`ObservationBody`（:128）/`SpanBody`（:172）/`GenerationBody`（:207）/`EventBody`（:260）/`ScoreBody`（:291）/`SdkLogBody`（:322） |
| OTLP 映射 | src/types/conversion.rs | `ingestion_events_to_otel`（:26，pub(crate) 内部函数，不对外导出） |
| OTLP 类型 | src/types/otlp.rs | `OtelScopeSpan`（:35）/`OtelResource`（:27）/`OtelStatus`（:88）等 |
| 会话类型 | src/types/session.rs | `SessionBody`（:6） |
| 错误 | src/error.rs | `LangfuseError`（thiserror） |

## 跨模块契约

- 消费方（唯一生产消费方）：`peri-controller/src/langfuse/session.rs`（:20-46 实例化 `LangfuseClient` + `Batcher`，组合进 TelemetrySession）；`peri-controller/src/langfuse/tracer/event_builder.rs:34`（持 `&Batcher` 上报事件）；`peri-controller/src/langfuse/drop_telemetry.rs:58`（`LangfuseError::ChannelClosed` → BatcherClosed 丢弃原因映射）
- **新增 trace 阶段/span 的改动点在消费侧而非本 crate**：`peri-controller/src/langfuse/tracer/mod.rs`（`on_stage_start` :906 / `on_stage_end` :927，SpanCreate 延迟到 end 且仅 duration>0 才发送）、`tracer/stages.rs`（`StageSpans` 生命周期）、`peri-acp-types/src/event.rs:207` 的 `Stage` 枚举（阶段事实源）；本 crate 只在类型/OTLP 映射变化时才动（`types/mod.rs`、`types/conversion.rs`）
- `peri-agent/src/session/transcript.rs:254/:763` 仅注释引用 batcher 的 Shutdown 模式（flush 后退出），无代码依赖
- lib.rs re-export（:8-12）：`Batcher`、`LangfuseClient`、`BackpressurePolicy`/`BatcherConfig`/`ClientConfig`、`LangfuseError`、`GenerationBody`/`IngestionEvent`/`ObservationBody`/`ObservationType`/`SpanBody`
