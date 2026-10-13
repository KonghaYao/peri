# langfuse-client 代码索引

导出重试的单调 deadline 与等待由 [peri-time](peri-time.md) 提供；HTTP 重试预算
和响应分类仍由本 crate 管理。

> 依据：源码、契约测试与 Langfuse OTLP v4。现行入口以代码为准。

## 数据流与边界

`生产观测完成 → 借用 SnapshotSource → 冻结准入（Inline/Spooled）→ 有界准备/拆批 → 流式 OTLP 编码 → 有界并发 HTTP → Langfuse`

- `Batcher` 独占 worker/join 所有权；worker 的 `JoinSet` 持有全部发送任务与阻塞准备任务。生产者不等待网络，`try_freeze` 永不等待。
- 队列命令槽、排队 JSON 字节、单事件 JSON 字节、单批事件数及 HTTP 并发分别约束；HTTP 编码及响应另外限额。
- 冻结 input 与导出正文按阈值走 Inline 或 spool 文件；所有阻塞 IO/压缩只由 worker 的有界 JoinSet/spawn_blocking 集合持有。
- 生产者尚未提交的事件、原始对象的 allocator capacity 和进程 RSS 不属于这些序列化字节预算。
- flush 在 FIFO 位置提交当前 buffer，等待前缀（事件水位 + 准备水位）全部发送任务再确认；后缀在确认之后才消费，不误等待后续请求。
- 完整成功不等于服务端已持久落库。HTTP 200 中的部分拒收、无效/截断响应及发送失败会进入失败账本。
- shutdown 关闭准入、排空已提交命令并 join 全部发送；取消等待保留原 worker handle，重复观察同一终态；未 join 完成时返回 Pending/PersistedPending/DeliveryIncomplete 而不是 Complete。

## 入口速查

