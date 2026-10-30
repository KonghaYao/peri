# Langfuse telemetry spool：冻结接口契约与任务分解

## 1. 状态与范围

- 状态：**已实施，待用户验收**（P5 2026-10-12 回填；文档与验收表同步流复跑后更新）。P1–P5 均已落地：P4b 宿主装配交付后，`peri-acp --lib` 827 passed、workspace `--all-targets` EXIT=0、WASM 目标检查 EXIT=0（命令与数字见[实施报告](2026-10-12-langfuse-telemetry-spool-report.md) §2.1）。§9 已逐行标注实际状态与缺口；真实端点、512 MiB 积压 RSS 与性能项仍未执行，不宣称产品验收完成。已知未通过项：`cargo fmt --all -- --check` EXIT=1（3 个源码文件，属源码流，见报告 §8.3）；§10 新增待裁决偏差 D8。
- 实施根：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-telemetry-spool`；本次核对 HEAD 与本地 `origin/main` 均为 `607f8a222a4abaf37a7d3d68863cd244a1a37bb3`，未 fetch；开始时工作树干净。
- 需求原文见 Git 历史（仅加元信息）；以下路径均相对实施根，行号只定位此基线。代码块是冻结待实施的签名，尚未落地，不是现有代码。
- 只改变客户端配置、冻结输入、OTLP 编码/传输、文件暂存及关闭报告；不改服务端、数据库表、模型事件 wire、采样/身份/时间/usage/成本语义。
- 偏离索引：D1 基线更新；D2 环境变量去掉 `PERI_`；D3 native 适配器留在 `langfuse-client`；D4 新配置解析改为显式 Result；D5 新默认值及测试迁移；D6 OS 阻塞 IO 超时不伪装已 join；D7 已裁决：Cargo.lock 为生成文件，不适用源码行数上限。理由和验证边界见 §10。
- C1 的“wire 不变”指 gzip 解压后的 OTLP JSON 字段/值与 input 字符串字节不变；gzip 默认开启会改变 HTTP 正文字节及 header，这是计划明确授权。`none` 保留未压缩格式。

## 2. 事实核对（亲自 Read/Grep，不以简报替代证据）

| 简报条目 | 核对结论与基线锚点 |
| --- | --- |
| 1：配置键、默认、优先级 | 已核对：`peri-config/src/observability.rs:3–52,68–176` 有 13 键、event=512 KiB、batch=4 MiB、queue=16 MiB；env 在 settings 后；旧非法数值忽略；现有 `resolve(...) -> LangfuseConfig` **不是 Result**。 |
| 1：配置数据面/生产映射 | 已核对：`peri-config/src/assembly.rs:78–85,177–202` 只取 global langfuse；`system.rs:167–183` 发布投影；`peri-controller/src/langfuse/session.rs:82–115` 分别构造 ExportConfig/BatcherConfig；`peri-acp/src/host/assemble.rs:911–919` 调用 `new_owned`。 |
| 1：client 配置 | 已核对：`langfuse-client/src/config.rs:5–42,60–131`；`ClientConfig::from_env` 仅读认证/URL，不是生产配置权威；`BatcherConfig::validate` 为 `pub(crate)`，校验非零容量、event≤batch/queue、非零 flush；`max_retries` 是不参与执行的旧字段。 |
| 1：测试/目录 | 已核对：`peri-config/src/observability_test.rs:6–111` 纯 BTreeMap，旧 key 数断言 13；`docs/standards/environment-variables.md:55–67` 同样 13 行。未复核“全仓仅测试使用 ClientConfig”“不存在第二处 settings 消费”的穷尽性，不据此删除 public API。 |
| 2：计量口径 | **修正简报**：`client.rs:90` 的 `preflight(&events, limit)` 计量事件数组 JSON；`:91–92` 转换后 encode 才计量完整 OTLP JSON；两者上限相同但数值不同。`client_export.rs:56–106` 是泛型 Serialize，不固定为 OTLP；`batcher/budget.rs:27–38` 单事件原文 JSON 是第三个口径。 |
| 2：编码副本 | 已核对：`types/conversion.rs:18–75` 构造整棵 OTLP；`:79–100` input/output `to_string`；`types/conversion/trace.rs:32–42`、`metadata.rs:8–15` 同样物化。必须替换映射输出方式，不另建一套字段业务规则。 |
| 2：HTTP/重试 | 已核对：`client.rs:33–42,72–185`：new 参数为四个借用/值参数，ingest async；禁 redirect/reqwest retry；POST、三 header、Vec body、`try_clone`；429/502/503/504 与四类传输错误；200 委托 read_success，其错误不重试。 |
| 2：错误/测试/依赖 | 已核对：`error.rs:4–32` 九变体；`client_export_test.rs:53–100` 限额和同一 body 指针测试；`Cargo.toml:83–86` reqwest=0.13.4、json/rustls；`langfuse-client/Cargo.toml:6–23` 无 stream feature，无 native 产品 cfg。未复核 retry jitter/response parser 全部细节，要求原文件行为原样保留。 |
| 2：枚举与平台 | 已核对 `types/mod.rs:326–414` 实际 12 变体，旧注释写 10；无需借此改无关 wire。未复核“全仓无 ReaderStream/wasm32 cfg”的负面结论；不能把该简报当目标平台可编译证明。 |
| 3：批处理及 owner | 已核对：`batcher.rs:30–87` Add 持完整事件、Batcher 持 worker；`batcher/admission.rs:23–43` 队列槽/字节；`batcher/worker.rs:26–37,73–117,138–168` 唯一 worker、JoinSet、FIFO drain，现有聚合按原文而非最终 OTLP。 |
| 3：结果/统计 | 已核对：`batcher/failure.rs:19–35,46–81` flush 增量失败和 shutdown 累计失败分离；`shutdown.rs:8–35` 借用 handle await，取消后可再 join；`stats.rs:4–42` 除 rejected_spans:u64 外计数为 usize，Relaxed 非事务。未逐条复核 admission 的驱逐循环与简报列举的所有测试函数，必须保留既有测试。 |
| 4：generation | 已核对：`peri-controller/src/langfuse/tracer/generation.rs:16–34,97–132,168–213` 保留 Arc<Value>/fallback，结束/取消才物化 Value；raw 到达释放 fallback；保留终态和 retry metadata。 |
| 4：接线/闸门 | 已核对：`tracer/llm_events.rs:60–119,274,299`；`tracer/registry/types.rs:78–87` 闸门也持原文，不能只改 tracker；`tracer/event_builder.rs:35–70` 同步入队与安全拒收统计。未复核 gate=64 的全部驱逐/重放逻辑；保持原顺序并加字节预算，不重写归属机制。 |
| 3/4：turn fire-and-forget | **限定结论**：`tracer/turn.rs:69,119–125,179–185` 返回 `JoinHandle<()>`，不是“spawn 后必然无 owner”；本次没有证明所有上层消费均 join。新 IO 一律归 Batcher，不借用这个 turn spawn 承担写盘/发送。 |
| 4：关闭贯穿 | 已核对：`session_like.rs:9–19` 为 `Send + Sync`、boxed Send future；`session.rs:33–79` non-Clone shutdown owner；`peri-acp/src/host/lifecycle.rs:172–175` Complete/TelemetryFailed；`peri-tui/src/acp_client/deployment.rs:49–63` 遥测失败不改变成功业务结果。 |
| 5：规范 | 已核对：`docs/standards/index.md:39–43` 1000 行适用于源码/测试；`rust.md:11–27` 结构化错误、不可堵塞 async runtime；`testing.md:16–41,190–199,258–266` 独立测试/真实生命周期；`documentation.md:27–31` 单一事实源。 |
| 5：资源/依赖边界 | 已核对 `architecture-contracts.md:124–129` 部署关闭与 turn-only flush；`langfuse-client/Cargo.toml:7–16` 无业务 crate；`peri-acp/Cargo.toml:37–40,67` 当前仅 dev 依赖 langfuse-client。未发现这些已读规则强制把遥测 adapter 放 peri-resources；采用用户 C3 默认，不反向增加依赖。 |
| 5：规模/未核对项 | 实测 `wc -l`：assemble.rs=983，Cargo.lock=7434；`STD-SIZE-001` 不适用于生成文件 Cargo.lock，不存在“所有修改文件≤1000行”的用户要求，见已裁决 D7。简报其余 peri-resources 工厂、完整 WASM 传递闭包、脚本内部与缓存预热均未独立验证，不作为已通过证据。 |

### 2.1 历史核验的证据范围与独立签名核验

以下保留上次核验的证据记录，其中“本次”指该历史核验，不代表本轮重新读取全部文件：本次 Read/Grep 的证据是下表列明的文件/源码范围，不是整个仓库目录。HEAD 仍为 `607f8a222a4abaf37a7d3d68863cd244a1a37bb3`；本次开始时契约与需求文件均为 untracked（不同于初稿开始时的干净状态），没有产品源码改动。§2 原表保留初稿核对结论；下表才是本次独立复核，不将未读的旧条目升级为新证据。计划书只读取 `/Users/konghayao/Downloads/peri-telemetry-spool-plan.md:1–207`（§1–§8）；§9 仍保留初稿验收映射，本次未重验。

| 基线签名（真实源码） | 现状 → 目标 | 全部仓内直接消费者/迁移范围（不含无法枚举的库外调用方） |
| --- | --- | --- |
| `peri-config/src/observability.rs:68–71` | 模块函数 `resolve(&Value, &BTreeMap<String,String>) -> LangfuseConfig`，不是关联方法 → 同参 `Result<LangfuseConfig, LangfuseConfigError>` | 同文件 `:64`；`peri-config/src/assembly.rs:191`；`observability_test.rs:7,25,72,102,116` |
| 同文件 `:62–65` | `pub fn load_with_settings(&Value) -> Self` → `Result<Self, LangfuseConfigError>` | 同文件 `from_env:58`；本次检索无其他具名仓内调用 |
| 同文件 `:57–60` | `pub fn from_env() -> Option<Self>` → `Result<Option<Self>, LangfuseConfigError>` | 本次检索无具名仓内调用；保留 public 入口，不以此证明外部未使用 |
| `langfuse-client/src/config.rs:97–120` | `pub(crate) fn validate(&self) -> Result<(), crate::LangfuseError>` → 签名不变，加入实际进度保留校验 | `langfuse-client/src/batcher.rs:63`；新预算构造消费者见 §4 |
| 同文件 `:123–131` | `pub fn from_client(&ClientConfig) -> Self` → 签名不变，复制 telemetry、删除旧 max_retries | `langfuse-client/src/config_test.rs:133` |
| `langfuse-client/src/client_export.rs:35–53` | `pub(super) fn validate(&self) -> Result<(), LangfuseError>` → 可见性/返回不变，增加 wire 校验 | `langfuse-client/src/client.rs:65`；`client_export_test.rs:17–49` 间接验证 |
| 同文件 `:93–107` | `pub(super) fn encode(&impl Serialize, usize) -> Result<Vec<u8>, LangfuseError>` → §3.4 `pub(crate) encode(&[TelemetryEvent], BudgetedWriter, &ExportConfig) -> Result<ExportPayload, LangfuseError>` | `client.rs:92`；`client_export_test.rs:66,69,87` 及该文件序列化失败测试；目标 P2a/P2b/P3a 共用 |
| `langfuse-client/src/client.rs:33–51,54–61` | `pub fn new(&str, &str, &str, usize) -> Self` → 不变 | `from_config:55`；`peri-controller/src/langfuse/session.rs:86`；client 的 `src/{client_test,client_export_test,client_response_test,client_retry_test,batcher_test,batcher_shutdown_test,batcher_concurrency_test,config_test}.rs`、`tests/{logging,otel_contract}.rs` |
| 同文件 `:72–81` | `pub async fn ingest(&self, Vec<IngestionEvent>) -> Result<(), LangfuseError>` → 不变；`ingest_with_limit` 不再承担 worker 文件发送 | client 的 `src/{client_test,client_export_test,client_response_test,client_retry_test}.rs`、`tests/otel_contract.rs`；worker 原 limited 路径迁入 P2b/P3a |
| `langfuse-client/src/batcher.rs:56–63` | `new(LangfuseClient, BatcherConfig) -> Self` / `try_new(...) -> Result<Self, LangfuseError>` → 同步各加 `Option<Arc<dyn SpoolStore>>` | `new→try_new:57`；`peri-controller/src/langfuse/session.rs:115`；client 的 `src/{batcher_test,batcher_shutdown_test,batcher_concurrency_test,config_test}.rs`、`tests/logging.rs` |
| 同文件 `:150–177` | `pub async fn flush/shutdown(&self) -> Result<(), LangfuseError>` → 分别 `Result<FlushReport/ShutdownReport, LangfuseError>` | flush 内 `shutdown:158,164`；controller `session.rs:69,140`；client 的 `src/{batcher_test,batcher_shutdown_test,batcher_concurrency_test,config_test}.rs`、`tests/logging.rs`；所有旧 `Ok(())` 匹配同步迁移 |
| `peri-controller/src/langfuse/session.rs:54–63,82–131` | `pub async fn new_owned(LangfuseConfig, String) -> Option<(Arc<Self>, LangfuseShutdownOwner)>` / `new(...) -> Option<Self>` → 各加 spool 参数，async/返回不变 | `new_owned→new:58`；`session_test.rs:15`；`peri-acp/src/host/assemble.rs:913`、`host/stdio/langfuse_shutdown_test.rs:27` |
| `peri-controller/src/langfuse/session_like.rs:9–19` | 真名 `LangfuseSessionLike: Send + Sync`，`flush` 为 boxed Send future，结果 `Result<(), LangfuseError>` → §3.6 新方法及 FlushReport；本文 SessionLike 仅简称，不新建同名 trait | 实现：`session.rs:134`、`fake_session.rs:44`、`metric_sink_test.rs:206`、`tracer/event_builder_test.rs:24`；消费：`tracer/event_builder.rs:53–70`、`tracer/turn.rs:120,182`，既有 tracer/bridge/forwarder 测试保留 |
| `peri-controller/src/langfuse/tracer/generation.rs:97–125,168–213` | tracker `on_llm_start(&mut self,&str,usize,Arc<Vec<BaseMessage>>,Arc<Vec<ToolDefinition>>) -> GenerationStart`、`on_llm_request_payload(...,Arc<Value>) -> ()`、`on_llm_end(...)->Option<GenerationEnd>`、`take_all_active()->Vec<AbandonedGeneration>` → start/request 改消费已准入 InputSource，end/abandoned 的 `input_json: Value` 改 `input: InputSource`，保留身份/终态/retry 字段 | `tracer/llm_events.rs:70,75,111,146,176,274`、`subagent_events.rs:198`、`turn_fallback.rs:83,164`、`generation_test.rs`、`tracer_test.rs:248`；gate `registry/types.rs:78–87` 同改 InputSource，不保留 Arc 原文旁路 |

消费者检索覆盖实施根受版本管理规则约束的 Rust 文件，结合上表定义与具名调用结果；它是冻结待实施的静态迁移清单，不是跨模块编译证明。新增目标调用、宏展开及库外消费者由 §7 分阶段编译交付与既有测试校验；签名尚未落地，验证待执行。

### 2.2 规范原文、文件基线与复核限度

- 规模原文：`docs/standards/index.md:39–43` 的 STD-SIZE-001 规定“单个文件最多 1000 行，含空行和注释”，Scope 为源码/测试，不含 Cargo.lock。实测 `wc -l`：assemble=983、Cargo.lock=7434、client config=136、client=196、client_export=145、batcher=214、controller session=158、session_like=20、generation=234、observability=185（wc 计换行；Read 显示的末尾空行可能多一）。未跑全库规模扫描，不能宣称全库通过；生成锁按已裁决 D7 豁免源码行数上限，由 Cargo 正常更新并通过 `--locked` 一致性校验。
- 测试原文：`docs/standards/testing.md:16–41` 要求 ≥30 行测试独立 `_test.rs`、生产模块显式挂载，0 tests 不算成功；`:190–200` 要求临时目录/可控时钟及跨进程真实生命周期；`:206–208,262–264` 要求局部 fixture、禁止 mockall。§8 只列待执行验证，不冒充已通过。
- 错误/阻塞原文：`docs/standards/rust.md:11–27` 要求 thiserror 结构化错误、tracing、阻塞 IO 不堵 async runtime、guard 不跨 await。`architecture-contracts.md:124–129` 明确“turn 仅 flush，Batcher 是唯一 worker/join owner”、未确认关闭保持 Incomplete 及可重入 owner；本次无产品运行证明。
- 依赖事实：`langfuse-client/Cargo.toml:6–23` 仅有 peri-time 等基础依赖；`peri-acp/Cargo.toml:37,62–67` 的 client 目前仅 dev 依赖。目标保持 `ACP 装配 → controller/client 端口 → native adapter`，controller 不获得宿主路径/文件 IO；未执行 layer-imports/WASM 闭包检查，不把 manifest 观察等同全部依赖合规。
- 环境/文档原文：`environment-variables.md:8–9,55–67` 要求变更同步唯一登记表；`documentation.md:27–31` 要求单一事实源。`docs/code-index/{langfuse-client,peri-controller,peri-config}.md` 的入口已按相关条目核实；契约冻结待实施，产品实施时仍由 P5 同步路由，本轮不改这些文件。
- 安全复核限度：`architecture-contracts.md:180–184` 的 ARC-SECRET-001 要求保留原始诊断，不等于本任务允许输出秘密；本次保留 §3/§7 不输出凭据、payload、服务端正文的明确约束，以及结构化错误和原因链，不据规范引用宣称源码已合规。输入不含路径、删除字段不加 shim、显式报错、环境键登记、非 exactly-once 与 D6 反例均保留。

## 3. 冻结接口

### 3.1 配置与校验（公共接口文件，P1 独占）

- `peri-config/src/observability.rs` 保留旧字段；新增的 **pub 字段、类型、默认值和 ENVIRONMENT_KEYS** 逐项以 §6 为完整定义。`pub enum LangfuseCompression { Gzip, None }`，derive `Debug, Clone, Copy, PartialEq, Eq`，默认 Gzip。
- 同文件签名改为 `pub fn resolve(settings: &serde_json::Value, environment: &BTreeMap<String,String>) -> Result<LangfuseConfig, LangfuseConfigError>`；`pub fn load_with_settings(settings: &serde_json::Value) -> Result<Self, LangfuseConfigError>`；`pub fn from_env() -> Result<Option<Self>, LangfuseConfigError>`，全为同步。
- `pub enum LangfuseConfigError { Invalid { key: &'static str, origin: ConfigValueSource, reason: &'static str } }`；`pub enum ConfigValueSource { Settings, Environment }`；thiserror、Send+Sync，不携带原配置值。`assembly.rs:191` 映射至既有 `ConfigurationError::InvalidInput`（`system.rs:118–122`），使用实际来源，失败不发布新 snapshot；不新增旁路 OS env reader。
- `langfuse-client/src/config.rs` 新增 `pub struct TelemetryConfig`：字段与 §6 同名，compression 类型为该模块 `pub enum Compression { Gzip, None }`；TTL/shutdown 分别用 `pub spool_ttl: Duration`、`pub shutdown_grace: Duration` 取代 `_secs`，其余类型不变。`pub fn validate(&self) -> Result<(), LangfuseError>`；配置值在 session 只逐字段转换，不再解析来源。
- `ClientConfig` 新增 `pub telemetry: TelemetryConfig`；`from_env` 不读取新环境键，仅填默认 telemetry，现有独立库入口保留。`BatcherConfig` 新增 `pub telemetry: TelemetryConfig`，默认 event 改 4 MiB；`from_client` 复制 telemetry；旧 `max_retries` 字段删除，执行权仍由 client 独占，所有字面量同步删除，不加 shim。
- `ExportConfig` 原五字段不变，新增 `pub compression: Compression`（Gzip）、`pub max_wire_bytes: usize`（4 MiB）；`max_request_bytes` 定义收敛为最终解压 JSON 上限，direct ingest 的既有事件预检继续执行。`validate` 保留原可见性和返回类型，追加非零 wire 校验。
- `BatcherConfig::validate` 保留三条旧规则及可见性，追加 telemetry 校验；原文 event≤batch/queue 不放宽；新增容量须非零且不超 Semaphore 范围，所有乘加 checked；threshold、chunk≤memory budget；逻辑请求上限可以小于阈值（先触发拒收，不要求测试放大原有限额）；TTL/grace 非零、时间加法不溢出。验证并**实际隔离保留** `prepare_max_in_flight × 编码工作区 + max_in_flight × HTTP工作区 + 一页索引 + 固定控制表`，只有总额减去该值后的 retained 分区可用于 source/event/payload；仅算出最小值不算保留，详见 §4；失败 `LangfuseError::Config`，禁止向上取整扩大配置。

### 3.2 文件端口与句柄（`langfuse-client/src/spool.rs`、`spool/types.rs`）

```rust
pub type SpoolResult<T> = Result<T, SpoolError>;
pub type SpoolReader = Box<dyn std::io::Read + Send>;
pub struct ObjectId([u8; 16]);
pub struct RouteFingerprint([u8; 32]);
pub struct PageCursor(Vec<u8>);
pub enum ObjectKind { Input, Export }
pub enum SpoolState { Writing, Ready, Sending, RetryWait, Delivered, TerminalFailed }
pub struct ObjectMeta {
    pub version: u16, pub id: ObjectId, pub kind: ObjectKind,
    pub route: RouteFingerprint, pub event_count: u64,
    pub encoding: crate::config::Compression,
    pub raw_len: u64, pub stored_len: u64, pub sha256: [u8; 32],
    pub accepted_at_ms: i64, pub expires_at_ms: i64,
    pub attempts: u64, pub next_attempt_at_ms: Option<i64>, pub state: SpoolState,
}
pub struct SpoolPage { pub entries: Vec<ObjectMeta>, pub next: Option<PageCursor> }
pub struct SpoolUsage { pub reserved_bytes: u64, pub stored_bytes: u64, pub objects: u64 }
pub trait QuotaReservation: Send { fn bytes(&self) -> u64; }
pub trait TemporaryObject: std::io::Write + Send {
    fn id(&self) -> ObjectId;
    fn written_len(&self) -> u64;
    fn publish(self: Box<Self>, meta: ObjectMeta) -> SpoolResult<SpoolObject>;
    fn abort(self: Box<Self>) -> SpoolResult<()>;
}
pub trait ObjectClaim: Send {
    fn meta(&self) -> &ObjectMeta;
    fn open(&self) -> SpoolResult<SpoolReader>;
    fn checkpoint(&mut self, meta: ObjectMeta) -> SpoolResult<()>;
    fn delete(&mut self) -> SpoolResult<()>;
}
pub trait SpoolStore: sealed::Sealed + Send + Sync {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>>;
    fn create(&self, quota: Box<dyn QuotaReservation>, kind: ObjectKind,
        cleanup: CleanupRegistration) -> SpoolResult<Box<dyn TemporaryObject>>;
    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta>;
    fn open(&self, id: ObjectId) -> SpoolResult<SpoolReader>;
    fn list(&self, after: Option<PageCursor>, limit: usize) -> SpoolResult<SpoolPage>;
    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>>;
    fn delete(&self, id: ObjectId) -> SpoolResult<()>;
    fn usage(&self) -> SpoolResult<SpoolUsage>;
}
```

- 所有上述 IO 方法 **同步且可能阻塞**，仅在 Batcher 所属、有并发上限的 blocking job 中执行；controller/async worker 不直接调用。此选择使 serde 的同步 Write 流式输出不需整包转桥。HTTP 读泵由发送子任务拥有并 join，不使用脱离 owner 的 adapter spawn。
- `SpoolObject` 在 `spool/types.rs`：`pub struct SpoolObject { pub(crate) store: Arc<dyn SpoolStore>, pub(crate) meta: ObjectMeta, pub(crate) claim: std::sync::Mutex<Option<Box<dyn ObjectClaim>>>, pub(crate) cleanup: CleanupRegistration }`；只公开同步无 IO 的 `pub fn meta(&self) -> &ObjectMeta`。Send+Sync；不是宿主路径，不公开构造；ObjectId/RouteFingerprint 为 Copy+Eq+Hash，其他句柄 non-Clone，metadata 可 Clone；正文不可变。publish 将临时写锁无缝转成交付句柄的 claim，恢复也先 claim 才构造对象；活跃 Input 不会被另一进程误判孤儿。进入 retry_wait **不释放 claim**：活跃 owner 持有至持久终态与本地前缀账本登记完成，或 §5 的关闭显式移交；本轮不支持存活调度器之间接管。删除前从 Option 移出 claim，转交原 CleanupSlot 的清理 owner；有界 blocking job 仅借用 `&mut dyn ObjectClaim` 调 delete，不消费 claim，禁止持 std MutexGuard 跨 await。
- 删除协议：清理 owner 始终拥有同一个 `Box<dyn ObjectClaim>`；blocking job 返回结果时一并归还该 claim，等待取消也须收集并 join，不能提前 drop。delete 失败不解锁，claim 归回原 CleanupSlot，slot/quota/注册及 cleanup_pending_objects 均保留，另一进程仍 claim=None；原 owner 可用同一 claim 重试幂等删除。delete 成功只确认删除完成，随后由清理 owner 释放 claim 并调用 `disarm_after_delete` 恰好一次回收注册、slot 与已写 quota；锁资源的释放不得早于删除确认。
- `spool/error.rs`：`pub enum SpoolError { Unavailable, QuotaExceeded { requested: u64, available: u64 }, Busy, NotFound, Corrupt { id: ObjectId }, InvalidMetadata, Io { operation: &'static str, source: std::io::Error } }`，thiserror、Send+Sync。`LangfuseError` 加 `Spool(#[from] SpoolError)`、`MemoryBudgetExceeded { requested: usize, available: usize }`、`SingleSpanTooLarge { limit_bytes: usize, encoded_bytes: u64 }`；保留旧变体。
- reserve 持有 token；create 消费并绑定 domain/id；writer 禁止写超 reservation，publish 核对实测 len/hash，未用额度返还。reservation Drop 只回收未写预留；写过的字节必须确认删除后返还，不能凭 Rust Drop 声称磁盘已清理。claim=None 表示别的 owner 持锁，NotFound 是显式错误；delete 幂等，忙对象返回 Busy。同步 open 从 0 读，仅可读完整发布对象。
- `spool/native.rs`：`pub struct NativeSpool` 私有字段；`pub fn open_default(route: RouteFingerprint, quota_bytes: u64) -> SpoolResult<Self>`、`pub fn open_at(root: std::path::PathBuf, route: RouteFingerprint, quota_bytes: u64) -> SpoolResult<Self>`；仅 native `cfg(all(not(target_arch="wasm32"), not(target_os="emscripten")))` 导出。路径只能在适配器/ACP 装配层出现；私有应用数据目录缺失时显式 Unavailable，不回退 cwd。能力探针（fsync + advisory lock）只判能力：对端**持有** domain 锁属 §5.2 的跨进程竞争，不升级为 `Unavailable`（收敛轮二修正）；「对端持锁」的错误形态由私有 `is_lock_contention` 统一判定——Unix 为 `WouldBlock`，Windows 的 `LockFileEx` 报 `ERROR_LOCK_VIOLATION`(33) 且当前 Rust 不映射为 `WouldBlock`（收敛轮三续修正，`probe_durable_io` 与 `try_lock_file` 共用同一判据）；`try_lock_file` 把「条目目录已被并发删除」单独报为不可领取，不与其他 owner 持锁混同——判定只看**本次 `open` 的错误**（`ENOENT`；macOS 另有实测的 `EINVAL`；Windows 另有实测的 `ERROR_ACCESS_DENIED`(5)——删除中的目录 / 条目形态，收敛轮五），不做事后探测父目录（收敛轮四：探测与失败之间的 TOCTOU 曾把 Linux CI 上的 `ENOENT` 升级为整页扫描失败）。
- 默认目录由 native adapter 获取本机私有应用数据根并追加 `peri/telemetry-spool/v1/<route>`；测试只用 open_at 临时目录。部署 route 使用规范化完整 OTLP endpoint、public key、secret 的长度分帧 SHA-256；不存原凭据，不记录 route 摘要，轮换任一凭据隔离旧对象；拒绝 URL userinfo/query/fragment，保留合法 path 前缀，scheme/host/默认 port 正规化、去尾斜杠。

### 3.3 快照、事件与 payload（`langfuse-client/src/payload.rs`、`snapshot.rs`）

```rust
pub struct BudgetedBytes { pub(crate) bytes: bytes::Bytes, pub(crate) permit: MemoryPermit }
pub enum InputSnapshot { Inline(BudgetedBytes), Spooled(SpoolObject) }
pub enum PayloadBody { Memory(BudgetedBytes), File(SpoolObject) }
pub struct ExportPayload {
    pub(crate) body: PayloadBody, pub encoding: Compression,
    pub raw_len: u64, pub wire_len: u64, pub sha256: [u8; 32], pub event_count: u64,
}
pub trait SnapshotSource: Send + Sync + 'static {
    fn retained_bytes(&self) -> Result<usize, LangfuseError>;
    fn write_json(&self, out: &mut dyn std::io::Write) -> Result<(), LangfuseError>;
}
pub struct SnapshotTicket { pub(crate) cell: Arc<SnapshotCell> }
pub(crate) struct SnapshotCell {
    pub(crate) budget: Arc<MemoryBudget>,
    pub(crate) prepare_seq: u64,
    pub(crate) state: std::sync::Mutex<SnapshotState>,
    pub(crate) cancelled: std::sync::atomic::AtomicBool,
    pub(crate) changed: tokio::sync::Notify,
}
pub(crate) enum SnapshotState {
    Preparing,
    Ready(InputSnapshot),
    Failed(LangfuseError),
    Taken,
}
pub enum InputSource { Ready(InputSnapshot), Preparing(SnapshotTicket) }
pub struct TelemetryEvent {
    pub(crate) event: IngestionEvent,
    pub(crate) input: Option<InputSource>, pub(crate) output: Option<InputSource>,
    pub(crate) permit: MemoryPermit,
}
```

- 类型均 Send+Sync；MemoryPermit/预算字节/source/ticket non-Clone；SnapshotSource 可由 `Arc<Value>` 或 controller 的 messages/tools 借用序列化适配器实现，但适配器不依赖文件系统。InputSnapshot 存 `serde_json` 紧凑 JSON 字节，非 OTLP 转义后的字符串；不存在把 file ID 塞进 wire 的变体。
- `snapshot.rs`：`pub fn snapshot_value(value: &Arc<serde_json::Value>) -> Arc<dyn SnapshotSource>` 只借用/克隆 Arc；调用方在跨事件保留该 Arc 前必须调用下列 freeze 准入。`Batcher::try_freeze(&self, source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError>` 同步计费并登记准备任务，IO 随 worker 执行；失败立即释放，不“先缓存再补票”。
- `TelemetryEvent::try_new(event: IngestionEvent, input: Option<InputSource>, output: Option<InputSource>, budget: &Arc<MemoryBudget>) -> Result<Self, LangfuseError>` 为同步 pub 构造；替代来源对应 body 字段必须 None，否则 Config 错误防止双份内容；无替代来源的现有 body 值按借用视图流式编码。保留原 IngestionEvent 定义和 serde wire，不添内部引用变体。
- `Batcher::try_add`/`add` 保留接受 IngestionEvent 的签名，但立即进入同一 TelemetryEvent 准入实现；新增 `pub fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError>`。两种入口不是双编码路径或兼容 shim：分别服务直接事件与提前冻结的真实用例。
- `try_new` 校验 event permit、input/output 的 Inline permit、Spooled 清理注册及 Preparing cell 的预算身份全部 `Arc::ptr_eq` 指向传入预算，`try_add_prepared` 再与 Batcher 部署预算逐项核对；错误为新增 `LangfuseError::BudgetMismatch`，不携带地址/正文。仅金额相同不算同一预算，不允许重新挂账洗掉来源身份。SnapshotCell、CleanupRegistry 与各 permit 均不可伪造地保存 `Arc<MemoryBudget>`；公有 enum 组合也必须经过校验，构造失败按原 owner 清理。单独 `MemoryBudget::new` 构造的事件不具备另一部署准入资格。
- tracker 与 gate 仅保存 InputSource；请求体到达替换 fallback 并释放原引用；结束/取消移动 ticket 而不是等 IO，也不克隆原文。worker 的有界 slot 表持准备状态/清理责任，ticket Drop 只置取消位并 Notify；不能在 Drop spawn 或积累无界清理消息。
- input 在最终 payload 完成（磁盘路径须原子发布）后释放；payload 保持到 HTTP 终态及清理。若编码失败可用原 snapshot 拆批重试；进程崩溃只剩 input 不合成 generation；Inline 永不冒充持久接受。

- `ExportPayload` 追加 `pub(crate) budget: Arc<MemoryBudget>`，由 encoder 绑定部署同一预算，HTTP owner 从此预约工作区；不能在 client 内偷偷新建另一份 32 MiB。直接 ingest 拥有调用期预算，多个直接调用不属于共享 Batcher 的部署承诺。
- 资源端口本轮明确**仅支持 langfuse-client crate 内可替换实现**（native、模块内测试 fake），外部 crate 只能消费/注入已构造 capability，不支持自行 implement SpoolStore；`spool.rs` 定义 `pub(crate) mod sealed { pub trait Sealed {} }`，并重导出 CleanupRegistration。外部实现是未授权扩展，不由并行流自行开放；若控制器选择外部实现，须补受控构造契约后重新复审。
- crate 内构造协议：`ObjectId::from_bytes([u8;16]) -> Self`、`RouteFingerprint::from_bytes([u8;32]) -> Self`、`PageCursor::try_from_bytes(Vec<u8>) -> SpoolResult<Self>` 均 pub(crate)、同步，cursor 限 4 KiB；对应 `as_bytes(&self)` 以同样可见性返回固定数组引用/字节 slice。`SpoolObject::from_claim(store: Arc<dyn SpoolStore>, claim: Box<dyn ObjectClaim>, cleanup: CleanupRegistration) -> SpoolResult<Self>` 为 pub(crate)，publish/恢复必须经此入口验证同 store/domain/id/预算绑定并保存注册；失败交原槽清理，不裸放字段构造。子模块/兄弟模块可用这些入口，crate 外无法伪造 ID/对象/注册；公开元数据用于只读消费，不构成构造许可。
- `spool/types.rs` 的 ObjectId/RouteFingerprint 私有字节只能由上述适配器/路由构造器生成；`pub fn route_fingerprint(endpoint: &str, public_key: &str, secret_key: &str) -> Result<RouteFingerprint, LangfuseError>` 放 `spool.rs`，同步且不记录参数。`ObjectMeta` 的 version/kind/encoding/state/期限/长度必须由 publish 验证，而不是信任调用者 descriptor。
- 内存 Inline 不含异步销毁；Spooled 句柄最终 drop 只登记已预分配 slot 的待清理状态，实际 delete 由 worker 执行，完整注册协议如下。`MemoryPermit` 不可 Clone；HTTP 共享内存正文时将整个 `BudgetedBytes` 包在私有 Arc 内，保持 permit 与 Bytes 同寿，不能只 clone Bytes 后提前归还 permit。

```rust
// spool/cleanup.rs；只由 crate 内 owner 构造，Drop 不做 IO/分配/spawn
pub struct CleanupRegistration { pub(crate) slot: Arc<CleanupSlot> }
pub(crate) struct CleanupRegistry { /* 固定容量槽表、Notify、预算身份 */ }
pub(crate) struct CleanupSlot { /* store/id/quota/claim、状态与关联序号 */ }
impl CleanupRegistry {
    pub(crate) fn try_register(&self) -> SpoolResult<CleanupRegistration>;
    pub(crate) async fn drain(&mut self) -> SpoolResult<()>;
}
impl CleanupRegistration {
    pub(crate) fn abandon(&mut self, claim: Option<Box<dyn ObjectClaim>>);
    pub(crate) fn disarm_after_delete(self);
    pub(crate) fn disarm_after_handoff(self);
}
```

- slot 在任何 create/恢复 claim 前分配，注册表由唯一 Batcher worker 持有；registry 预先拥有 slot 的强引用，句柄只持注册，不反向持 worker。`try_register` 同步无 IO，容量耗尽返回 Busy，在持有 source/准入前拒收；slot 上限为 checked `3×queue_capacity + prepare_max_in_flight + max_in_flight + 64`，表及关联 cell 内存纳入固定账单。所有 Input/Export、临时文件和恢复对象各占一槽；失败产物未清理不会释放槽供无限新建。
- create 在槽内登记 store/id/quota 后才执行第一笔写入；create/publish/abort 消费参数后失败仍由该槽保存清理状态和锁。publish 成功把同一注册与临时写锁转移给 SpoolObject，不另分配槽。SpoolObject Drop 用 abandon 把 claim 移回槽并置 CleanupPending/Notify，不能仅释放 OS 锁造成所有权真空；执行中 claim 在发送/写入子 owner 内，先 join 再归槽。drain 只由 worker 执行有界 blocking delete/abort，确认删除后 disarm_after_delete 才回收已写 quota/slot；未写 reservation 可以直接返还。
- `TelemetryEvent::try_new` 的字段冲突、计量/预算失败，以及 try_add_prepared 的预算身份/队列/关闭拒收，都消费并 drop input/output，必须走原来源所属 registry（不能登记到错误部署）；Preparing ticket 标记取消，worker join 准备任务并清理迟到结果。每个 slot 的同代登记只能清理/释放一次；删除失败保留槽、claim、quota 及 cleanup_pending_objects，不以无界错误队列补洞。
- shutdown 不在外部仍持有登记句柄时销毁 registry：先取消未完成准备，完成/失败结果中的句柄回收或标记失效，外部遗留注册计 active_tasks/cleanup 未决，保持可重入 Pending。正常 Export 移交只能在 §5 账本登记后 disarm_after_handoff，保留磁盘配额而释放本地槽；Drop 永不隐式当作移交。direct ingest 限内存，不创建没有 Batcher 清理 owner 的磁盘对象。
- 内存 sources/output 的大小判定以冻结 JSON 字节为准；正好达到 256 KiB 即转 spool（`>=`），测试阈值 -1/等于/+1；spool 关闭时不转磁盘。

### 3.4 预算 writer 与统一流式编码

- `memory.rs`：`pub struct MemoryBudget` / `pub struct MemoryPermit`（字段私有）；`pub fn new(limit: usize) -> Result<Arc<Self>, LangfuseError>`；`pub fn try_reserve(self: &Arc<Self>, bytes: usize) -> Result<MemoryPermit, LangfuseError>`；`pub fn used(&self) -> usize`；permit Drop 回收，`pub fn bytes(&self) -> usize`。以上同步、Send+Sync。
- `encoding/writer.rs`：`pub(crate) struct ExportContext { pub(crate) route: RouteFingerprint, pub(crate) accepted_at_ms: i64, pub(crate) expires_at_ms: i64 }`；`pub(crate) struct BudgetedWriter`（私有内存前缀、SpoolStore、quota/permit、context、限额计数）；实现 `std::io::Write + Send`；`pub(crate) fn new(config: &TelemetryConfig, budget: Arc<MemoryBudget>, spool: Option<Arc<dyn SpoolStore>>, context: ExportContext, max_wire_bytes: usize, cleanup: CleanupRegistration) -> Result<Self, LangfuseError>`；`pub(crate) fn finish(self, raw_len: u64, encoding: Compression, event_count: u64) -> Result<ExportPayload, LangfuseError>`；失败产物交 worker 清理，不能在 Drop 偷起任务。转磁盘前一次预留 max_wire_bytes 加 descriptor/原子更新空间，写入不允许越 reservation；无须不明确的隐式扩容。
- `client_export.rs`：`pub(crate) fn encode(events: &[TelemetryEvent], out: BudgetedWriter, config: &ExportConfig) -> Result<ExportPayload, LangfuseError>` 同步；同文件 `encode_none` / `encode_gzip` 使用同一签名并由 encode 分派；只允许已就绪 sources，Preparing 由 worker 先解析。gzip 固定 level=6、mtime=0，不加自定义 wire 扩展。
- `encoding/mod.rs`：`pub(crate) fn write_otlp(events: &[TelemetryEvent], out: &mut dyn std::io::Write) -> Result<u64, LangfuseError>`；`pub(crate) fn measure_otlp(events: &[TelemetryEvent], limit: usize) -> Result<u64, LangfuseError>`；同步。计量与写入使用同一映射 emitter，只替换 sink；包含信封/逗号/转义。不构造整棵 Vec<OtelSpan>，不对大 input/output/metadata 调 `to_string`。
- `encoding/json_string.rs`：`pub(crate) fn write_json_string(reader: &mut dyn std::io::Read, out: &mut dyn std::io::Write, chunk: &mut [u8]) -> Result<(), LangfuseError>`；将冻结 JSON 字节嵌入 OTLP stringValue，跨块 UTF-8 与 JSON 转义一致；none/gzip、inline/file 共用此实现。
- `stable_json.rs`（CI 修复追加）：`pub(crate) struct StableJson<'a>(pub(crate) &'a serde_json::Value)`、`pub(crate) struct StableMap<'a, V: Stable>(pub(crate) &'a HashMap<String, V>)`、`pub(crate) trait Stable`（`Value` 递归按键排序，`i32`/`f64` 透传）。凡是进入 OTLP 正文或冻结正文的 JSON——内联 input/output、metadata（整份与逐字段展开）、model_parameters/usage/cost_details、usage_details 展开、冻结快照字节——一律经本模块输出：对象键固定字典序、map 按键排序，与编译期 `serde_json/preserve_order` 是否启用无关（workspace 全量构建经 `agent-client-protocol` 启用该 feature，单 crate 构建不启用）。跨 crate 的冻结来源经 `snapshot.rs` 的 `pub fn write_stable_json<W: io::Write + ?Sized>(out: &mut W, value: &serde_json::Value) -> Result<(), LangfuseError>` 复用同一口径（`ValueSource::write_json` 与 controller 的 messages/tools 借用适配器都调用它），键序规则只有一份实现。
- `types/conversion/*` 只维护一套 ID/时间/字段映射，改为借用值/来源视图输出；旧 `ingestion_events_to_otel` 如测试需物化，只能收集同一 emitter，不保留另一份业务分支。原始事件上限、最终 raw 上限、wire 上限独立 writer；gzip 收尾/trailer 计入 wire；没有先压缩成整 Vec 再写盘。

### 3.5 HTTP 请求与重试

```rust
// langfuse-client/src/client.rs；枚举位于 client_send.rs，pub(crate)
pub(crate) enum SendOutcome {
    Delivered,
    Retryable { error: LangfuseError, retry_after: std::time::Duration, attempts: u64 },
    Terminal { error: LangfuseError, attempts: u64 },
    CheckpointFailed { error: LangfuseError, attempts: u64 },
}
// 以下是 LangfuseClient 的方法；future 为 Send，不要求 Sync
pub(crate) async fn send_payload(&self, payload: &ExportPayload,
    attempts: &mut dyn AttemptCheckpoint) -> SendOutcome;
// client_send.rs；由发送 owner 实现，只提供 checkpoint，不参与重试分类
pub(crate) trait AttemptCheckpoint: Send {
    fn before_attempt(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<
        Output = Result<u64, LangfuseError>> + Send + '_>>;
}
pub(crate) async fn build_request(&self, payload: &ExportPayload,
    body_owner: &mut BodyTaskOwner) -> Result<reqwest::Request, LangfuseError>;
```

- `client_send.rs` 的 `pub(crate) struct BodyTaskOwner` 私有字段，持固定上限读泵 join、取消标志和字节 permits；`pub(crate) async fn drain(&mut self) -> Result<(), LangfuseError>`。文件读泵 blocking job 每次最多一块，桥接 channel 容量 2；worker 持整个 HTTP future，退出前停止读泵并 drain。内存正文克隆 Bytes 共享底层 permit，不重新编码。
- 每次 attempt（含周期内所有 retry）严格执行：drain 上次 BodyTaskOwner → 检查周期期限/TTL/取消 → await `AttemptCheckpoint::before_attempt` → 从 byte 0 open 并 build_request → HTTP execute → 完整响应解析 → drain 读泵。checkpoint checked_add 持久 attempts、state=Sending 且清除 next_attempt_at，fsync/原子替换成功才返回累计 attempts；统计次数是预先登记的尝试，允许 checkpoint 后未发出即崩溃的保守高估，不允许实际 HTTP 少记。Authorization/Content-Type/x-langfuse-ingestion-version 同基线。gzip 才有 `Content-Encoding: gzip`；none 无此头；两者 `Content-Length=wire_len`，禁止同时 Transfer-Encoding。
- 文件路径的 `AttemptCheckpoint` 实现归 P2b 的发送 owner：发送任务独占从 SpoolObject 取出的 claim，blocking job 调 `ObjectClaim::checkpoint` 后把同一 claim 返还；job 被 await 取消时仍由 owner 收集并 join，不在 async runtime 阻塞或持 MutexGuard 跨 await。每次成功 checkpoint 后 BodyTaskOwner 才能读此 claim 的正文。内存路径使用同一接口维护 checked 累计次数，无磁盘 checkpoint；direct ingest 不得把 File 交给无持久实现。
- checkpoint 失败返回独立 `SendOutcome::CheckpointFailed { error: LangfuseError, attempts: u64 }`（加入上方枚举），不得 build/open/execute 本次请求，也不冒充 HTTP Terminal/Delivered；worker 保留 claim、原 payload 与持久状态，记录可见错误并返回 Pending/DeliveryIncomplete，修复持久状态前禁止继续发送。单次周期的 attempts 字段统一为累计持久水位（内存为累计登记水位），worker 不再次相加。checkpoint 卡住按 D6 留 owner；成功响应后的终态 checkpoint 失败也不删除对象、不报告 Complete。
- `client_retry.rs`、`client_response.rs` 分类/解析保留；max_retries/60s 周期不变；周期仅因可恢复错误耗尽才返回 Retryable。PartialSuccess、200 无效/过大/截断响应、永久 HTTP 错误 Terminal，不整批盲重试。worker 只调度 Retryable，不重复判断状态码。
- direct `pub async fn ingest(&self, events: Vec<IngestionEvent>) -> Result<(), LangfuseError>` 不变，使用同一编码/HTTP实现但无持久 owner；Retryable 映射原发送错误。`ingest_with_limit` 不再由 worker 当作文件发送路径；删除旧 request.try_clone 重试循环，测试改验证“只编码一次、请求重建、正文相同”。

### 3.6 队列、结果及关闭贯穿

```rust
// batcher.rs：私有命令；event_seq/prepare_seq 在同一 admission 锁内各自单调递增
struct QueuedEvent { seq: u64, event: TelemetryEvent, accepted_at_ms: i64 }
struct FlushBoundary { event_seq: u64, prepare_seq: u64 }
enum BatcherCommand {
    Add(QueuedEvent),
    Flush { boundary: FlushBoundary, reply: tokio::sync::oneshot::Sender<FlushSnapshot> },
}
// batcher/result.rs：公开，derive Debug/Clone/Copy/PartialEq/Eq
pub struct DeliveryCounts {
    pub pending_events: u64, pub persisted_pending_events: u64,
    pub failed_events: u64, pub cleanup_pending_objects: u64, pub active_tasks: u64,
}
pub enum FlushReport { Complete }
pub enum ShutdownReport { Complete }
// error.rs 新增；错误并非 HTTP 成功，也不能被 is_ok 当成完成
// LangfuseError::Pending { counts: DeliveryCounts }
// LangfuseError::PersistedPending { counts: DeliveryCounts }
// LangfuseError::DeliveryIncomplete { counts: DeliveryCounts }
// Batcher 的公开异步方法；future: Send
pub async fn flush(&self) -> Result<FlushReport, LangfuseError>;
pub async fn shutdown(&self) -> Result<ShutdownReport, LangfuseError>;
```

- Pending=仍待交付，至少一条未持久完成，或 owner 尚未排空；要求 `pending_events>0 || active_tasks>0`。PersistedPending=所有待交付事件均在可恢复 Export 中、pending>0 且 active_tasks=0；failed/cleanup 可同时非零，不能优先返回一个错误而丢掉其他计数。DeliveryIncomplete 用于 pending=0、active_tasks=0 但有新增失败/待清理；保留 WorkerJoinFailed。旧仅 HTTP 失败 `IngestionApi` 摘要语义和累计计数继续回归验证。
- 计数单位一律事件（不是 input 文件/批次数），持久数量是 pending 的子集；一次批次包含 n 个事件计 n；部分拒收因服务端不提供逐条身份，failed_events 计该批无法确认整体交付的事件，另保留 rejected_spans，禁止重发整批。
- FlushSnapshot 私有保存 FlushBoundary 与 DeliveryCounts；每次 flush 只覆盖捕获前缀，pending 每次重算、不因观察清零；失败增量确认与 shutdown 累计账本保持分离。try_freeze 在同一 admission 锁内完成 Open 检查、全部预算/槽预约、分配 prepare_seq、登记准备任务后才返回 ticket，这是 freeze 线性化点；拒收不留下半登记。try_add_prepared 成功才分配 event_seq，记录输入/输出的 prepare_seq 依赖，不把准备序号改写成事件序号。seq checked 溢出显式拒收。
- flush 等到命令槽后，在 admission 锁内原子捕获两个当前水位并入队，未入队的等待者不建立前缀保护。它等待 `event_seq≤E` 的发送结果/重试周期结果及依赖准备，另等待 `prepare_seq≤P` 的无终态事件快照准备收敛；只等 Ready/Failed/取消后 join 与必要清理，不等未来 generation end，也不虚增 pending_events。无事件准备失败计入独立准备失败账本并返回 `LangfuseError::SnapshotPreparationFailed { failed_snapshots: u64, counts: DeliveryCounts }`，不能伪造 failed_events；session 以 DeliveryIncomplete 旁路报告，不落 UnexpectedFailure。
- 后缀 `prepare_seq>P` 的 freeze、后缀 end/Add 不加入本次边界；即使引用同一前缀快照也不让前缀 flush 等待后缀发送。Ready/Taken 的准备完成事实由有界 slot/水位保留至所有覆盖屏障确认，不能因 ticket 移交而丢失；受保护准备不能被后缀取消/驱逐未 join 就视为完成。准备调度按序公平，后缀不占满 worker 使前缀饥饿；最多保存有界未决槽与完成区间，禁止每次 freeze 永久追加历史表。
- `SessionLike` 新增 `fn try_freeze(&self, source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError>`、`fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError>`、`fn memory_budget(&self) -> &Arc<MemoryBudget>`；原 try_add 保留；flush 改 `fn flush(&self) -> Pin<Box<dyn Future<Output=Result<FlushReport, LangfuseError>> + Send + '_>>`。无 shutdown 权限。
- `Batcher::try_new(client: LangfuseClient, config: BatcherConfig, spool: Option<Arc<dyn SpoolStore>>) -> Result<Self, LangfuseError>`；new 同加 spool 参数，内部唯一 worker/预算。controller `LangfuseSession::{new,new_owned}` 同加 `spool: Option<Arc<dyn SpoolStore>>`，原 async/Option 返回不变；运行期配置错误须诊断，不能默默 None。
- `LangfuseShutdownReport` 保留四旧变体，新增 `Pending { counts: DeliveryCounts }` / `PersistedPending { counts: DeliveryCounts }` / `DeliveryIncomplete { counts: DeliveryCounts }`；session.rs:68–79 显式映射，不能落 UnexpectedFailure。`tracer/turn.rs:120,182` 记录结构化 counts，不打印 payload。
- ACP 精确迁移点（本轮已读取真实源码）：`peri-acp/src/host/lifecycle.rs` 的 `HostExitContext::finish`（`:153–189`，遥测映射 `:172–175`）保持只有 Complete 映为宿主 Complete，其他包括 Pending 均 `TelemetryFailed(report)`；同时必须修改 `AcpHostHandle::shutdown`（`:67–102`）的结果分支 `:85–91`。该分支当前只对外层 Incomplete 保存 `self.retry = Some(context)`；目标须把仍持任务/清理 owner 的 `TelemetryFailed(LangfuseShutdownReport::Pending { .. })` 同样保存到 retry，且不写 `self.terminal`，使下一次 shutdown 经 `:71–76` 再次执行同一 context.finish。遥测 pending 类结果中，只有已按 §5 完成移交、无本地任务/清理 owner 的 PersistedPending 才可缓存终态，不能仅凭枚举名称或 pending 计数判断；其他既有终态规则不变。部署资源不在 Batcher join 前释放。`host/assemble.rs:911–919` 移为 `host/telemetry.rs` 装配函数；`host/mod.rs:243` 保留 non-Clone owner。TUI deployment.rs:51–55 保留旁路处理，新增测试断言 Pending 不把成功 prompt 变错误。

- `Batcher::try_new` 在启动 worker 前校验 client ExportConfig 与 BatcherConfig.telemetry 的 compression/max_wire_bytes 一致；session 从一份 peri-config projection 映射，不能由两个消费者重解释。公开配置独立构造产生冲突时返回 Config，而不是任取一个。
- `batcher/result.rs` 经 `batcher.rs`/`lib.rs` 重导出 FlushReport、ShutdownReport、DeliveryCounts；`lib.rs` 公开 `spool`、`payload`、`snapshot`、`memory` 模块，encoding 保持 crate-private。`Batcher::memory_budget(&self) -> &Arc<MemoryBudget>` 为 pub 同步 getter，供 SessionLike 透传，不允许外部替换预算。
- worker 为准备任务保留独立进度分支，不能被满 HTTP JoinSet 或等待 Preparing 的编码任务堵死；完成快照通知不回调 tracer 锁，只更新 cell。原始请求到达但冻结被拒绝时计数该 generation 拒收，不能偷偷上报旧 fallback 冒充实际请求。
- shutdown 在 grace 内等待同一 WorkerOwner；超时只返回计数快照而不 `take()` handle，后台仍由现有 owner 持有并最终 join；宿主遇到 active_tasks>0 的 Pending 保持可重入关闭状态，不把关闭事务缓存成不可重试的终态。native 工厂初始化由 ACP 已有 host task scope 拥有并等待，不产生第二个发送器；未取得 capability 之前无 HTTP/恢复任务。

## 4. 数据流与内存所有权

| 阶段/owner | 字节计费与预留时点 | 释放点/有界性 |
| --- | --- | --- |
| 接收借用，tracker/gate | 克隆/跨事件保留前计算 retained 上界并 reserve；String/Vec 按 capacity，Value 递归计容器/节点/键值，Arc 即使与业务共享也按全部保守计一次；不按 JSON 长度冒充 heap | raw 替代 fallback、取消、snapshot 发布后释放对应 Arc 和 permit；业务仍持有时不宣称进程内存已释放 |
| source sizing | `retained_bytes` 必须是无大分配遍历；源实现给出包含其树容量的保守上界；未知动态分配量拒绝而非记 0 | source 适配器只克隆已存在 Arc，不 `json!` 构造整树；messages/tools 用 Serialize 借用适配 |
| 冻结/编码 worker | 准备并发默认 1；预留输入 retained + 输出前缀 capacity + 读写各 64 KiB + gzip 状态固定账单 1 MiB/encoder；实际 backend 分配超过 1 MiB 则停止验收、调整账单而非漏计 | 写盘成功即替换来源；正在准备和等待准备的 source 共用 32 MiB，不以等待 permit 的任务列表存大对象 |
| 输出与队列 | 内存前缀最大 256 KiB（spool 开启）；跨阈值先 reserve 磁盘并写完前缀才释放内存；grow 先拿增量 permit；event queue 另保留 16 MiB 原文限制，文件引用按描述内存计费而非文件原文 | Inline/file 均占队列槽；对象描述、slot/cell、计数容器容量均计 `size_of + heap capacity`，严禁磁盘全索引载入 |
| HTTP/body owner | 每请求固定预留 1 MiB 框架账单（reqwest/hyper/TLS 用户态缓冲）+ channel 2×64 KiB + 读块 64 KiB + response 64 KiB；共享 Bytes 只计一次 | 最多 2 个发送；每次 retry drain 后才重开；账单须用 allocator instrumentation 校准，若不能证明框架上界，则只可报告受控预算通过，不能报告完整内存验收通过 |
| 恢复/清理 owner | 每页 64 条、metadata 单对象最多 4 KiB；页 buffer 预留 256 KiB，游标有长度上限；一次一页，worker 只维持有界 ready window | 磁盘积压增长不扩展内存索引；清理失败保留磁盘记录和配额，后续分页重试 |

32 MiB 是上述遥测 retained/heap/缓冲与保守账单的总上限，不包括调用前已存在且未被遥测保留的业务数据，也不是 RSS；OS page cache、allocator 碎片、内核 socket 与 JS/Blob 另测。输出内存回退允许超过 256 KiB 阈值但必须先取得全额增量预算及 wire limit；不复制已有磁盘前缀回内存后再尝试预约。

- **进度不变量**：Batcher 构造用 `MemoryBudget::for_batcher(&BatcherConfig, &ExportConfig) -> Result<Arc<MemoryBudget>, LangfuseError>`（pub(crate)，同步）一次预留 P；`P = Nprepare×(2×chunk + 1 MiB gzip账单) + Nhttp×(1 MiB框架账单 + 3×chunk + max_response_bytes) + 256 KiB页 + 固定槽表/调度元数据账单`，全部 checked；要求 `limit>P`。进度分区与 retained 分区不互借，MemoryBudget::try_reserve 只扣 retained；used 包括已隔离 P（不得重复计费），空闲进度额度不能被 source 抢占。公有 new 仅建独立 retained 预算；不是部署构造替代入口。
- worker 内部通过 `try_prepare_workspace(&Arc<MemoryBudget>) -> Result<WorkspaceLease, LangfuseError>` / `try_http_workspace(...) -> Result<WorkspaceLease, LangfuseError>` 取得固定池租约（均 pub(crate)，实现放 memory.rs）；租约 non-Clone，Drop 回池。等待并发槽可以调度，不再等待 retained 额度；没有“持全部 source 等另一批释放工作区”的环。
- freeze 同步准入必须同时预留 source retained、slot/cell 及 snapshot 输出 allowance：spool 可用时按阈值前缀 capacity，纯内存时按有界计量得到的 JSON capacity 上界（受事件逻辑限额）；unknown/超过上界即拒收。Inline 完成把 allowance 转成 BudgetedBytes permit，spool 完成才归还前缀额度；不以工作区租约长期存 ready 数据。事件准入同样预留 payload 输出 allowance（spool 时为阈值前缀，纯内存时为 max_wire_bytes 的容量上界），准备阶段只消耗已预约输出；可保守逐事件预约、聚合共享后归还差额，不在已接受后等待增量内存。
- 内存预约不足始终立即 `MemoryBudgetExceeded`，三种策略一致；Block 只在已完整计费后等待命令槽/队列原文额度，等待者由调用方拥有并可取消。磁盘失败的内存回退仅允许一次非等待预约，失败显式终结该准备/编码并清理，不让已接受任务互等预算。native 写入/HTTP 永久卡住仍按 D6 报 Pending，不把内存进度承诺扩大成 IO 有限完成承诺。
所有阻塞 IO/压缩任务只能由 worker 有界 JoinSet/spawn_blocking 集合持有；HTTP 读泵由其中发送任务持有。关闭/取消不可丢弃 blocking handle；正常路径不跨 tracer 锁 await/IO。Block 准入先计费再等槽，等待者由调用方 future 持有，取消回收；try_freeze 永不等待。活跃快照 slot 数另受 queue_capacity=1024 限制；用于关闭的控制位/Notify 不占数据槽。

## 5. 生命周期状态机、恢复与 quota

### 5.1 准入/驱逐矩阵（Inline、Preparing、Spooled 同一规则）

| 状态/边界 | DropNew | Block | DropOldest |
| --- | --- | --- | --- |
| 新事件所需 retained/输出 allowance/清理槽不足 | 立即拒收并按来源 owner 回收 | 同样立即拒收，不等待内存/清理槽 | 同样立即拒收，不先驱逐受保护或执行中对象来赌预算 |
| 命令槽/队列原文字节满 | try_add/add 拒收 | 仅 async add 可在完整计费后等待；同步 try_add/prepared/freeze 均不等待 | 按 event_seq 从最后已准入 flush 的后缀挑最旧可驱逐 Add，回收足额后新事件追加队尾；候选不足则拒收且不部分驱逐 |
| 未执行准备的 queued Add，来源为 Inline/Ready Input 或等待中的 ticket | 保留旧事件 | 保留旧事件 | 后缀且无任何屏障保护时可驱逐；移出 Add 后取消关联准备、归还 permit；file 交原清理槽，未确认删除不返还磁盘 quota |
| preparing：blocking 写入/编码已开始，或 ticket 已被执行 owner 领取 | 不驱逐 | 不驱逐；worker 继续推进 | 不作为即时回收候选；允许业务取消标记，但必须 join 并清理迟到产物，不能释放后复用正在使用的额度 |
| ready Export 已离开 Add 队列、retry_wait 或 sending/HTTP 读泵在途 | 不驱逐 | 不驱逐 | 均不驱逐（含恢复 window）；其 claim/发送/清理归 worker，不把文件 ID 当廉价可丢槽 |
| 已准入 flush 的 event 前缀、prepare 前缀及依赖；尚未 end 的活跃快照 | 保留 | 保留 | 前缀绝不驱逐；独立活跃快照不是 Add 候选，generation 自行替换/取消走有 owner 的清理 |

DropOldest 的候选判定、足额预检、旧 Add 移除和新 Add 追加须在同一 admission 临界区线性化；实际清理在锁外，旧事件退出集合后不得被 worker 再取走。ready Input 被消费掉时内存/文件/准备注册全部走同一释放协议；已有 snapshot 被屏障保护则该 Add 也不可驱逐。被驱逐事件更新 evicted 及累计丢弃统计，不伪造 Delivered；保留既有“flush 准入前已驱逐项不属于该前缀”的语义。§8 必须断言三策略×各状态及全部资源回收，不能只看队列长度。

### 5.2 持久状态与恢复

| 状态 | 转移和持久化要求 |
| --- | --- |
| writing | 先 quota reserve，临时目录含正文/descriptor；完成 gzip trailer、摘要/长度，fsync 文件与目录，同文件系统 rename 到 ready 再 fsync 父目录。失败不发布，取消清理；临时对象也持 OS 文件锁，扫描不清理其他活跃 writer。 |
| ready | 完整 Export 才算持久接受；claim 拿 OS advisory lock（不是 PID 存活判断），校验 route/version/len/hash/TTL 后 checkpoint sending。Input ready 只是快照，禁止被 HTTP 恢复队列当事件。 |
| sending | 每次请求前持久 attempts；同一 payload 仅一个 claim。Delivered 来自完整 200 校验；可恢复周期耗尽 → retry_wait；永久错误、PartialSuccess、无效响应 → terminal_failed。 |
| retry_wait | 原 accepted_at/expires 不变；保存下一次允许时间（含 Retry-After）；活跃 worker 保持原 claim，到期同一调度器继续发送，其他进程 claim=None。TTL 先到则 terminal_failed；不为每条建 timer/task。 |
| delivered | 先持久终态，再由清理 owner 借用同一 claim 调 `delete(&mut self)` 删除正文/描述；失败归回原 CleanupSlot，保留 claim/注册/slot/quota，其他进程仍 claim=None，原 owner 重试；成功后才释放 claim 并 disarm_after_delete。恢复不得重发。成功响应后、终态落盘前崩溃允许重复，不能承诺 exactly-once。 |
| terminal_failed | 记录不含正文/凭据的分类计数，持久终态后按 delivered 行相同的借用 delete/归槽协议幂等删除；失败继续持 claim 并计 quota/cleanup，原 owner 重试清理而非 HTTP；成功后才释放 claim/注册/slot/quota，不因永久拒收进入 retry_wait。 |

- metadata v1、摘要 SHA-256 针对 stored bytes，raw_len 为解压 JSON；Input encoding=None、event_count=0；Export event_count 为实际 span 事件数，排除旧规则跳过的 Session/SDK record。批次 accepted_at 取成员最早时间，expires=accepted+TTL，不延寿。
- 同一 route 配额由跨进程 domain 锁保护；reserve 持久记录租用额度，OS owner lock 释放后恢复可回收未写额度；不能只用进程内 Atomic 算“全域 1 GiB”。占用含写入预留、input、ready/sending/retry_wait、终态待删除、descriptor 和原子更新临时文件；publish 将 reservation 转实际存储，不双计，也不提前释放已写空间。
- domain 采用磁盘桶分页索引/有界游标，不依赖一次 read_dir 全收集排序；删除/插入期间分页允许重复，ObjectId 去重范围受已领取 window 约束；每轮重新扫描可见完整集合，不能永久漏过新条目。startup 扫描先重建 quota 再接受新磁盘写入。
- 重启：不恢复 Agent；清理无锁 writing；验证 ready/sending/retry_wait；sending 锁随进程退出释放，未记录终态可再领取；损坏/未知版本隔离诊断、清理，绝不发送。Input 无活跃 owner 则清理，不合成成功结束。只有同 route 的 Export 恢复发送；其他 route 不跨发，可按同一私有根的清理策略过期回收。
- owner/前缀协议：live retry_wait 仍占有界 ready window/事件账本槽，不能为释放内存偷偷解锁。完整成功先持久 Delivered，再在原 seq 账本登记 Delivered，最后删除；flush 只信自己账本，不以 inspect/claim 的 NotFound 当 Delivered。意外失锁、文件消失或终态 checkpoint 失败必须显式报告未确认/失败；不猜测另一进程已交付。
- 唯一正常移交点是 shutdown：停止准入与重试调度，join 所有写入/读泵/HTTP，持久 retry_wait 后，在原 seq 账本先登记不可撤回的 HandedOffPending（计入 persisted_pending_events），再释放 claim。另一进程可以领取；原 owner 后续 flush/shutdown 返回冻结的 PersistedPending 结果，不再轮询其文件，也不因另一进程删除而升级 Complete。本轮不支持显式运行中移交 API，不需要交付回执；若需存活调度器接管并反向确认成功，须另经控制器裁决，不能由实现流添加。
- 崩溃释放 OS 锁时原进程/等待者均已不存在，新 owner 仅为恢复事件建立自己的 seq；旧进程的 flush 不跨重启恢复。shutdown 的正常移交与崩溃恢复分别测试，不能用 NotFound 等同 Delivered。
- native 私有目录 0700、文件 0600（其他 OS 等价 ACL），拒绝 symlink/path traversal，句柄只接受内部 ObjectId；不把认证头/secret 放 descriptor。fsync/lock 不支持的 FS 明确 Unavailable，不宣称崩溃持久性。
- shutdown 停止准入，给准备/发送/清理 60s grace；超时若仍有 IO，返回 Pending 并保留 owner，可重入 join，不 abort 后遗留阻塞线程。仅全部 task 已 join 才可报 PersistedPending 并关闭资源。OS 卡死不能保证强制有限时 join，见 D6；不得超时后谎报 Complete。

## 6. 配置表（每行对应 pub 字段；新环境键全部加入 ENVIRONMENT_KEYS）

| settings `langfuse.*` / Rust 字段 | 新环境键 | 类型与默认值 |
| --- | --- | --- |
| spool_enabled | LANGFUSE_SPOOL_ENABLED | bool=true |
| compression | LANGFUSE_COMPRESSION | LangfuseCompression=Gzip |
| input_spool_threshold_bytes | LANGFUSE_INPUT_SPOOL_THRESHOLD_BYTES | usize=262144 |
| payload_memory_threshold_bytes | LANGFUSE_PAYLOAD_MEMORY_THRESHOLD_BYTES | usize=262144 |
| memory_budget_bytes | LANGFUSE_MEMORY_BUDGET_BYTES | usize=33554432 |
| prepare_max_in_flight | LANGFUSE_PREPARE_MAX_IN_FLIGHT | usize=1 |
| stream_chunk_bytes | LANGFUSE_STREAM_CHUNK_BYTES | usize=65536 |
| spool_quota_bytes | LANGFUSE_SPOOL_QUOTA_BYTES | u64=1073741824 |
| spool_ttl_secs | LANGFUSE_SPOOL_TTL_SECS | u64=86400 |
| max_wire_bytes | LANGFUSE_MAX_WIRE_BYTES | usize=4194304 |
| shutdown_grace_secs | LANGFUSE_SHUTDOWN_GRACE_SECS | u64=60 |

共新增 11 键，总数 24；旧 `batch_max_event_bytes` 默认改 4194304，旧 batch=4194304、queue bytes=16777216、queue slots=1024、HTTP=2、flush=10s 不变。恢复页 64、descriptor 4 KiB、gzip level=6 与 §4 工作区账单为内部固定量，不额外开放配置。
优先级 env > global settings > default；逐键选择胜出来源后解析，新键 bool env 接受 trim/case-insensitive true/false/1/0/on/off，settings 必须 JSON bool；compression trim/case-insensitive gzip/none；整数十进制、非零、checked 转换，不接受负数/浮点/溢出。胜出的非法新值返回结构化错误且不 publish，不隐退到 default；旧字段非法值忽略/clamp 行为保留。
spool=false：不构造 native、不扫描/恢复/清理旧 spool、不创建任何新磁盘文件；gzip 独立生效，内存路径继续预算/拒收。Langfuse 缺认证：不创建 spool。WASM/emscripten：native 模块/依赖排除，spool 能力 None、WARN 一次报告有界内存模式；内存 FS 不算磁盘能力。native 不可用/满盘：可见分类诊断，仅当预约内存成功才回退，否则拒收计数。

## 7. P1–P5 拆分、精确文件映射与依赖顺序

路径括号展开表示该行中列出的**每一个文件**；数字为各文件实现后的行数上限。★=公共接口/共同挂载文件，只能单一实现流写；后继提交改动前先交接锁。未列源码不得顺手改动，确有新增影响先补本文文件清单。测试文件上限统一 900，见 §8；新增 `_test.rs` 必须由对应实现文件挂载。

| 任务/执行流 | 修改文件 → 上限；新建文件 → 上限 | 依赖与完成条件 |
| --- | --- | --- |
| P1-0 公共结果/声明（集成流） | 新 ★`langfuse-client/src/batcher/result.rs`→180；改 ★`langfuse-client/src/{lib.rs,batcher.rs}`→各80/500；`peri-controller/src/langfuse/session_like.rs` 此阶段只读，P3c 才取得写锁 | 最先交付 DeliveryCounts/FlushReport/ShutdownReport；错误所需 ObjectId/SpoolError 与 P1a 的 LangfuseError::Spool 同单元声明挂载；冻结 LangfuseSessionLike 新方法声明但不在此阶段切换源码；P1a 的 error 不再前向依赖 P4。接口交付与运行期实现分开；尚未迁移实现体时不能声称 workspace 编译通过，交付闸门见下文 |
| P1a 配置/错误基础类型 | 改 ★`peri-config/src/observability.rs`→500、`peri-config/src/assembly.rs`→350；新 `peri-config/src/observability_spool.rs`→400；改 ★`langfuse-client/src/config.rs`→400、★`langfuse-client/src/client_export.rs`→500（只 ExportConfig）、★`langfuse-client/src/error.rs`→180、★`langfuse-client/src/lib.rs`→80；新 ★`langfuse-client/src/spool.rs`→250、★`langfuse-client/src/spool/types.rs`→250、★`langfuse-client/src/spool/error.rs`→120 | 依赖 P1-0 结果类型；ObjectId/SpoolError 的声明、derive/错误实现及 spool/types/error 挂载，与 LangfuseError::Spool 同一原子交付单元，不能前向引用 P1b；既有 observability/config 测试更新 Result/default；session.rs 暂不改，编译例外见闸门 |
| P1b 端口与所有权 | 交接修改 ★`langfuse-client/src/spool.rs`→250、★`langfuse-client/src/spool/types.rs`→250；新 ★`langfuse-client/src/payload.rs`→350、★`langfuse-client/src/snapshot.rs`→400、★`langfuse-client/src/memory.rs`→350、★`langfuse-client/src/spool/cleanup.rs`→400；改 ★`langfuse-client/src/lib.rs`→80 | P1a/P1-0 后扩展已存在的 spool/types，交付预算身份、清理/attempt/屏障跨流类型；资源实现范围仅 crate 内；端口契约测试先行，不引入资源业务 crate |
| P1c native | 新 `langfuse-client/src/spool/native.rs`→500、`spool/native_object.rs`→500、`spool/native_quota.rs`→500、`spool/native_scan.rs`→400（后三个均在 langfuse-client/src 下） | 依赖 P1b；真实临时 FS、跨进程 claim/quota、取消/恢复；无 HTTP |
| P1c 拆分（收敛轮二 2026-10-12） | 新 `langfuse-client/src/spool/native_test/lock.rs`→900；改 `spool/native_test.rs`（971 → 841）、`spool/native.rs`（472 → 477） | 只把「取锁语义」用例（不可领取判定、并发删除降级、能力探针竞争）逐字搬出并显式 `#[path]` 挂载为 `spool::native::tests::lock`；`native.rs` 只改 `probe_durable_io` 的竞争判定（`WouldBlock` 不再降级为 `Unavailable`），不改端口/签名；理由与证据见实施报告 §10.1/§10.3 |
| P1c 取锁语义（收敛轮三续 2026-10-13） | 改 `langfuse-client/src/spool/native.rs`（477 → 493）、`spool/native_test/lock.rs`（188 → 208） | 只把「对端持锁」的错误形态抽成 `is_lock_contention`（Unix `WouldBlock`；Windows `ERROR_LOCK_VIOLATION`(33)），`probe_durable_io` 与 `try_lock_file` 共用；`concurrent_delete_never_turns_lock_attempts_into_io_faults` 的「持锁清理分支」覆盖改为循环前确定性写入摘要不符条目（并发删除的产物依赖 OS 删除语义，Windows 上不可作覆盖证据），断言只增不减：原有 `pages > 0` 与「取锁不得报 IO 故障」保留，未改端口/签名；理由与证据见实施报告 §11.4 |
| P1c 取锁分类（收敛轮四 2026-10-13） | 改 `langfuse-client/src/spool/native.rs`（493 → 501）、`spool/native_test/lock.rs`（208 → 236） | 只把 `try_lock_file` 的取锁失败判定从「事后探测父目录」改成「按本次 `open` 的错误分类」：新私有 `is_concurrent_dir_removal`（`NotFound`；非 Windows 平台并入 `InvalidInput`——macOS 实测的「目录正在被删除」形态），删除 `lock_parent_is_dir`；新增确定性回归 `open_failure_is_classified_by_error_kind_not_by_probing_the_object_dir`（悬挂符号链接构造「`open` 报 `ENOENT` 而锁文件父目录仍在」的同一状态，旧判定下该用例红），既有断言逐字未改；理由与证据见实施报告 §12 |
| P1c 取锁分类（收敛轮五 2026-10-13） | 改 `langfuse-client/src/spool/native.rs`（501 → 512）、`spool/native_test/lock.rs`（236 → 266） | Windows job 失败面：并发删除在 Windows 报 `ERROR_ACCESS_DENIED`(5)（delete-pending 的目录 / 条目），收敛轮四的按错误分类未收该形态 ⇒ `list` 整页扫描失败。修：`is_concurrent_dir_removal` 增按**原始错误码** `Some(5)` 的 `#[cfg(windows)]` 形态（不按 `PermissionDenied` kind 并入，避免吞并未映射的共享冲突码）；新增 `#[cfg(windows)]` 确定性回归 `access_denied_open_failure_is_classified_as_concurrent_dir_removal`（锁路径被目录占位触发同一错误码，先 `assert_eq!(raw_os_error, Some(5))` 再断言 `Gone`）；既有断言逐字未改；理由与证据见实施报告 §13。**注**：`native.rs` 自设上限 500，轮四起为 501、本轮 512——与 D9 的超限清单同属待裁决口径（仓库硬上限 1000 未破） |
| P1d 依赖整合 | 改 ★`langfuse-client/Cargo.toml`→100、★`peri-acp/Cargo.toml`→180、★`Cargo.lock`→生成文件（不适用源码行数上限） | 在首个需要新依赖的交付单元前独占 manifest/lock；新增直接依赖边由 Cargo 正常更新 lock，更新后的 lock 必须随依赖变更交付并保持 `--locked` 校验通过；复用锁内 flate2=1.1.9、bytes=1.12.1、fs2=0.4.3，native 才启用 fs2/reqwest stream；tokio-util 如采用需 native io feature；WASM 仅有界内存，无文件泵 |
| P2a 编码 | 改 ★`langfuse-client/src/client_export.rs`→500；新 `langfuse-client/src/encoding/{mod.rs,writer.rs,json_string.rs}`→各500；改 `langfuse-client/src/types/conversion.rs`→400、`langfuse-client/src/types/conversion/{trace.rs,observation.rs,generation.rs,metadata.rs}`→各400 | P1b 后可与 P1c 并行；先内容保真，再双限额及 gzip；仅共用 emitter；与 P2b 原子交付，无独立可编译交接点，P2a 不写 client.rs |
| P2a CI 修复（收敛轮三 2026-10-12） | 新 `langfuse-client/src/stable_json.rs`→150、`src/stable_json_test.rs`→900；改 `langfuse-client/src/{lib.rs,encoding/mod.rs,snapshot.rs}`、`src/types/conversion/{metadata.rs,generation.rs}`、`peri-controller/src/langfuse/tracer/input_snapshot.rs`、`peri-controller/src/langfuse/{bridge/snapshot_test.rs,tracer/input_snapshot_test.rs,tracer/production_observation_test.rs}` | 只在 JSON 序列化边界引入显式键序：对象键字典序、map 按键排序；不改事件与 types 字段类型、不改冻结签名；CI 全量构建（`preserve_order` 生效）与单 crate 构建必须产出同一字节。`snapshot.rs` 暴露 `write_stable_json` 作为键序唯一实现，controller 的 messages/tools fallback 与三处测试期望随之统一到该口径（期望值用测试文件内独立 `stable_bytes` 实现，不复用产品代码） |
| P2b HTTP | 改 ★`langfuse-client/src/client.rs`→450（本阶段 P2b 唯一写者）；新 `langfuse-client/src/client_send.rs`→500、`langfuse-client/src/client_body.rs`→400 | P1b/P2a；在同一原子单元迁移 client.rs:92 的旧 encode 调用及相关测试后才发布 P2 基线；复用不修改 client_retry/client_response 策略；mid-body 断连从 0 重开 |
| P3a 准入/worker | 改 ★`langfuse-client/src/batcher.rs`→500、`langfuse-client/src/batcher/{admission.rs,budget.rs,worker.rs}`→各650；新 `langfuse-client/src/batcher/{prepare.rs,split.rs}`→各500 | P1/P2；准备 slot/permit 先于 retained；最大单条独立拒收；精确拆批 |
| P3b tracker/gate | 改 `peri-controller/src/langfuse/tracer/{generation.rs,llm_events.rs,turn_fallback.rs,event_builder.rs,subagent_events.rs}`→各600、`tracer/registry/{types.rs,gate.rs}`→各350（均 peri-controller/src/langfuse 下）；新 `peri-controller/src/langfuse/tracer/input_snapshot.rs`→400；改 ★`peri-controller/src/langfuse/tracer/mod.rs`→350 | P1/P3a/P3c 编译交付后；不再先跑依赖未知 SessionLike 的骨架；快照在 gate 前计费，request/end/cancel 序不变；桥接入口无需 IO；新 adapter 借用序列化 |
| P3c session 运行期接线 | 改 ★`peri-controller/src/langfuse/session.rs`→350（P1a 至 P3a 不写，P3c 独占，完成后交 P4b）、`peri-controller/src/langfuse/fake_session.rs`→250、★`peri-controller/src/langfuse/session_like.rs`→350；按 P1 已冻结签名切换源码，不重新定义接口 | 依赖 P1 公共声明 + P3a 的真实 Batcher 方法（不依赖 P3b）；在 session.rs:104–115 删除 max_retries、映射 telemetry 并传入 spool，落实 try_freeze/prepared/budget/flush 委托及局部 fake 的真实预算/错误行为，然后才放行 P3b；P4b 只接手关闭映射 |
| P4a 结果/恢复运行期 | 新 `langfuse-client/src/batcher/recovery.rs`→500；改 `langfuse-client/src/batcher/{failure.rs,shutdown.rs,stats.rs}`→各350；交接修改 ★batcher.rs→500、worker.rs→650；消费 P1 的 result/error 类型，不在 P4 创建它们 | P3a 后；恢复/TTL 与 live 事件同 worker；统计扩展见下一段 |
| P4a 拆分（收敛轮 2026-10-12） | 新 `langfuse-client/src/batcher/{recovery/scan.rs,failure/barrier.rs,worker/recover.rs}`→各500/350/350；改 `batcher/{recovery.rs,failure.rs,worker.rs}`（579/356/664 → 441/292/521）；★`batcher.rs` 不动，子模块由各自父文件挂载 | 只按职责逐字搬移既有实现（启动扫描与清理登记 / flush·shutdown 屏障账本 / 启动恢复接线），可见性与调用点保持原样，不改行为与冻结签名；`batcher/recovery_test.rs` 957 未拆，理由见实施报告 §9.2；证据见实施报告 §9.3 |
| P4b 宿主关闭 | P3c 交接后独占修改 ★`peri-controller/src/langfuse/session.rs`→350 的关闭映射；改 `peri-controller/src/langfuse/{drop_telemetry.rs,tracer/turn.rs}`→各350；改 ★`peri-acp/src/host/assemble.rs`→990、★`peri-acp/src/host/mod.rs`→1000、`peri-acp/src/host/lifecycle.rs`→350；新 `peri-acp/src/host/telemetry.rs`→250 | new_owned 接受能力；assemble 只替换调用不堆逻辑；ACP 测试实际 shutdown；TUI 产品处理不变 |
| P5 验收/文档 | 改 `docs/standards/{environment-variables.md,architecture-contracts.md}`→各400、`docs/code-index/{langfuse-client.md,peri-controller.md,peri-config.md}`→各300；回填本文→550（文档预算） | P1–P4 全部合入后唯一集成流；真实端点缺口在本文 §9 留运行报告，不另建多份事实源 |

P4 stats.rs 新 pub 字段：`raw_bytes_total, wire_bytes_total, active_snapshot_bytes, preparing_memory_bytes, memory_queue_bytes, spool_bytes, spool_objects, oldest_pending_age_ms, retry_attempts, recovered_events, expired_events, corrupt_objects, quota_rejections, resource_fallbacks: u64`；旧字段保留；队列/在途非事务快照，DeliveryCounts 屏障结果须一致快照。降级原因用 `pub enum SpoolFallbackReason { Disabled, Unsupported, Unavailable, Quota, Io }` 与 `pub fallback_reason: Option<SpoolFallbackReason>`；INFO 模式/汇总、WARN 降级/周期失败、ERROR 终态/损坏，无 payload/auth/服务端正文。
无环依赖图（箭头表示真实交付前置，不是同时改共享文件）：`P1d → 首个新增依赖消费者`；`P1-0值类型 → P1a（含 ObjectId/SpoolError）→ P1b声明/所有权`；`P1d + P1b → {P1c, P2a/P2b原子单元}`；`P2a → P2b` 仅为原子单元内部顺序，不发布 P2a 独立基线；`P1c + P2单元 → P3a → P3c → P3b`；`P3a → P4a`；`P3b + P3c + P4a → P4b → P5`。SessionLike 新声明在 P1 冻结，但 trait 的可编译源码切换与全部实现体在 P3c 原子交付；不得提前挂载缺少实现的方法使后继流自行修编译，也不得用默认 Ok/None/todo!/unimplemented! 冒充接口落地。P1-0 的结果值类型先真实编译并重导出，P1a error 才引用；接口签名依赖图与运行期实现图分开，不把“声明已写”当“实现已可用”。

公共挂载全部归集成流，不分给并行实现流；P1 冻结模块树/可见性及以下挂载表，源码挂载只在对应文件实际交付时串行提交，不建空文件占位。每次交接交付文件清单、公共签名、实际 check/test 命令/exit/非零用例数；后继仅基于下述闸门明确的 crate/target 可编译基线工作，不改前驱文件；未纳入闸门的产品调用点不宣称通过。若接口在其必验 target 内单独无法编译，就与其真实实现/消费者原子交付并顺延后继，而不是临时兼容 shim。

| 交接点 | 集成流独占的公共挂载/接口 | 后继无需碰共享文件的条件 |
| --- | --- | --- |
| P1-0 → P1a | batcher.rs 挂 `result` 并 lib.rs 重导出结果类型 | 值类型无运行期依赖；独立真实 check 通过后交 error.rs |
| P1a → P1b | P1a 同时挂载 spool/types/error、声明 ObjectId/SpoolError 并添加 LangfuseError::Spool；observability.rs 挂 observability_spool 与测试 | 不将 SpoolError/ObjectId 的声明或挂载留到 P1b；集成流把 spool.rs/types.rs 写锁交 P1b，error.rs 不由后继自行补类型 |
| P1b → P1c/P2 单元 | lib.rs 挂 `payload/snapshot/memory`；spool.rs 扩展端口并挂 cleanup | 内存/注册/受控构造真实实现和局部测试可编译；registry 的 worker drain 接线到 P3a，不用假 worker；trait 本身可声明，不伪造实现 |
| P1c；P2a/P2b 原子单元 | spool.rs 挂 native 四模块并 cfg；lib.rs 挂 crate-private encoding，encoding/mod.rs 挂 writer/json_string/stream_test | P1a 释放 client_export.rs 后交 P2a；client.rs 仅 P2b 可改，公共挂载时暂停并交集成流；encode 新签名、client 调用与相关测试同单元验收，不从 P2a 单独交付破损基线 |
| P2b → P3a | client.rs 挂 client_send/client_body；AttemptCheckpoint/SendOutcome 由 P1b 冻结、P2b 实现；batcher.rs 挂 prepare/split/spool_test | 编码与 checkpoint/HTTP 真实测试通过；batcher.rs 从 P1-0 串行交给 P3a，集成挂载时实现流暂停该文件写入 |
| P3a → P3c → P3b | P3c 原子切换 session_like.rs、session.rs、fake_session 及本地 trait fake；随后 tracer/mod.rs 挂 input_snapshot | P3a 完成 Batcher 新方法与结果返回再交 P3c；P3c check/test 通过才放行 P3b，P3b 不再改 session 文件 |
| P3a/P3c → P4a/P4b | batcher.rs 挂 recovery/recovery_test；host/mod.rs 挂 telemetry，telemetry.rs 挂 telemetry_test；assemble.rs 只替换接线 | P3a 释放 batcher/worker，P3c 释放 session；P4a 与 P3b 文件不重叠，可并行；P4b 等二者后原子迁移宿主 |
| 每个新测试 → 所属阶段验收 | memory.rs→memory_test；native.rs→native_test；observability_spool.rs→observability_spool_test；input_snapshot.rs→input_snapshot_test；其余按上表 | 文件作者只交测试文件，集成流在生产 owner 暂停写入后挂载；`tests/` 自动发现不改 lib.rs；过滤命令必须实际命中非零测试 |

上述新模块均使用 §7/§8 已列文件；增加的 `spool/cleanup.rs` 属 P1b。公共文件只能一位写者；P1d 集中处理依赖与 Cargo 正常生成的 lock，D7 已裁决，不再设置人工批准前置；依赖/`--locked` 校验确实失败时才不放行相关编译任务。允许减少并行度以保证真实可编译交付，不允许 consumer agent 自补公共签名或运行期空实现。

### 7.1 交接编译闸门

每次发布交接基线都必须列出 crate、Cargo target（`--all-targets` 包含 lib/bin/examples/tests/benches）、平台 triple、feature 集与实际命令/exit；下表为最小必过范围，未列 crate 不代表已编译。native 使用 `./scripts/cargo-rmcp-patched.sh check --locked -p <crate> --all-targets`；相关 §8 测试仍须执行并报告非零用例数。破坏本行必验 target 的改动必须与真实消费者原子交付，不允许以“下一阶段再修”放行。

| 交接基线 | 必须编译通过的 crate / target | 明确暂缓的产品调用点与解除阶段 |
| --- | --- | --- |
| P1-0 | langfuse-client，native `--all-targets` | 只落地结果值类型；SessionLike 新签名仅在本文冻结，源码切换延至 P3c |
| P1d | langfuse-client、peri-config，native `--all-targets`；与首个新依赖单元合交时继承该单元范围 | manifest/lock 一致交付；ACP 新生产依赖声明不代表 host 接线已通过 |
| P1a | langfuse-client、peri-config，native `--all-targets` | 允许 peri-controller 暂不编译：session.rs:104–114 的 BatcherConfig 字面量仍含 max_retries、缺 telemetry；P3c 才迁移。peri-acp/peri-tui 等上层闭包此时不验收 |
| P1b、P1c（各自交接） | langfuse-client、peri-config，native `--all-targets` | 继续保留上述 session 产品调用例外；不提前切换 Batcher 构造/flush/shutdown 的运行期签名 |
| P2a/P2b 原子单元 | langfuse-client、peri-config，native `--all-targets` | P2a 不单独交接；client.rs:92 的旧 encode 与 client_export_test.rs 消费者必须同单元迁移，无 client crate 内编译例外；controller 例外仍到 P3c |
| P3a；P4a（各自交接） | langfuse-client、peri-config，native `--all-targets` | P3a 切换 Batcher 签名后，session.rs:69,115,140 的 shutdown/构造/flush 与 controller fixtures 在 P3c 迁移；P4a 不覆盖已通过的 P3c 基线 |
| P3c、P3b（各自交接） | langfuse-client、peri-config、peri-controller，native `--all-targets` | P3c 消除全部 controller 例外并同步本地 trait fake/测试；new_owned 新 spool 参数在 ACP assemble.rs:913、host/stdio/langfuse_shutdown_test.rs:27 的调用延至 P4b；peri-acp/peri-tui 闭包暂不验收 |
| P4b → P5 | langfuse-client、peri-config、peri-controller、peri-acp、peri-tui，native `--all-targets` | 宿主产品接线/测试同时迁移，不再保留上述例外；必须执行跨两次真实 host shutdown 的生命周期验证 |
| P5 最终放行 | workspace，native `--all-targets`；`scripts/cargo-wasm.sh check --locked -p peri-wasm` 对应目标 lib | 全部例外关闭；WASM triple/feature 以脚本实际输出登记；再执行 §8/§9 验证，不以编译代替生命周期/真实端点验收 |

写锁以任务表和公共挂载表为准：session.rs 在 P1a–P3a 只读、P3c 迁移配置及 SessionLike/Batcher 消费者、P4b 接手关闭映射；client.rs 在 P1/P2a 只读、P2b 在 P2 原子单元内迁移 encode/HTTP，集成流仅在 P2b 暂停写入后挂载。新增未列消费者或必验 target 的失败须先补清单、调整原子单元再交接，不能暗中扩大编译例外。

## 8. 测试计划与必须保留的回归

每个新测试文件≤900 行；局部 fixture，不共享 test_helpers、不新增 mockall；native FS 用临时目录，环境隔离，TTL/重试时钟可控。下表包含建议用例名，测试入口由集成流按 §7 交接表串行挂载；集成 tests/ 自动发现。

冻结待实施的生命周期反例（测试尚未实现/执行，均待执行验证，不替代下表旧用例）：

| 审阅项 | 文件/建议用例 | 必须观察到的结果 |
| --- | --- | --- |
| 2：每次请求 checkpoint | `tests/spool_http.rs`：`every_attempt_checkpoints_before_http`、`checkpoint_failure_sends_no_request` | 至少两次周期内 HTTP retry，各次 execute 前能从持久 meta 读到新 attempts；第 N 次 checkpoint 注入失败时 HTTP 请求数恰为 N−1，claim/正文仍有 owner、无 Delivered，取消 checkpoint 后仍 join |
| 3：跨进程 retry_wait | `tests/spool_recovery.rs`：`live_retry_wait_cannot_be_taken_over`、`shutdown_handoff_keeps_original_prefix_pending` | 两真实子进程竞争：A 活跃时 B claim=None；A 正常移交后 B 可发送删除，但 A 重复 flush 仍 PersistedPending；另测 A 崩溃后 B 接管；NotFound 注入不能转 Complete |
| 4：准入/构造失败清理 | `src/batcher/spool_test.rs`：`rejected_prepared_input_drains_original_cleanup_slot` | try_new 字段冲突、预算不够、try_add_prepared 关闭/QueueFull/跨部署失败逐项覆盖；Preparing/Inline/Spooled 都验证原 owner join、slot/permit/已删 quota 恰好回收一次，迟到 publish 不泄漏；失败 delete 仍计 quota/cleanup |
| 5：近耗尽/预算身份 | `src/memory_test.rs`、`src/batcher/spool_test.rs`：`retained_saturation_preserves_progress_workspace`、`foreign_budget_and_mixed_sources_are_rejected` | retained 占满可用分区后已接受 freeze→编码→HTTP 可推进，新源立即拒收；工作区未被挪用、无相互等待；同额度不同 Arc、事件同预算但某个 input/output/ticket 异预算均 BudgetMismatch |
| 6：无 end 的准备前缀 | `src/batcher/spool_test.rs`：`flush_after_freeze_before_end_waits_only_prefix`、`suffix_freeze_cannot_delay_prefix_flush` | 暂停前缀准备后 flush 尚不完成；后缀 freeze/end 持续到来不扩大边界；释放前缀准备即可返回、pending_events=0、不等 end；准备失败显式错误、取消 waiter 不吞失败 |
| 6：驱逐完整回收 | `src/batcher/spool_test.rs`：`backpressure_state_matrix_reclaims_all_owned_resources` | 三策略×§5.1 每行分别测；DropOldest 只逐出可驱逐最旧后缀且新项在队尾，无足额候选不部分删；flush 双前缀不动、writing/HTTP 不动；Inline permit、ticket、input 文件/失败临时文件、输出 allowance 与 quota 最终按协议回收 |
| R1：删除失败保留 claim | `tests/spool_recovery.rs`：`failed_delete_keeps_claim_until_owner_retry_succeeds` | A 清理 owner 注入 delete 失败后，B 真实子进程仍 claim=None，原 CleanupSlot/注册/quota 不释放；撤除故障后 A 用同一 claim 重试删除成功，claim/slot/注册/已写 quota 恰好释放一次，无 HTTP 重发；不能用 A 退出释放锁替代重试 |
| R3：跨两次真实 host shutdown | `peri-acp/src/host/telemetry_test.rs`：`host_shutdown_reenters_after_telemetry_pending` | 同一个真实 AcpHostHandle 第一次 shutdown 遇受控 IO/清理阻塞返回 TelemetryFailed(Pending)，retry 保留原 context/owner、terminal 未缓存；解除阻塞后第二次 shutdown 必须真正重入 finish 并 join/清理，返回实际 Complete 或已完成移交的 PersistedPending，而非重放第一次报告；移交分支须验证无本地任务/清理 owner 后才缓存终态 |
| 7/8：独立编译与可见性 | 各 owner 的局部契约测试、crate public doc tests | P1 结果类型可在 error 之前编译；ObjectId/SpoolError 与 LangfuseError::Spool 同单元挂载；P2a/P2b 原子迁移 encode 消费者；实际 trait 所有实现同步；外部消费者可注入 NativeSpool 但不能伪造 ID/SpoolObject 或 impl sealed；内部 fake 可实现并 publish；每流按 §7.1 的 crate/target 范围独立 check/test，0 tests 不算通过 |

表内 `src/`、`tests/` 未带 crate 前缀者均相对 `langfuse-client/`，不新增共享 fixture 或第二发送器。

| P | 新文件（精确路径） | 必测用例名/断言 |
| --- | --- | --- |
| P1 | `peri-config/src/observability_spool_test.rs` | `spool_defaults_and_env_precedence`、`invalid_winning_value_rejects_snapshot`、`disabled_spool_preserves_old_files`；24 键逐项、全部默认、边界 0/溢出/空白/大小写，旧键无回归 |
| P1 | `langfuse-client/src/memory_test.rs`、`langfuse-client/src/spool/native_test.rs`、`langfuse-client/src/spool/native_test/lock.rs` | `reserve_before_retain_and_release_once`、`atomic_publish_never_exposes_partial_body`、`quota_includes_writing_and_failed_delete`、`claim_is_exclusive_across_processes`；8 MiB quota、失败清理后归零。收敛轮二新增（`native_test/lock.rs`）：`lock_attempt_reports_gone_instead_of_io_error_when_parent_is_deleted`、`concurrent_delete_never_turns_lock_attempts_into_io_faults`、`peer_holding_the_domain_lock_is_contention_not_unavailability`。收敛轮三续（2026-10-13）：`concurrent_delete_never_turns_lock_attempts_into_io_faults` 的「持锁清理分支」覆盖改为循环前确定性写入摘要不符条目并断言 `list` 返回 `Err(SpoolError::Corrupt { .. })`（原 `corrupt_pages > 0` 断言依赖 OS 删除语义，Windows 上不可达），循环内断言不变。收敛轮四（2026-10-13）新增 `open_failure_is_classified_by_error_kind_not_by_probing_the_object_dir`：取锁失败按**本次 `open` 的错误**分类（`ENOENT`；非 Windows 并入 macOS 实测的 `EINVAL`），不做事后探测父目录（旧判定下该用例红）。收敛轮五（2026-10-13）新增 `#[cfg(windows)]` 回归 `access_denied_open_failure_is_classified_as_concurrent_dir_removal`：Windows 的并发删除形态 `ERROR_ACCESS_DENIED`(5) 必须判 `Gone`（锁路径被目录占位触发同一错误码，先钉错误码再断言分类） |
| P1/P4 | `langfuse-client/tests/spool_recovery.rs` | `child_crash_at_each_durable_boundary`、`credential_rotation_never_cross_sends`；同一测试 binary 子进程退出再启动，writing/ready/sending/ack-before-delete 全覆盖，非单进程对象重建冒充崩溃 |
| P2 | `langfuse-client/src/encoding/stream_test.rs` | `hundred_boundary_inputs_roundtrip_exact_bytes`、`gzip_and_none_share_otlp_semantics`、`escaped_utf8_across_chunk_boundary`；≥100 固定种子样本、input/output/metadata、usage/cost/ID/time 保真 |
| P2 | `langfuse-client/src/stable_json_test.rs` | `stable_map_sorts_keys_despite_hash_order`、`stable_map_recursively_sorts_value_fields`、`stable_json_sorts_nested_objects_and_keeps_array_order`、`stable_json_matches_scalar_baseline`、`stable_json_handles_empty_containers`；`Value` 保插入序、`HashMap` 随机序时仍必须得到排序字节 |
| P2 | `peri-controller/src/langfuse/bridge/snapshot_test.rs`、`tracer/input_snapshot_test.rs`、`tracer/production_observation_test.rs` | 各文件内的 `stable_bytes` 期望口径（对象键递归字典序、数组保序，独立实现）；`borrowed_adapter_matches_baseline_bytes` 与 `fallback_baseline` 现按该口径逐字节钉住 fallback 冻结字节，raw 路径断言（`observe_raw_request_supersedes_snapshot_for_completed_generation`、`request_end_cancel_order_holds_for_parallel_generations`、`cancel_keeps_terminal_once`、`abandoned_generation_prefers_complete_raw_request`、`gated_generation_preserves_raw_request_through_late_subagent_join`）同口径比较 |
| P2 | `langfuse-client/tests/spool_http.rs` | `retry_reopens_at_zero_after_mid_body_disconnect`、`gzip_length_matches_wire_body`、`partial_success_never_replays_file`；本地 TCP server，真实断连，SHA/ID 不变；内存/文件均试 |
| P3 | `langfuse-client/src/batcher/spool_test.rs` | `otlp_expansion_splits_without_loss`、`oversized_span_rejects_only_itself`、`prepare_budget_bounds_slow_disk`、`flush_prefix_protects_preparing_events`；混合合法/超大，显式 512 KiB 限额仍严守 |
| P3 | `peri-controller/src/langfuse/tracer/input_snapshot_test.rs` | `long_call_releases_telemetry_raw_arc_after_publish`、`late_join_gate_is_budgeted`、`end_before_snapshot_ready_keeps_input`、`cancel_keeps_terminal_once`；看 Arc/permit 和最终 input 字节，不仅看文件存在 |
| P4 | `langfuse-client/src/batcher/recovery_test.rs` | `retry_wait_reports_persisted_pending`、`pending_counts_survive_repeated_flush`、`shutdown_cancel_reentry_keeps_owner`、`ttl_never_resets_on_retry`、`permanent_failure_and_pending_are_both_reported` |
| P4 | `peri-acp/src/host/telemetry_test.rs` | `host_reports_persisted_pending_without_complete`、`host_keeps_resources_until_io_joined`、`shared_session_has_no_shutdown_capability`；真实 owner 贯穿，不只测枚举转换 |
| P5 | `langfuse-client/tests/spool_acceptance.rs` | `thousand_calls_preserve_all_hashes`、`half_gib_backlog_has_constant_memory_window`、`ten_minute_outage_recovers_before_ttl`；可单独运行重型 profile，不默认真实公网 |

必须继续通过的既有文件（每个修改后≤1000 行；新增用例优先放上表新文件，不能机械挪行规避规模限制；可按新 API 调整 fixture，禁止删用例/弱化断言）：
- `peri-config/src/{observability_test.rs,assembly_test.rs,system_test.rs}`；其中默认 event/键数断言按明确目标精确改为 4 MiB/24，旧非法旧键、clamp 断言不变。
- `langfuse-client/src/{config_test.rs,error_test.rs,client_test.rs,client_export_test.rs,client_response_test.rs,client_retry_test.rs,batcher_test.rs,batcher_shutdown_test.rs,batcher_concurrency_test.rs,types_test.rs}`；`langfuse-client/src/batcher/{admission_test.rs,byte_budget_test.rs}`；`langfuse-client/src/types/{conversion_test.rs,session_test.rs}`；`langfuse-client/tests/logging.rs`。
- `peri-controller/src/langfuse/{config_test.rs,session_test.rs,metric_sink_test.rs,drop_telemetry_test.rs,bridge_test.rs}`；`peri-controller/src/langfuse/tracer/{generation_test.rs,event_builder_test.rs,production_observation_test.rs,tracer_test.rs,registry_test.rs,registry_lifecycle_test.rs}`；`peri-controller/src/langfuse/bridge/{snapshot_test.rs,registration_test.rs,ordering_test.rs,lifecycle_test.rs}`；`peri-controller/src/langfuse/tracer/registry/{registration_tracer_test.rs,parallel_tracer_test.rs,lifecycle_tracer_test.rs}`。
- `peri-acp/src/host/stdio/{langfuse_shutdown_test.rs,session_store_shutdown_test.rs}`；`peri-tui/src/acp_client/deployment_test.rs` 加 Pending 旁路用例。这些测试按 P3/P4 所属修改，文件锁遵循 §7；生产 fake_session 与每个测试本地 SessionLike 实现同步更新。
- 额外既有集成回归：`langfuse-client/tests/otel_contract.rs`、`peri-controller/tests/langfuse_e2e.rs`、`peri-acp/src/event/forwarder_test.rs`（若签名迁移涉及它们，P3 负责，修改后各≤1000 行）；不得只跑新 spool 测试遗漏已有真实 OTLP/forwarder 链路。
- 旧 body 指针测试迁移为内存 Bytes 共享、一次编码+多次请求重建的更强测试；none 模式必须继续逐字节匹配原 OTLP，gzip 模式先解压再做同样断言；禁止改成只断言 HTTP 200。

验证命令：`./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib`（langfuse-client/peri-config/peri-controller/peri-acp/peri-tui）；新集成逐个 `test --locked -p langfuse-client --test <target>`；变更 public doc 后 `test --locked -p <crate> --doc`；`cargo fmt --check`、`bash scripts/check-layer-imports.sh`、`bash scripts/check-file-size.sh`、`git diff --check`。过滤测试必须报告非零用例数/最终 exit status；WASM 用仓库 `scripts/cargo-wasm.sh check --locked -p peri-wasm` 并核对 target，不能用 native check 代替。

## 9. 计划书 §9 逐项验收映射（P5 2026-10-12 回填；文档与验收表同步流同日复跑后复核：实际状态与证据）

| 原文场景 | 离线自动化文件与精确断言 | 真实端点/人工缺口 | 实际状态与证据（命令与数字见[实施报告](2026-10-12-langfuse-telemetry-spool-report.md)） |
| --- | --- | --- | --- |
| 默认及关闭 | observability_spool_test.rs + native_test.rs：true 默认、false/0/off 文件增量=0，非法值 Err，memory used≤budget | 无；测试不能扫用户 HOME | **已自动化**：`observability_spool_test.rs::spool_defaults_and_env_precedence`、`new_boolean_and_compression_forms_are_normalized`、`new_integer_keys_reject_zero_negative_float_overflow_and_blank`、`invalid_winning_value_rejects_snapshot`、`disabled_spool_preserves_old_files`；`langfuse-client/src/spool_test.rs::disabled_spool_never_opens_native_and_leaves_the_spool_directory_untouched`；`src/batcher/freeze_test.rs::disabled_spool_never_touches_disk_and_rejects_over_allowance`。仍不扫 HOME |
| 内容保真 | encoding/stream_test.rs：≥100 样本，解出 stringValue.as_bytes 等于冻结 JSON，0 mismatch，全部非 input 字段同基线 | 真实接收展示另测，不以 mock 证明可查询 | **已自动化（离线部分）**：`encoding/stream_test.rs::hundred_boundary_inputs_roundtrip_exact_bytes`、`escaped_utf8_across_chunk_boundary`、`spooled_and_inline_sources_share_one_escaping`、`frozen_baseline_bytes_are_stable`；`batcher/send_test.rs::frozen_input_and_output_reach_the_wire_verbatim`。真实接收展示未测 |
| 长会话 | tests/spool_acceptance.rs：1000 调用、最后约8 MiB，event12 MiB/raw16 MiB，wire 显式16 MiB；本地收集 IDs/hash，0 静默丢弃 | **需真实端点**确认服务上限、1000 条可查询/hash；记录总原文/线包/CPU，未测不得写通过 | **未测**：`langfuse-client/tests/spool_acceptance.rs` 未创建，无离线用例；需真实端点 |
| 编码膨胀 | batcher/spool_test.rs：原文总量<batch但OTLP超限，拆批后每包raw/wire≤上限，ID集合完全相等 | 无 | **已自动化**：`batcher/split_test.rs::encoded_expansion_splits_without_losing_events`、`event_count_limit_splits_batches`、`raw_batch_limit_splits_before_the_encoded_limit`；`batcher/spool_test.rs::otlp_expansion_splits_without_loss` |
| 超大单条 | batcher/spool_test.rs：SingleSpanTooLarge 正确上限/实际值，拒收恰1，邻条送达，不截断/伪 span | 无 | **已自动化**：`batcher/spool_test.rs::oversized_span_rejects_only_itself`；`batcher/split_test.rs::oversized_encoded_span_is_rejected_while_the_rest_continues`、`oversized_raw_span_maps_to_the_frozen_error`、`measured_item_over_the_encoded_limit_is_rejected_not_batched` |
| 慢网络/断网积压 | tests/spool_acceptance.rs：512 MiB wire backlog、1 GiB quota、并发≤2、受控计费峰值≤32 MiB、页≤64；高熵 fixture 避免压缩“空跑” | **人工测量**同机匹配基线额外 RSS≤64 MiB；allocator/框架账单校准，超出写解释；不能用 permit 计数替代 RSS | **部分覆盖**：受控预算与配额部分见 `memory_test.rs::retained_saturation_preserves_progress_workspace`、`used_includes_isolated_progress_partition_exactly_once`、`workspace_pools_are_exclusive_and_return_leases`、`spool/native_test.rs::quota_includes_writing_and_failed_delete`、`batcher/spool_test.rs::queued_bytes_charges_source_descriptor_not_contents`。512 MiB 积压与 RSS 需人工测量，无离线验收文件 |
| 断网恢复 | tests/spool_acceptance.rs：可控时钟推进10min、恢复本地网络、未终态持久事件集合最终相等，期间 flush 必为 Pending/PersistedPending | **真实端点演练**保留10分钟断网记录；离线虚拟时间不是实际墙钟演练 | **部分覆盖**：`batcher/recovery_test.rs::retry_wait_reports_persisted_pending`、`pending_counts_survive_repeated_flush`、`ttl_never_resets_on_retry`、`recovery_deferral_keeps_objects_for_the_next_owner`。10 分钟墙钟与端点演练未测 |
| 文件流重试 | tests/spool_http.rs：中途关闭连接，第二次完整正文从0、wire SHA/ID相同、编码次数1 | 无；用 TCP 不以单纯503替代断流 | **已自动化（离线，本轮实测通过）**：`client_send_test.rs::file_attempts_rebuild_the_request_from_byte_zero`、`file_attempt_must_be_drained_before_the_next_arm`、`settle_reports_an_unconfirmed_pump_at_the_deadline`；集成层 `langfuse-client/tests/spool_http.rs::retry_reopens_at_zero_after_mid_body_disconnect`、`file_body_is_served_from_spool_without_transfer_encoding`、`gzip_and_none_requests_match_encoding_and_length`（`test --locked -p langfuse-client --test spool_http` → 3 passed、0 failed、EXIT=0）。真实网络抖动仍需端点演练 |
| 崩溃恢复 | tests/spool_recovery.rs：四边界 kill 子进程，writing不发送，ready/未确认sending恢复；delivered marker存在不重发 | FS/OS 支持矩阵需实机；允许 ack 丢失重复，不允许丢未过期持久数据 | **部分覆盖**：`spool/native_test.rs::abort_during_writing_leaves_no_visible_object`、`restart_resumes_enumerable_objects_and_sweeps_residue`、`claim_is_exclusive_across_processes`、`batcher/recovery_test.rs::corrupt_entry_is_quarantined_without_send_and_next_round_recovers`。四边界子进程 kill 与 FS/OS 矩阵未测 |
| 配额及清理 | native_test.rs/recovery_test.rs：8 MiB、TTL60s、ENOSPC/删除失败注入；临时计费、不无限回退、清理重入后引用/账单归0 | 实际磁盘满/断电耐久可另人工测，不用注入冒充断电 | **部分覆盖**：`spool/native_test.rs::quota_includes_writing_and_failed_delete`（8 MiB）；`spool/cleanup_test.rs::failed_delete_keeps_claim_and_slot_until_retry_succeeds`、`other_owner_sees_no_claim_while_cleanup_slot_holds_it`、`handoff_disarms_once_without_deleting`；`batcher/recovery_test.rs::expired_entry_is_cleaned_without_send`。native_test 自述临时目录无法稳定复现 ENOSPC；断电耐久未测 |
| 生命周期 | recovery_test.rs + host/telemetry_test.rs：慢前快后、并发后缀、shutdown取消重入，唯一owner、前缀正确、counts精确 | 无 | **部分覆盖（P4b 已交付）**：`batcher/recovery_test.rs::shutdown_cancel_reentry_keeps_owner`、`permanent_failure_and_pending_are_both_reported`；`batcher/pending_test.rs::flush_reports_a_retry_waiting_prefix_as_pending`、`quota_rejection_is_counted_and_its_reservation_reclaimed`；`batcher/spool_test.rs::flush_prefix_protects_preparing_events`；`batcher/freeze_test.rs::disk_failure_falls_back_to_memory_once`；host 侧 `peri-acp/src/host/telemetry_test.rs::host_shutdown_reenters_after_telemetry_pending`、`host_reports_persisted_pending_without_complete`、`host_keeps_resources_until_io_joined`、`shared_session_has_no_shutdown_capability`、`spool_capability_requires_enabled_credentials_and_routable_endpoint`（`-p peri-acp --lib` 827 passed 内含）。缺口：驱逐矩阵用例（`backpressure_state_matrix_reclaims_all_owned_resources`）仍未建；§8 R3 的「第二次 shutdown 返回 Complete 或已完成移交的 PersistedPending」被实测证伪，见 §10 D8 |
| 平台差异 | native_test.rs/配置测试 + emscripten 编译闭包：None能力不调用磁盘；spool关闭gzip仍生效 | **需目标宿主运行**：WASM/浏览器JS/Blob缓存峰值、模式真实性；编译通过不能替代 | **部分覆盖（编译面本轮通过）**：spool 关闭与内存路径证据同第 1 行；`scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare` EXIT=0（`Checking peri-wasm` → `Finished`，闭包告警 langfuse-client 5 / peri-acp 1 / peri-resources 14 / peri-middlewares 17，无 error；命令与数字见实施报告 §2.1、§4）。JS/Blob 峰值与模式真实性需目标宿主运行，未测 |

性能补表：**尚未执行**。环境/OS/CPU/基线 commit 见实施报告的运行环境与命令表；冻结/入队耗时、gzip CPU、磁盘写量、P95/P99、排空时间、堆/RSS/JS 峰值均无实测数字，本文与报告不提供推测值。

## 10. 偏差与未决

| 编号 | 裁定/理由/实施影响 |
| --- | --- |
| D1 | 原文审查基线 ad447afea 保留；实施采用已核实 607f8a222，不改写历史正文。 |
| D2 | 用户已裁决 LANGFUSE_SPOOL_ENABLED / LANGFUSE_COMPRESSION；不接受旧 PERI_ alias、不做双读。 |
| D3 | 采用 C3 的 client 内端口/native adapter；ACP 只装配能力，controller 只拿 trait，不新增 crate、不引入 peri-resources→client。原计划“复用 MCP 部署方式”在此收敛为进程内可替换资源端口；本轮不实现远程文件 MCP/base64 流、不新增工具。 |
| D4 | 旧 resolve 无法返回新配置错误，故同步改 Result 并接已有 InvalidInput；新键严格、旧键行为不改。配置规则只在 peri-config 解释，client 仅做执行安全校验。 |
| D5 | 计划明确要求 event 默认512 KiB→4 MiB、默认gzip，不能同时保持旧默认断言原样；允许改这些精确期望及签名，不放宽安全/内容/失败断言。显式512 KiB/none回归继续保留；不保留旧整树编码作为生产双路径。 |
| D6 | 有限 grace 不能强制终止任意 OS blocking IO；超时报告 Pending、保留 owner、资源不关闭，重新 shutdown 才能确认 join。若要求“任何磁盘故障都在60s内完全退出且无任务”，需独立进程隔离，是新增架构范围，不能由实现 agent 擅自承诺。 |
| D7（已由主 agent 裁决） | Cargo.lock 为生成文件，不适用 STD-SIZE-001 源码行数上限；不存在“所有修改文件≤1000行”的用户要求。新增直接依赖边由 Cargo 正常更新 lock，必须保持 `--locked` 校验通过（即更新后提交新的 lock 一致性；指后续实施交付，本次不执行 git commit）。P1d 无需额外批准，不拆锁、不伪造锁、不取消 --locked。 |
| D8（待用户裁决） | §8 R3 要求第二次 host shutdown 在受控端点恢复后可返回 `Complete` 或已完成移交的 `PersistedPending`；实测不可达：`Batcher` 终态在 worker join 后冻结（`langfuse-client/src/batcher.rs::shutdown`：重复调用返回同一终态、不重发事件）。证据 `peri-acp/src/host/telemetry_test.rs::host_shutdown_reenters_after_telemetry_pending`：第二次关闭确实重入 `finish`（部署关闭权调用 2 次），但交付尝试数不变、返回值与第一次相同且非 Complete。裁决项：按「终态不可变」修订 §8 R3 期望，或另行要求可续传的第二次交付（当前实现不承诺，属新增范围）。 |
| D9（待裁决：维持或拆分） | §7/§8 自设文件行数上限被 9 个文件超出，实测（`wc -l`，文档与验收表同步流）：`batcher/recovery_test.rs` 957/900、`client_export_test.rs` 988/900、`batcher/recovery.rs` 579/500、`batcher/shutdown.rs` 434/350、`batcher.rs` 523/500、`batcher/worker.rs` 664/650、`batcher/failure.rs` 356/350、`client_send.rs` 510/500、`tracer/input_snapshot.rs` 488/400。均低于仓库硬上限 1000 行，`scripts/check-file-size.sh` 报的超限文件不含本变更集；本轮不动代码，清单与逐项差值见实施报告 §8.3。收敛轮（2026-10-12 20:00 起）已按职责拆分 `batcher/{recovery.rs,failure.rs,worker.rs}`（579/356/664 → 441/292/521，只搬移不改行为，见实施报告 §9.2）；`batcher/recovery_test.rs` 957（夹具耦合，未拆）、`tests/spool_recovery.rs` 916 与其余 5 个他流文件仍超自设上限，待裁决。收敛轮二（2026-10-12 21:00 起）实测 `wc -l` 复核：`client_export_test.rs` 988/900、`batcher/recovery_test.rs` 957/900、`tests/spool_recovery.rs` **983**/900（+67 来自跨进程探针步进度插桩）、`batcher.rs` 523/500、`batcher/worker.rs` 521/650、`client_send.rs` 510/500、`batcher/shutdown.rs` 434/350、`tracer/input_snapshot.rs` 488/400；拆分后已回到上限内的：`batcher/recovery.rs` 441/500、`batcher/failure.rs` 292/350、`spool/native_test.rs` 841/900（原 971，取锁语义用例移入新 `spool/native_test/lock.rs` 188/900）。本变更集内文件全部低于仓库硬上限 1000；仍超自设上限的 3 个他流文件 + 2 个测试文件待裁决。 |
| 验证闸门 | reqwest stream 在锁定版本/emscripten 是否可用、gzip实际分配和框架缓冲上界必须由 P1d/P2/P5 实测；不足时报告 blocked，不自选整包缓冲冒充流式。真实端点权限/地址/测试窗口由用户提供后执行；不从开发者环境偷读凭据。 |

D7 已裁决，Cargo.lock 豁免源码行数上限；除真实验收环境外，无需用户再选择端口位置、默认预算、Pending 命名或文件归属。本文冻结接口，实现流已按 §7 分阶段交付完成（P1–P5，状态见 §1 与实施报告）；除 D8/D9 待裁决外不改变冻结签名/owner/计量口径，若要变更须先更新本契约并由集成负责人串行交接，禁止各 agent 自行创造不同版本。

## 11. 修订记录

- 2026-10-12｜MUST-FIX 8（历史返修记录，已由本次冻结结论取代）：当时撤回无条件实施表述并等待 D7 裁决；现行状态及放行规则以 §1、§7.1 和已裁决 D7 为准。
- 2026-10-12｜MUST-FIX 1：新增 §2.1/§2.2，列明本次实际读取的源码/规范/计划范围、14 组真实签名的现状→目标→消费者、行号及 wc 基线；区分静态核验与未执行的编译/运行验证，保留未涉及结论与安全约束。
- 2026-10-12｜MUST-FIX 2：为 send_payload 增加 AttemptCheckpoint 通道与 CheckpointFailed；冻结每次 attempt 的持久化→构建→发送顺序、claim 的 blocking owner、失败零发送与累计 attempts 口径。
- 2026-10-12｜MUST-FIX 3：retry_wait 改为活跃 owner 保持 claim；仅关闭排空后显式移交，原 seq 固化 HandedOffPending，禁止 NotFound 推断成功，并区分崩潰恢复与正常移交。
- 2026-10-12｜MUST-FIX 4：增加 CleanupRegistry/Registration、SpoolObject 注册字段及 create/writer 参数；明确有界 slot 分配、锁转移、try_new/准入失败和迟到产物清理、删除后回收及关闭持有者协议，禁止 Drop IO/隐藏 spawn。
- 2026-10-12｜MUST-FIX 5：将工作区最小校验改为实际隔离的进度分区，准入同时预约输出 allowance；冻结预算不足立即拒收/不互等规则，增加事件及全部来源的预算身份校验和 BudgetMismatch。
- 2026-10-12｜MUST-FIX 6：为 freeze 增加独立 prepare_seq 与双水位 FlushBoundary，明确尚未 end 的准备屏障、后缀隔离与准备失败；补三种背压策略对 preparing/ready/sending、input/file 的可驱逐矩阵与完整回收规则。
- 2026-10-12｜MUST-FIX 7：DeliveryCounts/结果类型与 SessionLike 声明前置 P1，拆开声明冻结与真实运行期切换；重排 P3a→P3c→P3b，给出无环任务图、公共模块树/测试挂载交接和逐阶段编译门禁，禁止空实现假交付。
- 2026-10-12｜MUST-FIX 8（端口/构造，历史返修记录，已由本次冻结结论取代）：资源实现限定 crate 内 sealed 端口，新增受控 ID/cursor/SpoolObject 构造及可见性；当时的 D7 人工闸门现已取消，Cargo.lock 按生成文件处理，阶段编译与生命周期验证要求仍保留。
- 2026-10-12｜MUST-FIX 2–8（验证）：§8 追加逐条生命周期反例和精确断言，覆盖 checkpoint 失败零发送、跨进程接管/原前缀、准入失败清理、近耗尽与异预算、freeze 未 end 的 flush、三策略驱逐、公共接口真实编译；冻结待实施／待执行验证，签名尚未落地、测试尚未执行。

- 2026-10-12｜R1–R5 定向冻结修订（R6 记录；行号指本次修订后的本文）：
  - R1：§3.2 第 106–129 行改为借用式 delete，冻结失败归槽/持锁、成功释放协议；§5.2 第 326–327 行同步终态表；§8 第 429 行增加跨进程拒绝 claim 与原 owner 删除重试断言。
  - R2：§7 第 365–413 行前置 ObjectId/SpoolError 至 P1a 同单元挂载，P2a/P2b 原子交付，明确 session.rs/client.rs 与公共文件写锁；§7.1 逐交接点声明 crate/target、暂缓调用点及解除阶段；§8 第 431 行同步编译断言。
  - R3：§3.6 第 277 行记录已核对的 AcpHostHandle::shutdown（源码 67–102、关键分支 85–91）与 HostExitContext::finish（153–189、映射 172–175）；§8 第 430 行要求跨两次真实 host shutdown，Pending 不缓存、完成移交的 PersistedPending 才可缓存。
  - R4：§1/§2 第 9、33、60 行，§7 第 369、380、395 行及 §10 第 487–490 行取消 D7 人为闸门，要求 Cargo 正常更新并交付一致的 lock、保持 --locked 校验。D7 由主 agent 裁决：Cargo.lock 为生成文件，不适用 STD-SIZE-001。
  - R5：§1 第 5–9 行、§2 第 33–64 行、§7 第 369–413 行、§8 第 419 行、§9 第 458–475 行、§10 第 487–490 行及 §11 第 494–503 行统一冻结待实施／待执行验证；旧返修记录标明已被取代，保留真实验证缺口、未确认关闭/删除/交付语义、本轮范围限制与暂停控制。

- 2026-10-12｜文档与验收表同步流（本轮）：
  - §1 状态改为「已实施，待用户验收」，并登记 P4b 交付后的实测闸门（workspace `--all-targets` EXIT=0、`peri-acp --lib` 827 passed、WASM 目标 EXIT=0，见实施报告 §2.1）。
  - §9 生命周期行、平台差异行、文件流重试行按本轮重跑回填：host 五项用例已建（含跨两次 shutdown 重入），WASM 编译面由「未通过」改为「EXIT=0，宿主行为仍未测」，`tests/spool_http.rs` 三项集成用例实测 3 passed；长会话/512 MiB 积压 RSS/断网 10 分钟/崩溃注入/性能补表仍保持未测或部分覆盖。
  - §10 新增 D8（§8 R3 的第二次 shutdown 期望被实测证伪，待用户裁决）与 D9（§7/§8 自设行数上限的 9 项超限清单，实测数据见实施报告 §8.3）。
  - 同步修正 `docs/code-index/{langfuse-client,peri-controller,peri-config}.md` 的过时描述（grace 归属、delete/移交两条 disarm 路径、`ingestion_events_to_otel` 为 `#[cfg(test)]` 物化回归入口、宿主装配入口）。本轮未改任何源码或 manifest。
- 2026-10-12｜集成收敛轮（本轮）：§7 新增「P4a 拆分（收敛轮）」行、§10 D9 补拆分进展、§11 本条；代码只做按职责拆分（`batcher/{recovery.rs,failure.rs,worker.rs}` 579/356/664 → 441/292/521，新增 `batcher/{recovery/scan.rs,failure/barrier.rs,worker/recover.rs}`）与 `cargo fmt -p {langfuse-client,peri-config,peri-controller,peri-acp}`，未改冻结签名、未删或弱化测试。命令证据、仍红项（`--test spool_recovery` 删除竞争不稳定，根因为 `spool/native*.rs` 把瞬态 ENOENT/EINVAL 透出为硬错误）与跨流改动请求见实施报告 §9。
- 2026-10-12｜收敛轮二（本轮，P6 收敛轮之后由主 agent 直接修复；证据见实施报告 §10）：
  - §3.2 `spool/native.rs` 行补两项并发语义：能力探针对端持锁（`WouldBlock`）不降级为 `Unavailable`；`try_lock_file` 把「锁文件父目录已被并发删除」单独报为不可领取。
  - §7 新增「P1c 拆分（收敛轮二）」行；§8 P1 行补 `spool/native_test/lock.rs` 的三条用例名；§10 D9 补本轮 `wc -l` 复核。
  - 代码改动限于 `langfuse-client/src/spool/native.rs`（`LockAttempt::{Free,Held,Gone}` 分流与 `lock_parent_is_dir`；能力探针竞争判定）、`client.rs`（`direct_budget` 取代 `max_wire_bytes + limit×2`）、`snapshot.rs`/`memory.rs` 的可见性、`tests/spool_recovery.rs` 的探针步进度插桩与新测试文件 `spool/native_test/lock.rs`；**未改任何冻结签名、端口、owner 或计量口径**，未删或弱化既有测试。
  - 同步修正 `docs/code-index/langfuse-client.md`（native 行、测试文件清单）与 `docs/standards/architecture-contracts.md`（ARC-TELEMETRY-SPOOL-001 的装配状态由「尚未编译通过」更正为已接线 + 失败降级语义）。
- 2026-10-12｜CI 修复（收敛轮三，PR #189 首次 CI 失败后由主 agent 修复；证据见实施报告 §11）：
  - 根因：`serde_json` 的对象底层随构建期 `preserve_order` feature 变化——workspace 全量构建经 `agent-client-protocol` 启用该 feature（保插入序），单 crate `-p langfuse-client` 构建不启用（BTreeMap 排序）；同一份事件因此编出不同字节，CI 的 `frozen_baseline_bytes_are_stable` 及 metadata 展开断言失败。`HashMap` 字段的随机迭代序是同类隐患。
  - 修复：新增 `stable_json.rs`（§3.4）把键序显式化；`encoding/mod.rs` 的 `FrozenInput::Value`、`snapshot.rs` 的 `ValueSource::write_json`、`types/conversion/{metadata.rs,generation.rs}` 的 JSON 属性与 map/展开输出全部改经它。
  - §7 新增「P2a CI 修复」行、§8 补 `stable_json_test.rs` 用例；代码未改任何事件/types 字段类型、冻结签名、端口或计量口径，未删或弱化既有测试。
  - `tests/spool_acceptance.rs` 的 P5 期望字节改由该文件内的稳定口径 helper（递归重排键后序列化）生成；判据仍是「线上 attr = 盘上对象 = 期望字节」三者逐字节相等，未放宽为解析等价。
- 2026-10-13｜CI 修复续（收敛轮三续，Windows job 失败后由主 agent 修复；证据见实施报告 §11.4/§11.5）：
  - 根因：CI run `38336790068` 的 Windows job 两处失败同源——`LockFileEx` 报 `ERROR_LOCK_VIOLATION`(33) 时 `io::ErrorKind` 为 `Uncategorized`（非 `WouldBlock`），`try_lock_file` 把它降级成 `Io { operation: "lock spool object" }`（`claim_is_exclusive_across_processes` 失败）；另一处是该用例的覆盖断言依赖 OS 删除语义（Windows 占用句柄使 `remove_dir_all` 失败，条目留不到扫描可见，`corrupt_pages > 0` 失败）。
  - 修复：§3.2/§7/§8 同步；`is_lock_contention` 统一「对端持锁」判定形态，覆盖断言改为确定性铺条（前置写入摘要不符条目并断言 `Err(Corrupt)`），原有断言全部保留。**未改任何冻结签名、端口、owner 或计量口径**，未删或弱化既有测试。
  - 另记：本机 `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1`（仅本地会话环境，仓库与 CI 均无此变量）会让 `peri-workflow` 三个 fail-closed 用例失守，属环境噪声；去掉该变量后 `-p peri-workflow --lib` 177 passed。
- 2026-10-13｜CI 修复续二（收敛轮四，PR #192 的 ubuntu job 失败后由主 agent 修复；证据见实施报告 §12）：
  - 失败面：CI run `38394041455` 的 ubuntu job 只在 `concurrent_delete_never_turns_lock_attempts_into_io_faults` 红（`langfuse-client/src/spool/native_test/lock.rs:155:17`，`lock spool object … No such file or directory (os error 2)`），`gh run rerun --failed` 同点复现；该变更集未触碰 `langfuse-client`（代码来自 main 的 PR #189）。
  - 根因：旧判定在 `open` 失败**之后**再探测父目录（`lock_parent_is_dir`，语义「父目录仍在 ⇒ 当真故障上抛」）；探测与失败之间的 TOCTOU 让并发删除的瞬态 `ENOENT` 逃逸为硬故障，契约 §5.2 要求分开的两类被重新混同。
  - 修复：§3.2/§7/§8/§11 同步；判定改为按**本次 `open` 的错误**分类（`NotFound`；非 Windows 平台并入 macOS 实测的 `InvalidInput`），删除事后探测 `lock_parent_is_dir`；`spool/native_test/lock.rs` 新增确定性回归钉住该规则。**未改任何冻结签名、端口、owner 或计量口径**，未删或弱化既有断言，未新增 `#[allow]`/`#[ignore]`。
  - 残余：锁文件位置被外部替换成悬挂符号链接时按「条目不可领取」处理（私有目录内不产生该形态，移除条件见实施报告 §12.4）；Linux 侧最终确认由 PR #192 的 ubuntu job 给出。**（已确认：run `38398854149` 的 ubuntu job 通过。）**
- 2026-10-13｜CI 修复续三（收敛轮五，PR #192 的 Windows job 失败后由主 agent 修复；证据见实施报告 §13）：
  - 失败面：CI run `38398854149` 的 Windows job（`115301056256`）只在 `concurrent_delete_never_turns_lock_attempts_into_io_faults` 红（`langfuse-client/src/spool/native_test/lock.rs:155:17`，`lock spool object … Access is denied. (os error 5)`）；同轮 ubuntu（收敛轮四的修复已生效）与 macOS 通过。
  - 根因：Windows 的并发删除形态是 `ERROR_ACCESS_DENIED`(5)——删除中的目录 / 条目（delete-pending）让 `open(O_CREAT)` 报拒绝访问；收敛轮四改成按错误分类后未收该形态，瞬态拒绝被升级为 `lock spool object` 硬故障。
  - 修复：§3.2/§7/§8/§11 同步；`is_concurrent_dir_removal` 增 `#[cfg(windows)]` 的原始码判定（`Some(5)`）；`spool/native_test/lock.rs` 新增 `#[cfg(windows)]` 确定性回归。**未改任何冻结签名、端口、owner 或计量口径**，未删或弱化既有断言，未新增 `#[allow]`/`#[ignore]`。
  - 残余：Windows 错误面无法区分 delete-pending 与真实 ACL 拒绝（私有 spool 目录内不产生该形态，移除条件见实施报告 §13.4）；Windows 侧最终确认由 PR #192 的新一轮 Windows job 给出。
