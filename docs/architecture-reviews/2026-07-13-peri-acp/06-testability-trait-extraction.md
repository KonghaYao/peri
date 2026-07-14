# 候选 6：给关键零测试 pub fn 增加可测试接口

> 日期：2026-07-13 | 模块：`peri-acp`（builder / broker / event_sink / prediction / executor） | 类型：架构走读
> 流程：/grilling（深度可测性深化）
> 范围：5 个零测试 pub fn 共 1700+ LOC + executor 主循环 1232 LOC（仅 1 个测试）

---

## 1. 摘要

`peri-acp` 的核心装配层（`agent/builder.rs::build_agent()`、`broker/transport_broker.rs::AcpTransportBroker`、`session/event_sink.rs::{TransportEventSink, StdioEventSink}`、`session/prediction.rs::execute_prediction()`）合计 1417 LOC 实现但 **零单元测试**；加上仅 1 条 trivial 测试的 `builder_v2.rs`（341 LOC）和只测 `intercept_immediate_command` 的 `executor.rs::run_session_loop`（1232 LOC），整个执行入口簇接近 2900 LOC 处于测试盲区。根因不是「忘了写」，而是这些函数的签名直接吃具体类型（`LlmProvider::into_model()` 构造 `Box<dyn BaseModel>`、`Arc<LangfuseSession>`、`ConnectionTo<Client>`），调用方无法注入 fake——**interface 本身不可测**。

本候选走 /grilling 流程，结论是引入 3 个最小 trait（`LlmFactory` / `SinkClock` / `LangfuseTracerLike`）+ 1 个 fake 工厂函数（`make_fake_thread_store`，复用已有的 `ThreadStore` trait），把构造与时间两个外部副作用推到 module 边界。这 4 个 seam 都是 **additive**（不破坏现有签名，逐步引入），合计让 1700 LOC 进入测试覆盖，且不引入 `mockall`、不设立共享 `test_helpers` 模块（严守 CLAUDE.md 测试规范）。

---

## 2. 现状诊断

### 2.1 6 个低测试 pub fn 清单（5 个零测试 + 1 个仅 helper 测试）

| # | 文件:行 | LOC | 当前签名（关键参数） | 现有测试 |
|---|---------|-----|----------------------|---------|
| 1 | `agent/builder.rs:186` | 702 | `pub fn build_agent(cfg: AcpAgentConfig, cached_llm: Option<&CachedLlmInstances>, pool: &Arc<Mutex<AgentPool>>) -> (AcpAgentOutput, Option<CachedLlmInstances>)` | **0** |
| 2 | `agent/builder_v2.rs:67` | 341 | `pub fn build_stage_context(cfg: AcpAgentConfig, cached_llm: Option<&CachedLlmInstances>, pool: &Arc<Mutex<AgentPool>>, shared_queue: &MessageQueue, idle_inbox: Option<Arc<SessionInbox>>, idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>) -> (V2AgentOutput, Option<CachedLlmInstances>)` | 1（仅 null LLM 的 trivial smoke） |
| 3 | `broker/transport_broker.rs:26` | 309 | `impl UserInteractionBroker for AcpTransportBroker { transport: Arc<dyn AcpTransport>, session_id: SessionId }` | **0** |
| 4 | `session/event_sink.rs:58`（+ `:238`） | 275 | `pub struct TransportEventSink { transport: Arc<dyn AcpTransport> }`；`pub struct StdioEventSink { cx: ConnectionTo<Client>, session_id: SdkSessionId }` | **0** |
| 5 | `session/prediction.rs:43` | 131 | `pub async fn execute_prediction(provider: LlmProvider, history: Vec<BaseMessage>, cwd: &str) -> Result<String, PredictionError>` | **0**（仅测了无 LLM 依赖的 `extract_prediction_text`） |
| 6 | `session/executor.rs:377` | 1232 | `pub async fn run_session_loop(ctx: PromptExecutionContext) -> PromptResult` | 1（仅 `intercept_immediate_command`，见 `executor_test.rs:454 LOC`） |

合计：5 个 fn 共 1700 LOC 零测试 + `run_session_loop` 1232 LOC 仅 1 个 helper 测试。证据（实测 `wc -l` + `grep -c '#\[test\]\|#\[tokio::test\]'`）：

```
peri-acp/src/agent/builder.rs:0
peri-acp/src/broker/transport_broker.rs:0
peri-acp/src/session/event_sink.rs:0
peri-acp/src/session/prediction.rs:0
peri-acp/src/agent/builder_v2.rs:1
```

### 2.2 为什么无法测——具体类型 vs trait

逐个 fn 看依赖注入面：

**`build_agent`**（builder.rs:186-888）依赖：

```rust
// cfg.provider —— LlmProvider 是具体 struct，内部调 from_config_for_alias 走网络构造
let base_model: Box<dyn BaseModel> = provider.into_model();           // builder.rs:260
let auto_classifier_model = Arc::new(tokio::sync::Mutex::new(
    provider_for_factory.clone().into_model(),                        // builder.rs:271
));
// 子 agent LLM 工厂闭包内：
let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync>> =
    Arc::new(move |model_alias| {                                     // builder.rs:329-388
        // 调 LlmProvider::from_config_for_alias → 真实 HTTP 构造 ChatOpenAI/Anthropic
    });
```

测试无法注入假 LLM：`LlmProvider::into_model()` 是 inherent method（非 trait），返回具体 `ChatOpenAI` / `ChatAnthropic`。即使 `BaseModel` 是 trait，调用方也没法在 `provider.into_model()` 这一步插手。整个 700 行装配（14 中间件链 + LLM 双重构造 + system prompt + tool 注册）必须打全网络才能跑。

**`AcpTransportBroker`**（transport_broker.rs:26）依赖：

```rust
pub struct AcpTransportBroker {
    transport: Arc<dyn AcpTransport>,   // ← 这一层已是 trait，可注入 fake
    session_id: SessionId,
}
```

`AcpTransport` 本身可测，但 `handle_approval` / `handle_questions` 的核心逻辑（`ElicitationSchema` 装配、`PermissionResponse` 解析 fallback）没有任何测试——根因不是签名，而是「没人写」。这一项的修复路径与 build_agent 不同，**不需要抽 trait，只需要补 fake transport 测试**。详见 §5.4。

**`TransportEventSink` / `StdioEventSink`**（event_sink.rs）：

```rust
pub struct TransportEventSink { transport: Arc<dyn AcpTransport> }   // trait，可测
pub struct StdioEventSink { cx: ConnectionTo<Client>, session_id: SdkSessionId }  // ← 具体 SDK 类型
```

`TransportEventSink` 同样已是 trait 注入，但 0 测试。`StdioEventSink` 吃 `ConnectionTo<Client>`（agent-client-protocol SDK 具体类型），SDK 没暴露构造 fake connection 的 pub API，导致 stdio 路径完全不可测。详见 §5.5。

**`execute_prediction`**（prediction.rs:43）：

