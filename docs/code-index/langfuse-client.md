# langfuse-client 代码索引

导出重试的单调 deadline 与等待由 [peri-time](peri-time.md) 提供；HTTP 重试预算
和响应分类仍由本 crate 管理。

> 依据：源码、契约测试与 Langfuse OTLP v4。现行入口以代码为准。

## 数据流与边界

`生产观测完成 → IngestionEvent → 有界准入 → FIFO 聚合/屏障 owner → 有界并发 HTTP → Langfuse`

- `Batcher` 独占 worker/join 所有权；worker 的 `JoinSet` 持有全部发送任务。生产者不等待网络。
- 队列命令槽、排队 JSON 字节、单事件 JSON 字节、单批事件数及 HTTP 并发分别约束；HTTP 编码及响应另外限额。
- 生产者尚未提交的事件、原始对象的 allocator capacity 和进程 RSS 不属于这些序列化字节预算。
- flush 在 FIFO 位置提交当前 buffer，等待前缀全部发送任务再确认；后缀在确认之后才消费，不误等待后续请求。
- 完整成功不等于服务端已持久落库。HTTP 200 中的部分拒收、无效/截断响应及发送失败会进入失败账本。
- shutdown 关闭准入、排空已提交命令并 join 全部发送；取消等待保留原 worker handle，重复观察同一终态。

## 入口速查

| 需求 | 文件与入口 | 现行行为 |
| --- | --- | --- |
| HTTP/auth | `src/client.rs`：`LangfuseClient::{new,from_config,ingest,with_export_config}` | 连接池复用；Basic auth；`POST /api/public/otel/v1/traces`；`x-langfuse-ingestion-version: 4`；禁用 redirect 和 reqwest 内部 retry |
| 编码与预算 | `src/client_export.rs`：`ExportConfig`、`preflight`、`encode` | 转换前零分配计量输入；有界 writer 编码一次，重试共享 body；默认请求 4 MiB、响应 64 KiB、累计期限 60s |
| 接受结果 | `src/client_response.rs`：`read_success`、`parse` | 限额读取、JSON object 校验；rejectedSpans 支持数字/字符串，非零返回 PartialSuccess 且不整批重试；错误/日志不含原文 |
| 重试 | `src/client_retry.rs`：`parse_retry_after`、`jittered_backoff` | 429/502/503/504 与可恢复网络错误；Retry-After 秒/date；有界指数退避 + jitter；永久响应不重试 |
| 准入及背压 | `src/batcher/admission.rs`：`Admission::{try_add,send,close}` | 命令槽及 JSON 字节两维准入；DropOldest 仅回收最后一个 Flush 之后的 Add，必要时回收多个，不破坏前缀 |
| 事件计量 | `src/batcher/budget.rs`：`event_bytes` | 有界零分配 JSON writer；超大事件返回 PayloadTooLarge，不截断 |
| 聚合及并发 | `src/batcher/worker.rs`：`BatchWorker::{run,process}` | 定量/定时提交；全部 HTTP task 由唯一 owner 的 JoinSet 管理，并发上限可配置；flush/关闭等待发送终态 |
| 结果与关闭 | `src/batcher/failure.rs`、`src/batcher/shutdown.rs` | 增量确认水位与部署累计失败分离；worker join 错误不同于 HTTP 失败，摘要不含 payload |
| 安全计数 | `src/batcher/stats.rs`、`Batcher::stats` | 累计接受/拒绝/驱逐事件、提交/完成/失败批次、部分拒收批次及 span 数；在途数在发送 future 销毁时释放，取消不遗留；各字段非事务快照 |
| 参数 | `src/config.rs`：`BatcherConfig` | 默认 batch=50、queue=1024、in-flight=2、event=512 KiB、batch=4 MiB、queue bytes=16 MiB；构造前验证 |
| OTLP dispatch | `src/types/conversion.rs`：`ingestion_events_to_otel` | 完整 Create 产生 span；Update 和 Score 显式失败；Session/SDK record 不是 span，不导出 |
| 身份与时间 | `src/types/conversion/validation.rs`：`build_span` | trace=32hex、span/parent=16hex 非零；标准 ID 正常化保留，领域 ID 确定性 SHA-256 映射；父引用共用 span 映射；校验时间顺序 |
| 字段映射 | `src/types/conversion/{trace,observation,generation,metadata}.rs` | 各族字段独立；JSON input/output 不截断；metadata 保留 blob 并补充可过滤的具名属性 |
| 错误 | `src/error.rs`：`LangfuseError` | PayloadTooLarge、PartialSuccess、QueueFull、ChannelClosed、IngestionApi、WorkerJoinFailed 等边界结果 |

## 回归入口

- `src/client_{test,export_test,response_test,retry_test}.rs`：认证、响应、限额、重试预算、请求 body 复用。
- `src/batcher_concurrency_test.rs`：慢第一批下第二批可发送、并发上限、乱序完成、flush 前缀、部分拒收计数及 shutdown/worker 取消。
- `src/batcher/{admission_test,byte_budget_test}.rs`：关闭、未提交 permit、取消、字节释放与驱逐前缀保护。
- `src/batcher_{test,shutdown_test}.rs`：失败确认、shutdown 所有权、panic/cancel、DropOldest 与客户端 retry 唯一权威。
- `src/types/conversion_test.rs`：标准及领域 ID、父引用、完整时间、过滤/拒绝不支持事件与 metadata。
- `tests/otel_contract.rs`：真实 HTTP mock 验证认证、OTLP 载荷、身份映射、父关系与 Session record 过滤，不代表真实服务端验收。
- 命令：`cargo test -p langfuse-client --lib`、`cargo test -p langfuse-client --doc`、`cargo clippy -p langfuse-client --all-targets -- -D warnings`。

## 生产接线

- `peri-controller/src/langfuse/config.rs` 接受 settings 与环境变量；队列容量、并发及字节预算分别配置，环境变量优先。
- `peri-controller/src/langfuse/session.rs` 验证并装配 client/batcher，失败沿既有 Option 降级；部署关闭权与 turn-facing flush 分离。
- `peri-controller/src/langfuse/tracer/` 在观测结束后导出完整 Create；子 agent 及 Workflow 不依赖服务端 update/dedup。
- `peri-controller/src/langfuse/drop_telemetry.rs` 保存有界的 trace/事件类别/拒绝原因计数；拥塞以周期汇总诊断为主。
- 跨层所有权以 `docs/standards/architecture-contracts.md` 的 ARC-HOST-SHUTDOWN-001 为准；代码索引不承诺负载 SLO 或真实服务端验收。