| 需求 | 文件与入口 | 现行行为 |
| --- | --- | --- |
| HTTP/auth | `src/client.rs`：`LangfuseClient::{new,from_config,ingest,with_export_config}` | 连接池复用；Basic auth；`POST /api/public/otel/v1/traces`；`x-langfuse-ingestion-version: 4`；禁用 redirect 和 reqwest 内部 retry |
| 编码与预算 | `src/client_export.rs`：`ExportConfig`；`src/encoding/{mod,writer}.rs`：`write_otlp`、`write_otlp_counted`、`measure_otlp`、`BudgetedWriter` | 计量与写入共用同一 emitter（契约 §3.4）；默认请求 4 MiB、响应 64 KiB、累计期限 60s、初始退避 1s、上限 30s、wire 4 MiB |
| 接受结果 | `src/client_response.rs`：`read_success`、`parse` | 限额读取、JSON object 校验；rejectedSpans 支持数字/字符串，非零返回 PartialSuccess 且不整批重试；错误/日志不含原文 |
| 重试 | `src/client_retry.rs`：`parse_retry_after`、`jittered_backoff` | 429/502/503/504 与可恢复网络错误；Retry-After 秒/date；有界指数退避 + jitter；永久响应不重试 |
| 准入及背压 | `src/batcher/admission.rs`：`Admission::{try_add,send,close}` | 命令槽及 JSON 字节两维准入；DropOldest 仅回收最后一个 Flush 之后的 Add，必要时回收多个，不破坏前缀 |
| 事件计量 | `src/batcher/budget.rs`：`event_bytes`、`queued_bytes`、`snapshot_allowance` | 三种口径互不代偿：事件原文 JSON、队列记账（文件引用按描述内存计）、冻结输出 allowance；超出即 PayloadTooLarge/拒收，不截断 |
| 聚合及并发 | `src/batcher/worker.rs`：`BatchWorker::{run,process}` | 定量/定时提交；全部 HTTP task 由唯一 owner 的 JoinSet 管理，并发上限可配置；flush/关闭等待发送终态 |
| 结果与关闭 | `src/batcher.rs`：`Batcher::shutdown`；`src/batcher/{failure,shutdown}.rs`：`DeliveryLedger`、`ShutdownSnapshot`、`WorkerOwner`、`RetainedOwner::converge` | 增量确认水位与部署累计失败分离；worker join 错误不同于 HTTP 失败，摘要不含 payload；grace 逻辑在 `Batcher::shutdown` 内：到期返回 `Pending{counts}`（`active_tasks` 至少 1），保留同一 `WorkerOwner` 与 retained 状态供后续调用重入收敛——不 abort、不取走 handle、不返回 Complete |
| 安全计数 | `src/batcher/stats.rs`、`Batcher::stats` | 累计接受/拒绝/驱逐事件、提交/完成/失败批次、部分拒收批次及 span 数；在途数在发送 future 销毁时释放，取消不遗留；各字段非事务快照 |
| 参数 | `src/config.rs`：`BatcherConfig`、`TelemetryConfig`、`ClientConfig` | batch=50、queue=1024、in-flight=2、event=4 MiB、batch=4 MiB、queue bytes=16 MiB；telemetry 默认 spool=true、gzip、memory budget=32 MiB、prepare=1、chunk=64 KiB、quota=1 GiB、TTL=24h、wire=4 MiB、grace=60s；`TelemetryConfig::validate` 校验非零、阈值/块不超内存预算与时限溢出，`Batcher::try_new` 另校验 client 与 telemetry 的 compression/max_wire_bytes 一致 |
| spool 端口与身份 | `src/spool.rs`：`SpoolStore`（sealed）、`SpoolObject`、`ObjectId`、`SpoolPage`、`SpoolUsage`、`route_fingerprint` | 外部只能消费 trait；`ObjectId` 仅 crate 内构造、端口不可外部实现；route 按端点指纹隔离，凭据轮换不跨发 |
| native 暂存 | `src/spool/native.rs`：`NativeSpool::{open_at,open_default}`、`try_lock_file`、`LockAttempt`、`probe_durable_io`；`src/spool/{native_object,native_quota,native_scan}.rs` | 私有目录 0700/文件 0600；quota reservation→publish 不双计；claim 用 OS 文件锁而非 PID；启动分页扫描回收半写残留；`LockAttempt::{Free,Held,Gone}` 把「锁文件父目录已被并发删除」（macOS `EINVAL`／Linux `ENOENT`）与真实 IO 故障分开：`list`/`scan_totals` 跳过、`claim` 归 `NotFound`、幂等 `delete` 视为已达成，装配期目录缺失在创建路径仍显式报错；`probe_durable_io` 只判 fsync/advisory lock 能力，对端**持有** domain 锁（`WouldBlock`）算竞争而非 `Unavailable`（否则整次装配降级为有界内存模式） |
| 清理与租约 | `src/spool/cleanup.rs`：`CleanupRegistration`、`CleanupRegistry`、`CleanupSlot` | 删除失败保留 claim/slot/quota 并由原 owner 重试，delete 成功后调用 `disarm_after_delete` 回收注册/槽/已写配额；shutdown 移交由 `src/batcher/recovery.rs::release_hand_off` 在解锁 claim 后对替换出的登记调 `disarm_after_handoff`（保留磁盘配额与正文、只还本地槽），不把 Drop 隐式当移交 |
| 内存预算与工作区 | `src/memory.rs`：`MemoryBudget::{new,for_batcher,try_reserve}`、`MemoryPermit`、`try_prepare_workspace`/`try_http_workspace` | retained 与固定进度分区不互借；worklease non-Clone、Drop 回池；used 含已隔离进度账单且只计一次 |
| 冻结来源与事件 | `src/snapshot.rs`：`SnapshotSource`、`SnapshotTicket`、`TelemetryEvent::try_new`、`InputSource` | 只借用已存在 Arc、不重建 JSON 树；事件在 gate 前完成预算预约并持有输出 allowance |
| 冻结准入与拆批 | `src/batcher/freeze.rs`：`freeze_snapshot`、`InputSpool`、`resolve_event`；`prepare.rs`：`prepare_event`；`split.rs`：`split`、`BatchLimits`、`EventMeasure`、`SplitPlan`、`oversize_reason` | 阈值决定 Inline/Spooled；磁盘失败只允许一次非等待内存回退；编码膨胀按 final OTLP 拆批、超大单条经 `RejectedEvent` 独立拒收 |
| 正文所有权 | `src/payload.rs`：`BudgetedBytes`、`ExportPayload`、`PayloadBody`、`InputSnapshot` | 正文与 permit 绑定同一预算身份；共享 Bytes 延续 permit 至最后一个引用 |
| 键序稳定化 | `src/stable_json.rs`：`StableJson`、`StableMap`、`Stable`；`src/snapshot.rs`：`write_stable_json` | 进入 OTLP 正文与冻结正文的 JSON 一律经此序列化：对象键字典序、map 按键排序；输出不随编译期 `serde_json/preserve_order`（workspace 全量构建经 agent-client-protocol 启用）漂移。`write_stable_json` 是键序唯一实现，供本 crate 的 `ValueSource` 与 controller 的 messages/tools fallback 复用 |
| 统一流式编码 | `src/encoding/{mod,writer,json_string}.rs`：`write_otlp`、`measure_otlp`、`BudgetedWriter`、`write_json_string`、`EscapeWriter` | 计量与写入共用同一 emitter/字段映射；input/output 分块嵌入、metadata 边序列化边转义，不物化整串 |
| 请求重建 | `src/client_send.rs`：`MemoryCheckpoint`/`FileCheckpoint`、`BodyTaskOwner`；`src/client_body.rs`：`file_body`、`memory_registration` | 文件正文每次 attempt 从字节 0 重开、drain 后才可再次 arm；每次 HTTP 前持久 attempts |
| 交付结果与恢复 | `src/batcher/result.rs`：`DeliveryCounts`、`FlushReport`、`ShutdownReport`；`src/batcher/recovery.rs`：`RetryQueue`、`RetryEntry`、`HandOff(Plan)`、`scan`、`register_discard` | Pending/PersistedPending/DeliveryIncomplete 按 owner 与待交付事实区分；恢复与 live 事件共用唯一 worker，TTL 不因重试重置；移交只对持久文件正文，内存 payload 仍留在本地 pending |
| 物化回归（测试专用） | `src/types/conversion.rs`：`ingestion_events_to_otel`（`#[cfg(test)]`，只由 `conversion_test` 与 `encoding::stream_test` 调用） | 驱动同一 emitter 再解析回 OTLP 结构供既有断言对比，不是生产 dispatch，也不构成第二条业务分支；生产编码走 `encoding::{write_otlp,write_otlp_counted}`。Create 产生 span；Update 与 Score 使整批失败；Session/SDK record 不是 span，不导出 |
| 身份与时间 | `src/types/conversion/validation.rs`：`build_span` | trace=32hex、span/parent=16hex 非零；标准 ID 正常化保留，领域 ID 确定性 SHA-256 映射；父引用共用 span 映射；校验时间顺序 |
| 字段映射 | `src/types/conversion/{trace,observation,generation,metadata}.rs` | 各族字段独立；JSON input/output 不截断；metadata 保留 blob 并补充可过滤的具名属性 |
| 错误 | `src/error.rs`：`LangfuseError` | PayloadTooLarge、PartialSuccess、QueueFull、ChannelClosed、IngestionApi、WorkerJoinFailed 等边界结果；新增 Config、Spool、MemoryBudgetExceeded、SingleSpanTooLarge、BudgetMismatch、Pending/PersistedPending/DeliveryIncomplete、SnapshotPreparationFailed；交付结果错误不能被 `is_ok` 当成完成 |