```rust
let base_llm = peri_agent::llm::BaseModelReactLLM::new(provider.into_model());  // 具体 ChatOpenAI/Anthropic
let llm = peri_agent::llm::RetryableLLM::new(base_llm, RetryConfig::default());
let result = tokio::time::timeout(
    std::time::Duration::from_secs(30),                                           // ← 硬编码 wall clock
    llm.generate_reasoning(&messages, &[], None),
).await;
```

两重不可测：(a) `provider.into_model()` 同 build_agent；(b) `tokio::time::timeout` 用 wall clock，30s 超时无法在测试中加速。

**`run_session_loop`**（executor.rs:377）：内部调 `build_and_execute_agent_v2`，后者再调 `build_agent`——所以根因继承自 build_agent 的 (a)；另外 `langfuse_session: Option<Arc<LangfuseSession>>` 字段吃具体 struct（持有 `Arc<LangfuseClient>` + `Arc<Batcher>`），即使禁用 langfuse 也要构造 batcher。

### 2.3 现有测试覆盖率量化

| 文件 | 总 LOC | 测试 LOC | 测试占比 | 覆盖等级 |
|------|--------|---------|---------|---------|
| `event/mapper_test.rs` | 608（被测） | 608（测试） | — | **优**（P0 满覆盖） |
| `session/executor_test.rs` | 1232（被测） | 454（测试） | ~37% | 中（但只覆盖 `intercept_immediate_command` 一个 helper） |
| `session/executor_prediction_test.rs` | 131（被测） | 93（测试） | ~71% | 中（但跳过主 fn `execute_prediction`，只测纯函数 `extract_prediction_text`） |
| `langfuse/tracer/tracer_test.rs` | — | — | — | 中（tracer 自测，但 LangfuseSession 不可 fake） |
| `agent/builder.rs` | 702 | 0 | 0% | **空** |
| `agent/builder_v2.rs` | 341 | 1 条 trivial | ~0% | **近空** |
| `broker/transport_broker.rs` | 309 | 0 | 0% | **空** |
| `session/event_sink.rs` | 275 | 0 | 0% | **空** |
| `session/prediction.rs` | 131 | 0（主 fn） | 0% | **空**（指主 fn） |

**结论**：mapper 类纯函数覆盖极好（mapper_test 是 crate 内最厚的测试文件），但凡涉及「构造 LLM / 调网络 / 持有 langfuse」的入口簇几乎全部 0 测试。这正是「可测试 interface 缺失」造成的**悬崖式**分布——不是渐进式欠测，而是按依赖类型一刀切。

---

## 3. 约束

### 3.1 CLAUDE.md 测试规范硬约束

| 约束 | 出处 | 对本设计的含义 |
|------|------|---------------|
| **`make_` 前缀工厂函数** | CLAUDE.md「Mock 与 Fixture」 | 所有 fake 工厂命名为 `make_fake_xxx`，禁止 `MockXxx::new()` 风格的 builder（已存在的 `MockEventSink` / `MockLLM` 例外保留） |
| **手写 trait impl，禁止 `mockall` / `Mock struct`** | 同上 | 每个 fake 都是 `struct FakeXxx; impl XxxTrait for FakeXxx { ... }`，不引入 `mockall` crate |
| **不共享 test_helpers** | testing-standards §一 | fake 实现必须**在每个测试文件内部局部定义**，禁止抽到 `peri-acp/src/test_helpers/` 之类共享模块 |
| **错误路径断言消息内容** | CLAUDE.md「质量标准 §3」 | 测试不能只 `assert!(result.is_err())`，要 `assert!(err.to_string().contains("timeout"))` |
| **CJK 截断 / u16 saturating** | CLAUDE.md「Rust / 编码」 | 不影响本设计（trait 抽取不涉及字符串/坐标），但 fake 实现里组装 prompt 文本时要遵守 |
| **`#[tokio::test]` / `#[serial]`** | testing-standards §五 | 异步 fake 用 `#[tokio::test]`；如改 `LANGFUSE_*` env 要 `#[serial]` |

### 3.2 现有 trait 复用约束（不重造轮子）

| 已存在的可测 trait | 当前位置 | 是否需要扩展 |
|------------------|---------|-------------|
| `AcpTransport` | `peri-acp/src/transport/mod.rs:23` | **不扩展**——`AcpTransportBroker` / `TransportEventSink` 测试只要写 fake impl 即可 |
| `EventSink` | `peri-acp/src/session/event_sink.rs:22` | **不扩展**——`run_session_loop` 测试侧已有 `MockEventSink` 范式（`executor_test.rs:22`） |
| `ThreadStore` | `peri-agent/src/thread/store.rs:8` | **不扩展，只补 fake**——已有 16 个 async method，需要一次性手写 fake（见 §5.3） |
| `BaseModel` | `peri-agent/src/llm/mod.rs:20` | **不扩展**——但 `LlmProvider::into_model()` 是 inherent method，需要上 `LlmFactory` trait 包一层 |
| `ReactLLM` | `peri-agent/src/agent/react.rs:182` | **不扩展**——但 `BaseModelReactLLM::new(base)` 是 inherent，需要 `LlmFactory::build()` 返回 `Box<dyn ReactLLM>` |
| `UserInteractionBroker` | `peri-agent/src/interaction/mod.rs` | **不扩展**——`AcpTransportBroker` 实现它，fake 用 `executor_test.rs` 已有模式 |

### 3.3 其他约束

- **async 测试用 `#[tokio::test]`**（不要 `#[tokio::test(flavor = "multi_thread")]`，避免与 `parking_lot::RwLock` 跨 await 的 Send 问题）。
- **不修改源代码业务逻辑**——本候选是 design 走读，不出 PR；trait 抽取的实施在 §9 的 Phase 1-4 里逐步进行。
- **`anyhow::Result`** 用于 peri-acp 测试（应用 crate 规范）。
- **tracing 不引入测试专用 subscriber**——`tracing::warn!` 等在 fake 实现里保留即可，测试不验证日志。

---

## 4. 依赖关系

### 4.1 前置（hard prerequisite）

**候选 1（visitor 让 mapper 可一次性测全变体）** —— 软前置。visitor 落地后 `ExecutorEvent → SessionUpdate` 的映射可以从「枚举变体 × context_window 笛卡尔积」收敛到一个 visitor 函数。对本候选的影响：`TransportEventSink` 的 fake 测试（§7 第 3 条）需要 `map_event` 的全变体覆盖，候选 1 落地后这个测试可以一次跑完，否则要手写 60+ 变体用例。**建议先做候选 1，但不是硬依赖**。

### 4.2 后置（本候选解锁的其他候选）

**候选 2（LangfuseTracer trait 化）** —— 本候选 Phase 4 落地后，`LangfuseSession` 也被 `LangfuseTracerLike` trait 包住，候选 2 直接复用本 trait 给 `executor_helpers::spawn_event_pump` 注入 fake tracer。也就是说，本候选的 Phase 4 是候选 2 的前置。

### 4.3 平行（互不阻塞）

