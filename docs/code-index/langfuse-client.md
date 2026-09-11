# langfuse-client 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11
> 依据：langfuse-client/src 源码、Doc.md（x-langfuse-ingestion-version: 4）

## 架构速览

- 数据流：`调用方构造 IngestionEvent → Batcher（有界命令队列 + 单个后台 task）→ LangfuseClient::ingest（OTLP 转换 + HTTP 重试）→ Langfuse API`
- 入口：`LangfuseClient::new`（client.rs:31）；`Batcher::{new,try_new}`（batcher.rs，先验证配置再启动后台 task）；`ClientConfig::from_env`（config.rs:25，读 LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY / LANGFUSE_BASE_URL）
- 稳定不变量：发送走 OTLP 端点 `POST /api/public/otel/v1/traces`，必带 `x-langfuse-ingestion-version: 4` 头与 Basic auth（base64(public_key:secret_key)，client.rs:32-34）；4xx 不重试，网络错误/5xx 按 max_retries 指数退避（1s, 2s, 4s…）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 HTTP 发送/重试 | `src/client.rs` | `LangfuseClient::new`（:31）；`from_config`（:52）；`ingest`（:73） | 空事件直接 Ok；4xx 返回 `LangfuseError::IngestionApi`；5xx/网络错误重试 `max_retries` 次指数退避；reqwest 连接超时 5s / 请求超时 30s（:37-40） |
| 改批量/背压策略 | `src/batcher.rs` + `src/batcher/{admission,worker}.rs` + `src/config.rs` | `Batcher::{add,try_add}`；`Admission::{try_add,send}`；`BatchWorker::run` | 容量 = max_events 个命令槽（含 Flush），Semaphore 等容量，短锁提交/关闭；DropOldest 只替换最后一个已准入 Flush 后的最旧 Add，新事件追加队尾，无可驱逐事件则 QueueFull；在途 HTTP 不变，Block 取消准入前不产生事件 |
| 改 flush 错误确认 | `src/batcher.rs` + `src/batcher/failure.rs` | `Batcher::flush`；`FailureLedger::{record_failure,snapshot,observe}` | 已准入 Flush 及其前缀不可驱逐，准入前已替换事件不在屏障集合；FIFO barrier 返回尚未确认的失败水位，实际收到 Err 才确认，取消/worker ack 不清错，旧确认不影响后续失败 |
| 改 shutdown 所有权 | `src/batcher.rs` + `src/batcher/{admission,shutdown,worker}.rs` | `Batcher::shutdown`；`Admission::close`；`CommandReceiver::drop`；`WorkerOwner::join` | 独立 watch 关闭准入/semaphore，只排空已提交命令，不等待持有 permit 的未提交生产者；receiver drop 唤醒发送者并释放 ack；保留原 JoinHandle 跨取消/重试，区分 HTTP 与任务失败 |
| 改遥测事件类型 | `src/types/mod.rs` | `IngestionEvent`（:330，12 变体）；`ObservationType`（:35）；`event_timestamp`（:14） | 所有变体必带 `id` + `timestamp` + `body`；body 结构 `deny_unknown_fields`；`SessionCreate/Update` 用 `session::SessionBody`（session.rs:6） |
| 改 Ingestion→OTLP 映射 | `src/types/conversion.rs` + `src/types/conversion/` | `ingestion_events_to_otel`（:31，`pub(crate)`）；`trace_create` / `span_create` / `generation_create` / `observation_create` / `score_create` 等私有纯函数 | 唯一穷尽 dispatch 按输入顺序逐事件产生 span，不合并 Create/Update；事件族保留各自属性差异；共享 ID 去 dash（`build_span_id` :17）与 RFC3339→nano（:130） |
| 改 OTLP 载荷结构 | `src/types/otlp.rs` | `OtelTraceExportRequest`（:10）；`OtelSpan`（:56）；`OtelAttributeValue::string/int/bool`（:118/127/136） | 直接对应 OTLP JSON wire 格式；属性值支持 string/int/double/bool；Score 数值优先转 double |
| 改错误类型 | `src/error.rs` | `LangfuseError`（Http / JsonSerialize / IngestionApi / QueueFull / ChannelClosed / WorkerJoinFailed / Config） | 队列满 → QueueFull（add）；通道关闭 → ChannelClosed（try_add）；WorkerJoinFailed 的 cancelled 字段区分取消/panic，不暴露 payload |
| 改配置读取/验证 | `src/config.rs` + `src/batcher.rs` | `ClientConfig::from_env`；`BatcherConfig::{from_client,validate}`；`Batcher::{new,try_new}` | 默认 max_events=50 / interval=10s / DropNew；容量须在 1..=Semaphore::MAX_PERMITS 且间隔非零；try_new 在 spawn 前返回 Config，new 同步 panic；legacy BatcherConfig.max_retries 保留但不生效，真实重试仅由传入 LangfuseClient 决定 |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| HTTP 客户端 | src/client.rs | `LangfuseClient`（:17，持 reqwest::Client + auth_header + max_retries） |
| 批量聚合公开面 | src/batcher.rs | `Batcher`；`BatcherCommand`（Add/Flush）；构造验证、add/flush/shutdown 只协调准入、确认与 owner |
| 队列准入 | src/batcher/admission.rs | `Admission` / `CommandReceiver`；同一有界 VecDeque owner，Semaphore 管容量等待，Notify 唤醒唯一接收者；queued permit 取出即释放，驱逐时复用 |
| 后台发送 | src/batcher/worker.rs | `BatchWorker::{run,process}`；单 worker 持 HTTP 与 buffer，定量/定时/FIFO flush；buffer 随实际事件增长，不按配置上限预分配；背压驱逐只归 Admission |
| 关闭任务 owner | src/batcher/shutdown.rs | `WorkerOwner`（:8）/`join`（:19）；等待时借用句柄，完成后缓存安全终态；无 worker→owner 引用环 |
| 背压与 shutdown 生命周期回归 | src/batcher_shutdown_test.rs + src/batcher/admission_test.rs | 真实 HTTP 门控验证 add/try_add DropOldest 身份、flush 前缀保护/取消/不等后批请求、client retry 唯一入口；保留 shutdown join/取消/并发终态；Admission 覆盖未提交 permit、receiver Drop 与取消等待 |
| flush 失败水位 | src/batcher/failure.rs | `FailureLedger`（:14）/`FlushSnapshot`（:20）；worker 记录失败，公开 flush 的接收方确认快照，重叠确认幂等 |
| flush 契约回归 | src/batcher_test.rs | `test_flush_barrier_*`：自动失败后空 flush、ack 后取消、旧确认保留新失败、并发观察顺序与安全摘要 |
| 配置 | src/config.rs + src/config_test.rs | `ClientConfig`、`BatcherConfig`、`BackpressurePolicy`；零值/上限错误在无 runtime 场景也先拒绝，new 明确同步 panic |
| 事件/载荷类型 | src/types/mod.rs | `TraceBody`（:95）/`ObservationBody`（:128）/`SpanBody`（:172）/`GenerationBody`（:207）/`EventBody`（:260）/`ScoreBody`（:291）/`SdkLogBody`（:322） |
| OTLP dispatch / 通用属性 | src/types/conversion.rs | `ingestion_events_to_otel`（:31）；12 分支顺序与 resource/scope 包装；`append_common_obs_attrs`（:82）、`build_status`（:116） |
| Trace 映射 | src/types/conversion/trace.rs | `trace_create`（:4）；root 身份与 trace 属性，envelope timestamp 用作 start |
| Span / Observation / Event 映射 | src/types/conversion/observation.rs | `span_create`（:6）/`span_update`（:43）；`event_create`（:75）；`observation_create`（:103）/`observation_update`（:153），保留 Create/Update 属性差异 |
| Generation 映射 | src/types/conversion/generation.rs | `generation_create`（:7）/`generation_update`（:91）；Create 的 model parameters/legacy usage/cost/prompt 属性不补到 Update |
| Score / SDK / Session 映射 | src/types/conversion/metadata.rs | `score_create`（:4）、`sdk_log`（:52）、`session_create`（:71）/`session_update`（:103） |
| 转换契约测试 | src/types/conversion_test.rs | `mixed_event_families_keep_create_update_and_parent_order`；12 种混合事件、字段差异、状态/时间/ID 边界与导出包装 |
| OTLP 类型 | src/types/otlp.rs | `OtelScopeSpan`（:35）/`OtelResource`（:27）/`OtelStatus`（:88）等 |
| 会话类型 | src/types/session.rs | `SessionBody`（:6） |
| 错误 | src/error.rs | `LangfuseError`（thiserror） |