## 回归入口

- `src/client_{test,export_test,response_test,retry_test}.rs`：认证、响应、限额、重试预算、请求 body 复用。
- `src/batcher_concurrency_test.rs`：慢第一批下第二批可发送、并发上限、乱序完成、flush 前缀、部分拒收计数及 shutdown/worker 取消。
- `src/batcher/{admission_test,byte_budget_test}.rs`：关闭、未提交 permit、取消、字节释放与驱逐前缀保护。
- `src/batcher_{test,shutdown_test}.rs`：失败确认、shutdown 所有权、panic/cancel、DropOldest 与客户端 retry 唯一权威。
- `src/types/conversion_test.rs`：标准及领域 ID、父引用、完整时间、过滤/拒绝不支持事件与 metadata。
- `src/encoding/stream_test.rs`：≥100 边界样本的 input/output 逐字节保真、UTF-8 跨块转义、gzip 与 none 共享 OTLP 语义。
- `src/stable_json_test.rs`：`Value` 保插入序与 `HashMap` 随机序下仍得到字典序字节（`preserve_order` 构建的真实判据）。
- `src/{memory,payload,snapshot}_test.rs`：预算预留/释放一次、permit 与正文身份绑定、事件来源冲突与 ticket 取消。
- `src/spool/{native_test,types_test,cleanup_test}.rs`：原子发布、跨进程 claim/quota、8 MiB 配额含 writing 与失败删除、分页扫描与残留回收；取锁语义（不可领取判定、并发删除不被升级为 IO 故障、能力探针竞争）在 `src/spool/native_test/lock.rs`，由 `native_test.rs` 显式 `#[path]` 挂载。
- `src/batcher/{freeze_test,prepare_test,split_test,spool_test,recovery_test,pending_test,send_test}.rs`：阈值边界、编码膨胀拆批、超大单条拒收、前缀屏障、TTL/恢复与 Pending 计数。
- `src/spool_test.rs`、`src/client_send_test.rs`：route 指纹与凭据轮换隔离、文件正文从 0 重开、attempt checkpoint。
- `tests/otel_contract.rs`：真实 HTTP mock 验证认证、OTLP 载荷、身份映射、父关系与 Session record 过滤，不代表真实服务端验收。
- `tests/spool_http.rs`：真实 TCP 连接上验证中途断连后第二次 attempt 从字节 0 重开、gzip/none 正文与长度、spool 文件正文不出现 chunked 传输；用 TCP 断流而不是 503 代替。
- 重型验收（1000 次调用长会话、512 MiB 积压、断网 10 分钟恢复、四边界崩溃注入）尚无离线用例文件，缺口见 [实施报告](../../spec/issues/2026-10-12-langfuse-telemetry-spool-report.md)。
- 命令：`cargo test -p langfuse-client --lib`、`cargo test -p langfuse-client --doc`、`cargo clippy -p langfuse-client --all-targets -- -D warnings`。