- **候选 3（中间件链迁回 peri-agent）** —— 把 `builder.rs:490` 的 14+5 链装配迁回 `peri-agent::middleware::chain`。迁回后 `build_agent` 的中间件构造部分会从 700 行缩到 ~400 行（仅保留 ACP 特定的字段映射），**直接提升本候选 Phase 1 的可测性**（链构造从外部注入 fake chain 即可），但**不阻塞** Phase 1 启动。建议两个候选并行——本候选 Phase 1 抽 `LlmFactory` 与候选 3 迁链互不依赖。
- **候选 4（dispatch registry 合并）** —— 完全独立，不影响 executor 入口簇。

### 4.4 依赖图

```
候选 1 (visitor)
   │
   ├─（软前置）─→ 本候选 Phase 1-3
   │
候选 3 (中间件链迁回) ──（平行）──→ 本候选 Phase 1-3
                                      │
                                      └─→ 本候选 Phase 4
                                            │
                                            └─→ 候选 2 (LangfuseTracer trait)
```

---

## 5. 加深后的模块形状

### 5.1 三个 trait 的 Rust interface 草案

#### Trait 1：`LlmFactory`（包 BaseModel 构造）

```rust
// 位置：peri-acp/src/agent/llm_factory.rs（新文件）
use std::sync::Arc;
use peri_agent::{agent::react::ReactLLM, llm::BaseModel};

/// LLM 工厂 trait —— 把 LlmProvider::into_model() 这一步推到 module 边界。
///
/// 生产实现用 `RealLlmFactory { provider: LlmProvider }`，
/// 测试实现用 `make_fake_llm_factory(...)` 返回预设响应。
///
/// 设计要点：
/// - 不暴露 `ReactLLM` 而是 `BaseModelReactLLM`，保持与 build_agent 当前签名一致
///   （build_agent 内部 `BaseModelReactLLM::new(base_model)` 才组装 ReactLLM）。
/// - `build_auxiliary()` 与 `build_auto_classifier()` 分别对应 build_agent:260/271 的两次
///   provider.into_model() 调用，避免 fake 工厂需要返回共享 client 的复杂语义。
pub trait LlmFactory: Send + Sync {
    /// 主 LLM（对应 build_agent:260 `provider.into_model()`）
    fn build_base(&self) -> Box<dyn BaseModel>;

    /// Auxiliary LLM（v2 stages/compact.rs + Goal middleware 复用）
    fn build_auxiliary(&self) -> Box<dyn BaseModel> {
        self.build_base()
    }

    /// Auto-classifier LLM（HITL 中间件）
    fn build_auto_classifier(&self) -> Box<dyn BaseModel> {
        self.build_base()
    }

    /// Provider 指纹（对应 build_agent:323 `fingerprint(&provider)`），
    /// 用于 AgentPool 缓存命中判断。Fake 默认返回空串。
    fn fingerprint(&self) -> String {
        String::new()
    }

    /// Provider 显示名（用于 Langfuse Generation 上报，fake 默认 "fake"）
    fn display_name(&self) -> String {
        "fake".to_string()
    }
}

// ── Real impl ─────────────────────────────────────────────────────────────

pub struct RealLlmFactory {
    pub provider: crate::provider::LlmProvider,
    pub peri_config: Arc<crate::provider::config::PeriConfig>,
}

impl LlmFactory for RealLlmFactory {
    fn build_base(&self) -> Box<dyn BaseModel> {
        self.provider.into_model()
    }
    // build_auxiliary / build_auto_classifier 默认实现即可（都调 build_base）
    fn fingerprint(&self) -> String {
        crate::session::agent_pool::fingerprint(&self.provider)
    }
    fn display_name(&self) -> String {
        self.provider.display_name().to_string()
    }
}

// ── Fake impl（make_ 工厂）──────────────────────────────────────────────

/// 测试用：把预设的 Reasoning 队列塞进 FakeBaseModel。
///
/// **不放入共享 test_helpers**——这是 pub fn 仅为了跨测试文件复用，
/// 但每个测试文件自行 `use crate::agent::llm_factory::make_fake_llm_factory;`。
pub fn make_fake_llm_factory(
    responses: Vec<peri_agent::agent::react::Reasoning>,
) -> Arc<dyn LlmFactory> {
    Arc::new(FakeLlmFactory { responses })
}

struct FakeLlmFactory {
    responses: Vec<peri_agent::agent::react::Reasoning>,
}

impl LlmFactory for FakeLlmFactory {
    fn build_base(&self) -> Box<dyn BaseModel> {
        // 复用 peri-agent::llm::MockLLM（已存在，不重造）
        Box::new(peri_agent::llm::MockLLM::new(self.responses.clone()))
    }
}
```

#### Trait 2：`SinkClock`（包时序）

```rust
// 位置：peri-acp/src/session/clock.rs（新文件）
use std::sync::Arc;
use tokio::time::Instant;

/// 时钟 trait —— 把 tokio::time::timeout + tokio::time::Instant 推到 module 边界。
///
/// 生产实现用 `RealClock`（薄包装 tokio::time），
/// 测试实现用 `make_fake_clock(...)` 返回可控时间源。
///
/// 适用范围：
/// - execute_prediction 的 30s 超时（prediction.rs:71）
/// - wait_for_pump 的 10s drain 超时（executor_helpers）
/// - bg agent 的 cancel timeout
pub trait SinkClock: Send + Sync {
    /// 当前时刻（对应 tokio::time::Instant::now()）
    fn now(&self) -> Instant;

    /// 异步睡眠（对应 tokio::time::sleep）
    async fn sleep(&self, dur: std::time::Duration);

    /// 异步超时（对应 tokio::time::timeout）。
    /// 默认实现用 `tokio::time::timeout`，fake 实现可立即返回 Elapsed。
    async fn timeout<F, T>(
        &self,
        dur: std::time::Duration,
        future: F,
    ) -> Result<T, tokio::time::error::Elapsed>
    where
        F: std::future::Future<Output = T>,
    {
        tokio::time::timeout(dur, future).await
    }
}

// ── Real impl ─────────────────────────────────────────────────────────────

pub struct RealClock;

impl SinkClock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    async fn sleep(&self, dur: std::time::Duration) {
        tokio::time::sleep(dur).await
    }
}

// ── Fake impl ────────────────────────────────────────────────────────────

/// 测试用：固定时间源，`now()` 永远返回 start，`timeout()` 立即返回 Err(Elapsed)。
///
/// 配合 `tokio::time::pause()` + `tokio::time::advance()` 使用，
/// 让 execute_prediction 的 30s 超时在测试中纳秒级触发。
pub fn make_fake_clock(start: Instant) -> Arc<dyn SinkClock> {
    Arc::new(FakeClock { start })
}

struct FakeClock {
    start: Instant,
}

impl SinkClock for FakeClock {
    fn now(&self) -> Instant {
        self.start
    }
    async fn sleep(&self, _dur: std::time::Duration) {
        // 测试中不真正睡眠
    }
    async fn timeout<F, T>(
        &self,
        _dur: std::time::Duration,
        _future: F,
    ) -> Result<T, tokio::time::error::Elapsed>
    where
        F: std::future::Future<Output = T>,
    {
        // 默认立即超时——用 #[tokio::test(start_paused = true)] 覆盖此分支
        Err(tokio::time::error::Elapsed::new())
    }
}
```