## 跨模块契约

- 部署生命周期：Controller `LangfuseSession::new_owned` 授予 non-Clone shutdown 权限；ACP `host/lifecycle.rs::AcpHostHandle` 在资源 drain 完整后关闭，TUI/print `acp_client/deployment.rs` 显式 close transport 并 await host；turn 继续只 flush（ARC-HOST-SHUTDOWN-001）。
- 消费方（唯一生产消费方）：`peri-controller/src/langfuse/session.rs`（实例化 `LangfuseClient` + `Batcher::try_new`，非法批处理配置沿 None 降级，组合进 `LangfuseSession`）；`peri-controller/src/langfuse/tracer/event_builder.rs`（经 `LangfuseSessionLike::try_add` 同步上报事件）；`peri-controller/src/langfuse/drop_telemetry.rs`（`LangfuseError::ChannelClosed` → BatcherClosed 丢弃原因映射）。Langfuse bridge/tracer 的实现已归 `peri-controller/src/langfuse/`；`peri-acp/src/event/forwarder.rs` 仍只是把协议化前事件分支接到可选 `LangfuseBridge` 的接线点，不是 bridge 实现或遥测状态宿主。
- **新增 trace 阶段/span 的改动点在消费侧而非本 crate**：`peri-controller/src/langfuse/tracer/`（`span_events.rs` 的 `on_stage_start` :117 / `on_stage_end` :138，SpanCreate 延迟到 end 且仅 duration>0 才发送）、`tracer/stages.rs`（`StageSpans` 生命周期）、`peri-acp-types/src/event.rs:207` 的 `Stage` 枚举（阶段事实源）；本 crate 只在类型/OTLP 映射变化时才动（`types/mod.rs`、`types/conversion.rs`）
- `peri-agent/src/session/transcript.rs:254/:763` 仅注释引用 batcher 的 Shutdown 模式（flush 后退出），无代码依赖
- lib.rs re-export（:8-12）：`Batcher`、`LangfuseClient`、`BackpressurePolicy`/`BatcherConfig`/`ClientConfig`、`LangfuseError`、`GenerationBody`/`IngestionEvent`/`ObservationBody`/`ObservationType`/`SpanBody`