## 生产接线

- `peri-controller/src/langfuse/config.rs` 接受 settings 与环境变量；队列容量、并发及字节预算分别配置，环境变量优先。
- `peri-controller/src/langfuse/session.rs::{new,new_owned}` 接受 `Option<Arc<dyn spool::SpoolStore>>`：装配点按 `spool_enabled` 决定是否提供 native 能力，controller 只消费 trait、不打开文件系统；`TelemetryConfig` 与 client 配置一致性由 `Batcher::try_new` 校验。
- `peri-controller/src/langfuse/session.rs` 验证并装配 client/batcher，失败沿既有 Option 降级；部署关闭权与 turn-facing flush 分离，`map_shutdown` 把 Pending/PersistedPending/DeliveryIncomplete 显式映射而不落 UnexpectedFailure。
- `peri-controller/src/langfuse/tracer/` 在观测结束后导出完整 Create；输入经 `tracer/input_snapshot.rs` 借用成快照来源，冻结与落盘由 client worker 完成；子 agent 及 Workflow 不依赖服务端 update/dedup。
- `peri-controller/src/langfuse/drop_telemetry.rs` 保存有界的 trace/事件类别/拒绝原因计数；拥塞以周期汇总诊断为主。
- 宿主装配在 `peri-acp/src/host/telemetry.rs::assemble`（契约 §7 P4b，`host/mod.rs` 挂 `mod telemetry`）：按 `spool_enabled`、凭据与 `route_fingerprint` 决定是否打开 native 端口并注入 `LangfuseSession::new_owned`，降级为有界内存模式并留 WARN；宿主只保留共享 session 与 `LangfuseShutdownOwner`，`LangfuseShutdownReport::retains_ownership` 决定待交付结果是否保留下一次关闭上下文。回归入口 `peri-acp/src/host/telemetry_test.rs`（含跨两次 host shutdown 重入）与 `peri-acp/src/host/stdio/langfuse_shutdown_test.rs`；命令与结果见 [实施报告](../../spec/issues/2026-10-12-langfuse-telemetry-spool-report.md)。
- 跨层所有权以 `docs/standards/architecture-contracts.md` 的 ARC-HOST-SHUTDOWN-001、ARC-TELEMETRY-SPOOL-001 为准；代码索引不承诺负载 SLO 或真实服务端验收。