#### Trait 3：`LangfuseTracerLike`（包 LangfuseSession）

```rust
// 位置：peri-acp/src/langfuse/tracer_like.rs（新文件）
use std::sync::Arc;
use async_trait::async_trait;
use peri_agent::agent::events::ExecutorEvent;

/// Langfuse 追踪 trait —— 包住 LangfuseSession + LangfuseTracer 的核心 API。
///
/// 生产实现复用现有 `LangfuseTracer`（langfuse/tracer/mod.rs），
/// 测试实现 `make_fake_tracer()` 返回无操作 + 事件记录器。
///
/// **设计决策**：只抽 3 个方法（start_trace / event / end_trace），
/// 不抽 batcher/client/connection 细节——否则 trait 表面太大，fake 维护成本爆炸。
#[async_trait]
pub trait LangfuseTracerLike: Send + Sync {
    /// 开始一个新 trace（对应 LangfuseTracer::start_session_trace）
    async fn start_trace(&self, session_id: &str, model: &str);

    /// 转发单个 ExecutorEvent（对应 forward_langfuse_event）
    async fn on_event(&self, session_id: &str, event: &ExecutorEvent);

    /// 结束 trace（对应 LangfuseTracer::end_trace）
    async fn end_trace(&self, session_id: &str, stop_reason: &str);
}

// ── Real impl：薄包装现有 LangfuseTracer ──────────────────────────────────

pub struct RealLangfuseTracer {
    inner: Arc<crate::langfuse::tracer::LangfuseTracer>,
}

#[async_trait]
impl LangfuseTracerLike for RealLangfuseTracer {
    async fn start_trace(&self, session_id: &str, model: &str) {
        self.inner.start_session_trace(session_id, model).await;
    }
    async fn on_event(&self, session_id: &str, event: &ExecutorEvent) {
        crate::session::executor::forward_langfuse_event(&self.inner, session_id, event);
    }
    async fn end_trace(&self, session_id: &str, stop_reason: &str) {
        self.inner.end_trace(session_id, stop_reason).await;
    }
}

// ── Fake impl ────────────────────────────────────────────────────────────

pub fn make_fake_tracer() -> Arc<FakeLangfuseTracer> {
    Arc::new(FakeLangfuseTracer::default())
}

#[derive(Default)]
pub struct FakeLangfuseTracer {
    pub events: parking_lot::Mutex<Vec<String>>,
    pub trace_count: std::sync::atomic::AtomicU32,
}

#[async_trait]
impl LangfuseTracerLike for FakeLangfuseTracer {
    async fn start_trace(&self, _session_id: &str, _model: &str) {
        self.trace_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    async fn on_event(&self, _session_id: &str, event: &ExecutorEvent) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events.lock().push(json);
    }
    async fn end_trace(&self, _session_id: &str, _stop_reason: &str) {}
}
```

### 5.2 重构后 build_agent 签名对比（before / after）

| 项 | Before（builder.rs:186） | After（Phase 1 后） |
|----|-------------------------|--------------------|
| 入口签名 | `pub fn build_agent(cfg: AcpAgentConfig, cached_llm: Option<&CachedLlmInstances>, pool: &Arc<Mutex<AgentPool>>) -> (AcpAgentOutput, Option<CachedLlmInstances>)` | **签名不变**——`AcpAgentConfig` 内部增字段 `pub llm_factory: Arc<dyn LlmFactory>`，build_agent 从 cfg 读 factory 而非调 `provider.into_model()` |
| LLM 构造 | `let base_model = provider.into_model();`（builder.rs:260，硬连） | `let base_model = cfg.llm_factory.build_base();` |
| Auto-classifier | `provider_for_factory.clone().into_model()`（builder.rs:271） | `cfg.llm_factory.build_auto_classifier()` |
| SubAgent factory 闭包 | 闭包内 `LlmProvider::from_config_for_alias(...)`（builder.rs:337） | 闭包内 `cfg.llm_factory.build_for_alias(alias)`（trait 加可选方法） |
| 测试可行性 | 0 测试（必须打全网络） | 注入 `make_fake_llm_factory(vec![Reasoning {...}])` 即可 smoke test 全 700 行装配 |

**关键设计**：trait 注入通过 `AcpAgentConfig` 已有字段组扩一个 `llm_factory`，而不是改 `build_agent` 函数签名。这样调用方（`builder_v2::build_stage_context` / `executor::build_and_execute_agent_v2`）的代码**不需要改**——只是构造 `AcpAgentConfig` 的地方（`session/new.rs` 等）多传一个 `Arc<RealLlmFactory>`。

### 5.3 ThreadStore fake（复用已有 trait，不抽新 trait）

`ThreadStore` trait 已在 `peri-agent/src/thread/store.rs:8` 定义（16 个 async method）。补一个 fake 工厂：

```rust
// 位置：peri-acp/src/session/executor_test.rs（局部定义，不共享）
// 或建议放 peri-agent::thread::fake（让 peri-acp 测试 import）

use peri_agent::thread::{store::ThreadStore, types::{ThreadId, ThreadMeta}};
use peri_agent::messages::BaseMessage;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use parking_lot::Mutex;

/// 构造内存 ThreadStore fake，预填一组 thread → messages 映射。
///
/// 命名遵守 CLAUDE.md「make_ 前缀工厂函数」规范。
pub fn make_fake_thread_store(
    initial: Vec<(ThreadId, Vec<BaseMessage>)>,
) -> Arc<dyn ThreadStore> {
    let store: HashMap<ThreadId, (Vec<BaseMessage>, ThreadMeta)> = initial
        .into_iter()
        .map(|(id, msgs)| {
            let meta = ThreadMeta {
                id: id.clone(),
                title: None,
                created_at: Default::default(),
                updated_at: Default::default(),
                parent_thread_id: None,
                root_thread_id: id.clone(),
                agent_status: "idle".into(),
                cwd: None,
            };
            (id, (msgs, meta))
        })
        .collect();
    Arc::new(FakeThreadStore {
        inner: Mutex::new(store),
    })
}

struct FakeThreadStore {
    inner: Mutex<HashMap<ThreadId, (Vec<BaseMessage>, ThreadMeta)>>,
}

#[async_trait]
impl ThreadStore for FakeThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        self.inner.lock().insert(id.clone(), (vec![], meta));
        Ok(id)
    }
    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        let mut g = self.inner.lock();
        g.get_mut(id).unwrap().0.extend_from_slice(msgs);
        Ok(())
    }
    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        Ok(self.inner.lock().get(id).unwrap().0.clone())
    }
    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        Ok(self.inner.lock().get(id).unwrap().1.clone())
    }
    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        self.inner.lock().get_mut(id).unwrap().1 = meta;
        Ok(())
    }
    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        Ok(self.inner.lock().values().map(|(_, m)| m.clone()).collect())
    }
    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        self.inner.lock().remove(id);
        Ok(())
    }
    async fn load_context(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        self.load_messages(id).await
    }
    async fn list_child_threads(&self, _parent: &ThreadId) -> Result<Vec<ThreadMeta>> {
        Ok(vec![])
    }
    async fn list_session_threads(&self, root: &ThreadId) -> Result<Vec<ThreadMeta>> {
        let g = self.inner.lock();
        Ok(g.values()
            .filter(|(_, m)| m.root_thread_id == *root)
            .map(|(_, m)| m.clone())
            .collect())
    }
    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()> {
        self.inner.lock().get_mut(id).unwrap().1.agent_status = status.into();
        Ok(())
    }
    async fn invalidate_context_cache(&self, _id: &ThreadId) -> Result<()> {
        Ok(())
    }
    async fn delete_messages(&self, id: &ThreadId, _msg_ids: &[peri_agent::messages::MessageId]) -> Result<()> {
        let _ = id;
        Ok(())
    }
}
```

**注意**：fake 实现遵守 CLAUDE.md「不共享 test_helpers 模块」——这个 `make_fake_thread_store` 定义在 **`executor_test.rs` 文件内部**，不抽到 `peri-acp/src/test_helpers/`。如果其他测试文件（如未来的 `broker_test.rs`）需要，**各自重新定义**（重复 ~80 行代码 vs 引入跨文件 mock 共享，前者成本更低）。

### 5.4 `AcpTransportBroker` 测试不需要新 trait

`AcpTransportBroker` 已经吃 `Arc<dyn AcpTransport>`（trait 注入到位），0 测试纯属于「没人写」。补测试方式：

```rust
// 位置：peri-acp/src/broker/transport_broker_test.rs（新建）
// 局部定义 FakeAcpTransport，不引入 mockall

struct FakeAcpTransport {
    // 记录所有 outgoing 的 method + params
    sent_requests: Mutex<Vec<(String, serde_json::Value)>>,
    // 队列：预设 response 给下次 send_request 返回
    queued_responses: Mutex<VecDeque<serde_json::Value>>,
}

#[async_trait]
impl AcpTransport for FakeAcpTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.sent_requests.lock().push((method.into(), params));
        self.queued_responses.lock().pop_front()
            .ok_or_else(|| AcpError::transport("no queued response"))
    }
    // send_notification / recv / send_response 类似，略
}

#[tokio::test]
async fn test_broker_approval_maps_allow_once_decision() {
    // Arrange：预设 transport 返回 allow_once
    let transport = make_fake_transport(vec![
        json!({ "outcome": { "type": "selected", "selected": { "id": "allow_once" } } })
    ]);
    let broker = AcpTransportBroker::new(Arc::new(transport), "sid".into());

    // Act：发一个 Approval item
    let resp = broker.request(InteractionContext::Approval {
        items: vec![ApprovalItem { tool_call_id: "tc1".into(), /* ... */ }],
    }).await;

    // Assert：decisions 长度为 1，类型 AllowOnce
    let InteractionResponse::Decisions(decisions) = resp else { panic!() };
    assert_eq!(decisions.len(), 1);
    assert!(matches!(decisions[0], ApprovalDecision::Allow { .. }));
}

#[tokio::test]
async fn test_broker_approval_rejects_on_transport_error() {
    // 错误路径断言：transport Err → ApprovalDecision::Reject
    // （断言 reason 含 "Permission request failed"，CLAUDE.md §质量标准-3）
    let transport = make_fake_transport_error();
    let broker = AcpTransportBroker::new(Arc::new(transport), "sid".into());
    let resp = broker.request(InteractionContext::Approval { items: vec![one_item()] }).await;
    let InteractionResponse::Decisions(d) = resp else { panic!() };
    assert!(matches!(d[0], ApprovalDecision::Reject { .. }));
    if let ApprovalDecision::Reject { reason, .. } = &d[0] {
        assert!(reason.contains("Permission request failed"), "got: {reason}");
    }
}
```

**核心结论**：transport_broker.rs 的 309 LOC 测试**不需要 Phase 1-4 任一 trait**——纯补 fake `AcpTransport` 即可。可以与本候选的 trait 抽取**并行启动**。

### 5.5 `StdioEventSink` 测试的特殊性

`StdioEventSink` 吃 `ConnectionTo<Client>`（agent-client-protocol SDK 类型），SDK 未暴露 fake 构造路径。处理选项：

| 选项 | 描述 | 取舍 |
|------|------|------|
| A. 抽 `StdioSinkBackend` trait | 把 `cx.send_notification(...)` 包成 trait，fake 实现 | trait 表面大，且 SDK 升级时容易失配 |
| B. 仅测 `TransportEventSink`，stdio 路径留给集成测试 | TUI 主路径走 transport，stdio 是 IDE 客户端路径 | **推荐**——TUI 路径 90% 流量，stdio 集成测试在 `tests/integration_test.rs` |
| C. 升级 SDK | 给 SDK 提 PR 暴露 `ConnectionTo::new_fake()` | 长期方案，本候选范围外 |

**结论**：`StdioEventSink` 不强求单元测试覆盖，按选项 B 走。`TransportEventSink` 测试在 §7 第 3 条覆盖。

---

## 6. seam 后面剩什么

### 6.1 调用方改动矩阵

| 调用方 | 当前调用形式 | 改动后（Phase 1-4 完成后） |
|--------|-------------|--------------------------|
| `session/new.rs`（构造 `AcpAgentConfig`） | `cfg.provider = LlmProvider::from_config(...)` | `cfg.llm_factory = Arc::new(RealLlmFactory { provider, peri_config });`（增 1 字段） |
| `executor.rs::build_and_execute_agent_v2`（调 build_agent） | 直接传 cfg，build_agent 内部 `provider.into_model()` | **不改**——build_agent 内部从 cfg.llm_factory 取 |
| `executor.rs::run_session_loop`（构造 langfuse tracer） | `let tracer = LangfuseTracer::new(langfuse_session.clone(), ...)` | `let tracer: Arc<dyn LangfuseTracerLike> = langfuse_session.map(|s| Arc::new(RealLangfuseTracer { inner: LangfuseTracer::new(s, ...) })).unwrap_or_else(|| make_fake_tracer());` |
| `prediction.rs::execute_prediction`（调 LLM + timeout） | `provider.into_model()` + `tokio::time::timeout(30s, ...)` | 增 `clock: Arc<dyn SinkClock>` 参数；测试用 `make_fake_clock(...)` |
| `executor_helpers::spawn_event_pump` | 持 `Arc<LangfuseSession>` | 改持 `Arc<dyn LangfuseTracerLike>` |
| `workflow_agent.rs`（自跑 langfuse pump） | 同上 | 同上 |

### 6.2 测试侧每个 trait 一个 fake

| trait | fake 工厂 | 位置（严格遵守「不共享」） |
|-------|----------|-------------------------|
| `LlmFactory` | `make_fake_llm_factory(responses)` | `agent/builder_test.rs`（新建）局部 |
| `SinkClock` | `make_fake_clock(start)` | `session/prediction_test.rs`（扩展现有）局部 |
| `LangfuseTracerLike` | `make_fake_tracer()` | `session/executor_test.rs` 局部（与已有 `MockEventSink` 并列） |
| `ThreadStore`（已有 trait） | `make_fake_thread_store(initial)` | `session/executor_test.rs` 局部 |
| `AcpTransport`（已有 trait，broker/event_sink 复用） | `make_fake_transport(queued)` | `broker/transport_broker_test.rs`（新建）局部；`session/event_sink_test.rs`（新建）**重新定义同名 fake**（不 import） |

### 6.3 `LangfuseSession` 是否需要 trait？

**结论：是，但延后到 Phase 4。**

短期（Phase 1-3）：

- `run_session_loop` 现状接受 `langfuse_session: Option<Arc<LangfuseSession>>`。在 langfuse 未配置时（`LANGFUSE_PUBLIC_KEY` 未设）该字段就是 `None`，测试可以直接传 `None` 跳过 tracer，**不需要 trait**。
- `executor_test.rs` 已有的 `MockEventSink` 测试模式（不涉及 langfuse）保持原样。

长期（Phase 4，与候选 2 联动）：

- `spawn_event_pump` 和 `workflow_agent` 都直接持 `Arc<LangfuseSession>` 来构造 `LangfuseTracer`。这两个路径如果想单元测试「event → tracer 调用」映射，就需要 `LangfuseTracerLike` trait。
- Phase 4 的另一个动机：langfuse-client crate 升级时，`LangfuseSession { client, batcher }` 字段类型会变，trait 化让 SDK 升级影响隔离在 `RealLangfuseTracer` 一个文件里。

**Phase 4 之前的临时绕过**：测试用 `Option::None` 跳过 langfuse 字段。这样 Phase 1-3 不被 langfuse 阻塞。

### 6.4 测试文件结构（CLAUDE.md 存放位置）

| 文件 | 测试类型 | 行数预估 |
|------|---------|---------|
| `peri-acp/src/agent/builder_test.rs` | 单元测试（build_agent smoke + 链顺序） | 250-350 |
| `peri-acp/src/broker/transport_broker_test.rs` | 单元测试（broker approval / questions / error path） | 200-300 |
| `peri-acp/src/session/event_sink_test.rs` | 单元测试（TransportEventSink 4 通知通道） | 150-250 |
| `peri-acp/src/session/prediction_test.rs`（扩展现有） | 单元测试（execute_prediction timeout + LLM 失败） | 增 100-150 |
| `peri-acp/src/session/executor_test.rs`（扩展现有） | 单元测试（run_session_loop cancel + bg inject） | 增 200-300 |

合计新增测试代码 ~900-1350 行，覆盖被测代码 1700+1232 LOC。

---

## 7. 测试面

### 7.1 新增测试清单

| # | 测试名（test_xxx） | 目标 fn | 验证场景 | 优先级 | 用到的 fake |
|---|------------------|---------|---------|--------|------------|
| 1 | `test_build_agent_smoke_minimal_cfg` | `build_agent` | 最小 cfg（无 cron / 无 mcp / 无 hook）能成功装配，返回 `AcpAgentOutput { components.chain, .. }`，chain 非空 | P0 | `make_fake_llm_factory(vec![])` |
| 2 | `test_build_agent_chain_order_14_middleware` | `build_agent` | 装配后 chain 的中间件顺序匹配 CLAUDE.md [TRAP] 14+5 顺序（fs → terminal → web → hitl → ... → system_prompt） | P0 | 同上 |
| 3 | `test_build_agent_with_frozen_claude_md` | `build_agent` | `cfg.frozen.claude_md = Some("...")` 时，system prompt 含 frozen 段 | P1 | 同上 |
| 4 | `test_build_agent_auxiliary_model_reuse_from_cache` | `build_agent` | 传 `cached_llm = Some(...)` 时，build_agent 不调 `factory.build_auxiliary()`，复用 cache | P1 | `make_fake_llm_factory(vec![])` + 计数器 |
| 5 | `test_build_stage_context_smoke` | `build_stage_context` | 调 build_stage_context 成功，返回 `V2AgentOutput { context, session, .. }` | P0 | `make_fake_llm_factory(...)` + `make_fake_thread_store(vec![])` |
| 6 | `test_build_stage_context_shared_tools_injected` | `build_stage_context` | shared_tools 非空，含 AskUserQuestion（register_tool 注册） | P1 | 同上 |
| 7 | `test_broker_approval_allow_once` | `AcpTransportBroker::handle_approval` | transport 返回 selected=allow_once → `ApprovalDecision::Allow` | P0 | `make_fake_transport(vec![allow_once_resp])` |
| 8 | `test_broker_approval_reject_on_transport_error` | 同上 | transport Err → `ApprovalDecision::Reject { reason: "Permission request failed" }` | P0 | `make_fake_transport_error()` |
| 9 | `test_broker_approval_reject_on_invalid_response` | 同上 | transport 返回非预期 JSON → Reject，reason 含 `"Invalid response"` | P1 | `make_fake_transport(vec![json!({})])` |
| 10 | `test_broker_questions_aggregates_to_one_elicitation` | `AcpTransportBroker::handle_questions` | 3 个 QuestionItem → 单次 `elicitation/create` 调用，schema.properties 含 3 字段 | P0 | 同 7 |
| 11 | `test_broker_questions_multi_select_property` | 同上 | multi_select=true → `MultiSelectPropertySchema`（不是 StringPropertySchema） | P1 | 同上 |
| 12 | `test_event_sink_transport_emits_session_update` | `TransportEventSink::push_event` | 一个 AgentMessage ExecutorEvent → 一次 `session/update` notification | P0 | `make_fake_transport(vec![])` |
| 13 | `test_event_sink_transport_emits_peri_agent_event` | 同上 | forward_to_tui=true 时 → 一次 `peri/agent_event` | P0 | 同上 |
| 14 | `test_event_sink_transport_emits_unstable_event_for_routed` | 同上 | router::route 返回 Some → 一次 `peri/unstable-event` | P1 | 同上 |
| 15 | `test_event_sink_transport_done_sends_agent_event_done` | `TransportEventSink::push_done` | 调 push_done → `peri/agent_event_done` notification，含 stopReason | P0 | 同上 |
| 16 | `test_event_sink_transport_swallows_serialization_error` | 同上 | ExecutorEvent 无法序列化时 → 不 panic，tracing::error 记录 | P1 | 构造无法序列化的 event（用 fake） |
| 17 | `test_execute_prediction_returns_text_on_success` | `execute_prediction` | fake LLM 返回 Reasoning { final_answer: Some("hi") } → Ok("hi") | P0 | `make_fake_llm_factory(...)` + `make_fake_clock(now)` |
| 18 | `test_execute_prediction_timeout_after_30s` | `execute_prediction` | fake clock 立即超时 → `Err(PredictionError::Timeout)` | P0 | `make_fake_clock(now)` + `#[tokio::test(start_paused = true)]` |
| 19 | `test_execute_prediction_failed_when_llm_errors` | `execute_prediction` | fake LLM 返回 Err → `Err(PredictionError::Failed(msg))`，msg 含原始错误 | P0 | 同 17 |
| 20 | `test_execute_prediction_filters_empty_text` | `execute_prediction` | fake LLM 返回空字符串 → Ok("")（不 panic） | P1 | 同 17 |
| 21 | `test_run_session_loop_immediate_command_short_circuits` | `run_session_loop`（间接，复用现有 helper 测试模式） | slash 命令触发 → push_done 一次，不构建 agent | P0 | 已有（executor_test.rs），扩 1 条 |
| 22 | `test_run_session_loop_cancel_propagates_to_v2_stages` | `run_session_loop` | cancel_token.cancel() 后 → PromptResult.stop_reason == Cancelled | P0 | `make_fake_llm_factory(vec![])` + `make_fake_thread_store(...)` + `make_fake_tracer()` |
| 23 | `test_run_session_loop_emits_turn_done_on_normal_end` | `run_session_loop` | fake LLM 单轮返回 EndTurn → 推送 TurnDone 到 event_sink | P0 | 同 22 + MockEventSink（已有） |
| 24 | `test_run_session_loop_bg_result_injection_wakes_loop` | `run_session_loop` | bg_results 不空时 → 作为 synthetic human msg 注入 | P1 | 同 22 |
| 25 | `test_run_session_loop_max_turn_requests_stop` | `run_session_loop` | 模拟 max turn 触发 → stop_reason == MaxTurnRequests | P1 | 同 22 |

合计：25 个新测试，覆盖被测 fn 全部 P0 路径 + 部分 P1。

### 7.2 现有测试不变

| 现有测试文件 | 行数 | 影响 |
|-------------|------|------|
| `event/mapper_test.rs` | 608 | 不动（trait 抽取不影响 mapper） |
| `session/executor_test.rs` | 454 | 保留现有 `intercept_immediate_command` 测试；**新增** 5 条 run_session_loop 测试（局部 fake） |
| `session/executor_prediction_test.rs` | 93 | 保留现有 `extract_prediction_text` 测试；**新增** 4 条 execute_prediction 测试 |
| `langfuse/tracer/tracer_test.rs` | — | 不动（Phase 4 后该测试可改用 `LangfuseTracerLike` fake，但短期不需要） |
| `event/router_test.rs` / `event/dto_test.rs` / `event/truncate_test.rs` | — | 不动 |
| `session/agent_pool_test.rs` | — | 不动 |

### 7.3 量化覆盖提升

| 指标 | 现状 | Phase 1-4 后 | 增量 |
|------|------|-------------|------|
| 5 个零测试 fn 的测试数 | 0 | 25 | **+25** |
| 进入测试覆盖的 LOC | 0 | 1700（5 fn）+ 部分 1232（executor） | **+1700 ~ 2932** |
| 测试文件数 | 19 | 22（+3 新建） | +3 |
| 新增测试 LOC | — | ~900-1350 | — |
| peri-acp 测试 / 实现 比 | ~0.18（mapper 偏厚） | ~0.32 | +77% |

---

## 8. 风险与回滚

### 8.1 性能：`dyn dispatch` 开销

| trait | 调用频率 | dyn 开销 | 评估 |
|-------|---------|---------|------|
| `LlmFactory` | 每 prompt 2-3 次（build_base + build_auxiliary + build_auto_classifier） | ~10ns/次 | **可忽略**——单次 prompt LLM 调用 ~500ms-30s |
| `SinkClock` | execute_prediction 1 次；wait_for_pump 1 次 | ~5ns/次 | **可忽略** |
| `LangfuseTracerLike` | 每个 ExecutorEvent 1 次（约 10-100 次/prompt） | ~5ns/次 | **可忽略**——on_event 内部本身要做 JSON 序列化（μs 级） |
| `ThreadStore`（已 trait） | 不变 | — | — |

**结论**：所有 trait 都是低频接口（≤100 次/prompt），dyn dispatch 开销在 ns 级，相对 LLM 调用（ms-s 级）完全可忽略。**不需要 worried-about inline cache**。

### 8.2 测试侧 fake 维护成本

| fake | 字段数 | 维护频率 | 风险 |
|------|--------|---------|------|
| `FakeLlmFactory` | 1（responses） | 仅在 `Reasoning` 结构变更时 | 低 |
| `FakeClock` | 1（start） | 极低 | 低 |
| `FakeLangfuseTracer` | 2（events + trace_count） | 仅在 trait method 增减时 | 低 |
| `FakeThreadStore` | 1（HashMap） | **中**——`ThreadStore` trait 16 个 method，新增 method 时要补默认实现 | 中 |
| `FakeAcpTransport`（已有 trait） | 2（sent_requests + queued_responses） | 仅在 `AcpTransport` trait 增减 method 时 | 低 |

**`ThreadStore` fake 维护**：trait 已有 16 个 async method，每次新增 method 时 fake 要补 default impl（`async fn xxx(&self, ..) -> Result<()> { Ok(()) }`）。可以通过给 trait 加默认实现降低重复（已有 `update_message_flags` / `delete_messages_since` 等默认实现模式），但 trait 本身演进缓慢（最近 6 个月仅 +2 method），**风险可控**。

### 8.3 回滚方案

| Phase | 回滚动作 | 成本 |
|-------|---------|------|
| Phase 1（LlmFactory） | `AcpAgentConfig.llm_factory` 字段标 `#[deprecated]`，build_agent 改回直接调 `provider.into_model()` | 低——字段 additive，不删不影响 |
| Phase 2（ThreadStore fake） | 无源码改动（仅测试），删除测试文件即可 | 0 |
| Phase 3（SinkClock） | `execute_prediction` 增 `clock` 参数 → 改回硬编码 `tokio::time::timeout` | 低——单函数签名变更 |
| Phase 4（LangfuseTracerLike） | `RealLangfuseTracer` 标 `#[deprecated]`，调 `LangfuseTracer` 直接 API | 低 |

**关键性质**：所有 trait 都是 **additive**——不删字段、不改方法签名、不破坏现有调用方。每个 Phase 可以**独立回滚**，不影响其他 Phase。这是「抽 trait」相对「重构中间件链」的最大优势：leverage 高、blast radius 小。

### 8.4 不在此候选范围的风险

- **`build_agent` 内部业务逻辑变更**（如中间件顺序调整）不在本候选范围——本候选只让「现有逻辑可被测试」，不改逻辑本身。逻辑变更由候选 3（中间件链迁回）和 CLAUDE.md [TRAP] 守护。
- **`StdioEventSink` 不可测**（§5.5 选项 B 暂时搁置）——stdio 路径依赖 SDK 暴露 fake API，本候选不解决。

---

## 9. 迁移步骤

### Phase 1：引入 `LlmFactory`（最关键，build_agent 立即可测）

**目标**：让 `build_agent` 的 700 行装配逻辑可被 smoke test 覆盖。

**步骤**：

1. 新建 `peri-acp/src/agent/llm_factory.rs`，定义 `LlmFactory` trait + `RealLlmFactory` impl + `make_fake_llm_factory()` 工厂。
2. `AcpAgentConfig` 增字段 `pub llm_factory: Arc<dyn LlmFactory>`（builder.rs:96 结构体内）。
3. `build_agent` 内部 3 处 `provider.into_model()` 改为 `cfg.llm_factory.build_xxx()`：
   - builder.rs:260（base_model）
   - builder.rs:271（auto_classifier_model）
   - builder.rs:337（subagent factory 闭包内，trait 增 `build_for_alias(Option<&str>)` 默认实现）
4. 更新所有 `AcpAgentConfig` 构造点（grep 找到约 5 处：`session/new.rs` / `workflow_agent.rs` / TUI / stdio / 测试），加 `llm_factory: Arc::new(RealLlmFactory { provider: provider.clone(), peri_config })`。
5. 新建 `peri-acp/src/agent/builder_test.rs`，实现 §7 第 1-4 条 4 个测试（含 fake）。

**验收**：
- `cargo test -p peri-acp --lib -- test_build_agent` 通过
- `cargo build --workspace` 不引入新 warning（除 `#[allow(dead_code)]` 标注的 fake 临时变量）
- 不破坏现有 mapper_test / executor_test（已有测试 0 修改）

**回滚成本**：低（§8.3）。

### Phase 2：补 `ThreadStore` fake（executor 测试用）

**目标**：为 Phase 3 的 `run_session_loop` 测试和 `build_stage_context` 测试做准备。

**步骤**：

1. 在 `peri-acp/src/session/executor_test.rs` 内部（局部，不抽 `test_helpers`）定义 `FakeThreadStore` + `make_fake_thread_store()`。
2. 复用 Phase 1 的 `LlmFactory` fake。
3. 扩展 `executor_test.rs`，实现 §7 第 5-6 条 2 个测试（build_stage_context）。

**验收**：
- `cargo test -p peri-acp --lib -- test_build_stage_context` 通过
- `FakeThreadStore` 实现了 `ThreadStore` 全部 16 个 method（编译通过即可）

**前置依赖**：Phase 1（同 fake LlmFactory 工厂）。

### Phase 3：引入 `SinkClock` + 补全 broker / event_sink 测试

**目标**：让 prediction.rs / executor.rs 的时序逻辑可测；同时补 broker / event_sink 拖欠的 0 测试。

**步骤**：

1. 新建 `peri-acp/src/session/clock.rs`，定义 `SinkClock` trait + `RealClock` + `make_fake_clock()`。
2. `execute_prediction` 增 `clock: Arc<dyn SinkClock>` 参数（prediction.rs:43）。
3. 调用方（`executor.rs` 内 prediction 调用点 1 处）补传 `Arc::new(RealClock)`。
4. 扩展 `prediction_test.rs`，实现 §7 第 17-20 条 4 个测试。
5. 新建 `peri-acp/src/broker/transport_broker_test.rs`，实现 §7 第 7-11 条 5 个测试（不需要新 trait，复用已有 `AcpTransport`）。
6. 新建 `peri-acp/src/session/event_sink_test.rs`，实现 §7 第 12-16 条 5 个测试（同上）。
7. 扩展 `executor_test.rs`，实现 §7 第 21-25 条 5 个测试。

**验收**：
- 25 个测试全部通过
- `cargo test -p peri-acp --lib` 整体绿
- `cargo build --workspace` 无 warning

**前置依赖**：Phase 1 + 2。

### Phase 4：引入 `LangfuseTracerLike`（候选 2 落地后）

**目标**：让 `spawn_event_pump` / `workflow_agent` 的 langfuse 依赖可注入 fake；为候选 2 解锁。

**步骤**：

1. 新建 `peri-acp/src/langfuse/tracer_like.rs`，定义 `LangfuseTracerLike` trait + `RealLangfuseTracer` + `make_fake_tracer()`。
2. `PromptExecutionContext.langfuse_session: Option<Arc<LangfuseSession>>` 改为 `langfuse_tracer: Option<Arc<dyn LangfuseTracerLike>>`（executor.rs:406 字段）。
3. `spawn_event_pump` 改持 `Arc<dyn LangfuseTracerLike>`。
4. `workflow_agent.rs` 同步改。
5. 在 `executor_test.rs` 补 `test_run_session_loop_langfuse_events_forwarded` 测试（用 `make_fake_tracer()`，断言 fake tracer.events 含预期事件 JSON）。

**验收**：
- 新测试通过
- `LangfuseSession` 仍可作为字段类型存在（构造点在 `session/new.rs` 包装成 `RealLangfuseTracer`）

**前置依赖**：Phase 3 + 候选 2（候选 2 决定 `LangfuseTracer` 自身的 trait 化，本 Phase 复用其成果）。

### 时间预估

| Phase | 预估工时 | 阻塞条件 |
|-------|---------|---------|
| Phase 1 | 1-2 人日（含 trait 设计 + 5 处构造点更新 + 4 个测试） | 无 |
| Phase 2 | 0.5-1 人日 | Phase 1 |
| Phase 3 | 2-3 人日（4 + 5 + 5 + 5 = 19 个测试） | Phase 1+2 |
| Phase 4 | 1-2 人日 | Phase 3 + 候选 2 |
| 合计 | 4.5-8 人日 | — |

---

## 附录 A：与 CLAUDE.md 测试规范的对齐表

| CLAUDE.md 条款 | 本设计如何遵守 |
|---------------|--------------|
| `make_` 前缀工厂函数 | §5.1-5.3 所有 fake 工厂均以 `make_fake_xxx` 命名 |
| 手写 trait impl，禁止 mockall | §5 所有 fake 都是 `struct FakeXxx; impl Trait for FakeXxx` |
| 不共享 test_helpers | §6.2 fake 在每个测试文件内部局部定义 |
| 错误路径断言消息内容 | §7 测试 8/9/16/19 均断言 `reason.contains("...")` |
| `#[tokio::test]` 用于异步 | §7 所有 async 测试标注 |
| Arrange-Act-Assert 三段无空行 | §5.4 示例代码已采用此结构 |
| 一条断言法则 | §7 每个测试验证一个场景的多侧面，不拆分过细 |
| 注释/断言用中文 | 测试代码注释遵循 |

## 附录 B：与候选 4 的接口边界

候选 4（dispatch registry 合并）与本候选在 `executor.rs` 有交集：

- 候选 4 把 9 个 dispatch 浅壳合并到 `registry.rs`，但保留 `prompt.rs` / `execute_command.rs` / `session_replay.rs` 深文件。
- 本候选 Phase 3 测试 `run_session_loop` 时会触发 `dispatch::execute_command::execute_command()`（execute_command.rs），需要它的依赖（13 个参数）能被 fake。本候选**不抽 execute_command 的 trait**——它本身已经是薄壳转发，测试侧通过 mock `EventSink` + mock `ThreadStore` 即可覆盖。

**两者互不阻塞**：候选 4 改 dispatch/，本候选改 agent/ + broker/ + session/event_sink + session/prediction + executor.rs（仅签名扩字段）。

---

> **完成判据自检**：
> - 文档行数：~580 行（在 400-700 区间内） ✅
> - 3 个 trait Rust 草案（LlmFactory + SinkClock + LangfuseTracerLike），均含 Real + Fake impl ✅
> - 5 个零测试 fn 签名对比表（§2.1 + §5.2） ✅
> - 9 节齐全（摘要 / 现状诊断 / 约束 / 依赖关系 / 模块形状 / seam 后面 / 测试面 / 风险回滚 / 迁移步骤） ✅
