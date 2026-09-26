//! V-02：host seam 证据（主 plan §6 V-02 行 / §8 第 2、3、7、13 行；sub-plan H §4/§5）。
//!
//! owner：V-02（`peri-acp/src/host/mcp_v4_builtin_test.rs`，模块名 `host::mcp_v4_builtin`；
//! 挂载点 `peri-acp/src/host/mod.rs`）。夹具复用 **V-06** 的
//! `host::mcp_v4_wire_fixture`（真实 `.mcp.json` + 真实 loader + 真实 pool + 真实
//! `run_session_loop` + 带 wire 日志与 `tools/call` 分支的 node 对端 + 能返回工具调用的
//! model 替身）。
//!
//! 本文件承担的断言面（全部落在**可观察能力面**，不落中间量）：
//!
//! 1. **首个 LLM 请求**：三个 IF-D5 冻结 effective name 直接可见、裸名不再出现（与
//!    acceptance §2 的迁移前基线逐项对照，A12）；用户 MCP 实例与两实例共存；
//!    两实例在 host seam 是真实连接且 `transport_type == "builtin"`（IF-D11）。
//! 2. **能力关闭**（IF-D10 面① / ARC-CAPABILITY-CLOSURE-001）：`WebMiddleware` /
//!    `ArtifactMiddleware` / 两者同时关闭时，首个请求的工具集合逐项变化，而 pool 级
//!    ready 事实不变（R8 的分层）。
//! 3. **`PERI_MCP_BUILTIN=off`**（A2）：两实例的工具既不在首个请求、也不在 ToolSearch
//!    摘要；**不是**回退到 middleware 旧实现（提供面已删除），且用户 MCP 不受影响。
//! 4. **启动 fatal 的用户可见投影**：模型 0 次调用、`ExecutionFailureKind::Internal`、
//!    ACP `-32000` + `data.kind = internal`、`TurnEnded(Error)`。
//! 5. **契约 6 BLOCKED 缺口复证**（A10/R19）：被提升为 direct 的工具走完整审批链，
//!    批准后 wire 上恰好一条 `tools/call`（`params.name` 为**裸名**），拒绝后 **0** 条。
//! 6. **关闭态差分**（acceptance §7 第 9 条的运行时证伪面 / §4 关闭面矩阵第 3、4 面）：
//!    `WebMiddleware=false` 下模型编造的 web 工具调用不触达审批、不触达 wire，以
//!    「未知工具」结算；同一 turn 内的可用工具是正控制（证明审批面/wire 面已装配）。
//! 7. **off 的判定面**（§7 第 4 条的运行时证伪面）：`PERI_MCP_BUILTIN=off` 只关注入，
//!    不改变 effective name ↔ 原始名的判定 parity（`default_requires_approval` /
//!    `is_edit_tool`；`is_mutation_tool` 是 crate 内私有 fn，由 peri-middlewares 侧覆盖）。
//!
//! 边界：本文件不实现生产代码；不修改 `mcp_v4_startup_test.rs` 的既有断言，也不修改
//! `mcp_v4_wire_fixture_test.rs` 的既有命题（只在那里补充 V-02 需要的 seam 与可见性）。

use std::{
    ffi::OsString,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use peri_acp_types::builtin_mcp::original_tool_name_of_effective;
use peri_acp_types::{
    event::{ExecutorEvent, TurnStatus},
    interaction::{
        ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
    },
    permission::{PermissionMode, SharedPermissionMode},
    session::ExecutionFailureKind,
};
use peri_agent::session::FrozenContext;
use peri_middlewares::permission::{default_requires_approval, is_edit_tool};
use serial_test::serial;

use super::{
    executor_flow_tests::MockEventSink,
    mcp_v4_wire_fixture::{
        run_wire_prompt, run_wire_prompt_with_frozen, ExtraServer, ScriptedToolCall,
        WireFixtureHarness, WireScriptedModel, WEB_ARTIFACT_CAPABILITIES,
        WIRE_FIXTURE_ECHO_EFFECTIVE_NAME, WIRE_FIXTURE_SERVER_NAME,
    },
};
use crate::session::executor::{FrozenSessionData, SessionContext};

// ── 夹具 ─────────────────────────────────────────────────────────────────────

/// `PERI_MCP_BUILTIN=off` 的进程级守卫（A2）。
///
/// env 是进程全局状态 ⇒ 本守卫必须与 `#[serial]` 一起使用（同进程内所有 `#[serial]`
/// 用例互斥，包括 `mcp_v4_startup_tests` 的 HOME 重定向组）。
struct BuiltinInjectionOff {
    previous: Option<OsString>,
}

impl BuiltinInjectionOff {
    const ENV: &'static str = "PERI_MCP_BUILTIN";

    fn set() -> Self {
        let previous = std::env::var_os(Self::ENV);
        std::env::set_var(Self::ENV, "off");
        Self { previous }
    }
}

impl Drop for BuiltinInjectionOff {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(Self::ENV, value),
            None => std::env::remove_var(Self::ENV),
        }
    }
}

/// 会话级冻结数据：只承载 `meta_harness.disabled_middlewares`（能力关闭的唯一输入）。
///
/// 与 `executor_flow_tests::frozen_with_dynamic_prompt_policy` 同形（该函数是私有测试
/// 夹具，跨模块不可见 ⇒ 按 IF-H1 的单 owner 约束在本文件内复刻，不改其宿主文件）。
fn frozen_with_disabled(disabled: &[&str]) -> FrozenSessionData {
    use peri_acp_types::meta_harness::MetaHarnessState;

    FrozenSessionData::from_frozen_parts(
        FrozenContext::builder()
            .system_prompt("V-02 host seam")
            .claude_md("")
            .skill_summary("")
            .date("2026-09-26")
            .meta_harness(MetaHarnessState {
                disabled_middlewares: disabled.iter().map(|name| (*name).to_string()).collect(),
                ..Default::default()
            })
            .build(),
        None,
    )
}

/// 受控审批 broker：记录收到的审批项（工具名 + 入参），按固定决策作答。
///
/// 审批证据的纪律（sub-plan H §6.1）：审批**看到的名字**与 wire 上的 `tools/call`
/// 是两个独立事实，本 broker 只提供前者；后者由夹具对端的 wire 日志提供。
#[derive(Clone, Copy, PartialEq, Eq)]
enum BrokerDecision {
    Approve,
    Reject,
}

struct RecordingBroker {
    decision: BrokerDecision,
    requests: AtomicUsize,
    seen: Mutex<Vec<(String, serde_json::Value)>>,
}

impl RecordingBroker {
    fn new(decision: BrokerDecision) -> Self {
        Self {
            decision,
            requests: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<(String, serde_json::Value)> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        match context {
            InteractionContext::Approval { items } => {
                self.requests.fetch_add(1, Ordering::SeqCst);
                self.seen.lock().unwrap().extend(
                    items
                        .iter()
                        .map(|item| (item.tool_name.clone(), item.tool_input.clone())),
                );
                InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| match self.decision {
                            BrokerDecision::Approve => ApprovalDecision::Approve { source: None },
                            BrokerDecision::Reject => ApprovalDecision::Reject {
                                reason: "v02 host seam reject".to_string(),
                                source: None,
                            },
                        })
                        .collect(),
                )
            }
            _ => InteractionResponse::Rejected,
        }
    }
}

/// 带受控 broker 的 session 装配面：审批链只在非 Bypass 模式下才会真正征询用户。
fn session_context_with_broker(
    harness: &WireFixtureHarness,
    session_id: &str,
    broker: Arc<RecordingBroker>,
) -> SessionContext {
    let mut ctx = harness.session_context(session_id);
    ctx.broker = broker as Arc<dyn UserInteractionBroker>;
    ctx.permission_mode = SharedPermissionMode::new(PermissionMode::Default);
    ctx
}

// ── 首个 LLM 请求：三个冻结 effective name + 用户 MCP 共存 ────────────────────

/// V-02 主断言（§8 第 1 行「迁移前基线」的迁移后一侧 +「能力关闭」行的正面对照）：
/// 真实生产装配路径下，首个 LLM 请求直接看到三个 IF-D5 冻结 effective name，
/// 且迁移前的裸名**不再**出现（acceptance §2.4 的逐项对照口径：恰有一者）。
///
/// 同时断言：用户 MCP 实例（`wire_fixture`）与两个 builtin 实例共存 —— 其 direct
/// 工具在同一请求中可见、未提升的同 server 工具保持 deferred；两实例是真实连接且
/// 传输分类为 `builtin`（IF-D11）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn builtin_instances_expose_frozen_effective_names_on_first_model_request() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    for instance in ["web", "artifact"] {
        harness.await_connected(instance).await;
        let infos = harness.pool().all_server_infos();
        let info = infos
            .iter()
            .find(|info| info.name == instance)
            .unwrap_or_else(|| panic!("{instance} 必须在 pool 的服务器清单内: {infos:?}"));
        assert_eq!(
            info.transport_type, "builtin",
            "{instance} 的传输分类必须是 builtin（IF-D11）: {info:?}"
        );
    }

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result = run_wire_prompt(
        harness.session_context("mcp-v4-builtin-first-request"),
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "builtin 注入不得使 prompt 失败: stop={:?} failure={:?}",
        result.stop_reason, result.failure
    );
    assert_eq!(model.call_count(), 1, "首个 prompt 恰好一次模型调用");

    let names = model.first_request_tool_names();
    let system = model.first_request_system_text();
    // 现场证据：acceptance 记录本次命令的输出（与 §2.3 基线逐项对照）。
    println!(
        "[V-02 迁移后] 首个 LLM 请求工具数 = {}；工具名 = {names:?}",
        names.len()
    );

    for (bare, effective) in WEB_ARTIFACT_CAPABILITIES {
        assert!(
            names.iter().any(|name| name == effective),
            "首个 LLM 请求必须直接含冻结 effective name `{effective}`（迁移前该 capability \
             由裸名 `{bare}` 提供）: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == bare),
            "迁移后不得再出现裸名 `{bare}`（可见性等价 = 恰有一者；重复暴露即红）: {names:?}"
        );
    }

    assert!(
        names.iter().any(|name| name == WIRE_FIXTURE_ECHO_EFFECTIVE_NAME),
        "用户 MCP 实例（system_mcp_tools: [\"echo\"]）的 direct 提升必须与 builtin 实例共存: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "mcp__wire_fixture__glob"),
        "未列入 system_mcp_tools 的同 server 工具必须保持 deferred: {names:?}"
    );

    // deferred 摘要面必须存在（否则「不在摘要里」是空断言）。
    assert!(
        system.contains("## Deferred Tools"),
        "首个请求必须带 ToolSearch 摘要段: {system}"
    );
    let deferred = direct_deferred_entries(&system);
    for (_, effective) in WEB_ARTIFACT_CAPABILITIES {
        assert!(
            !deferred.iter().any(|name| name == effective),
            "direct 工具不得同时出现在 deferred 条目里: {deferred:?}"
        );
    }
    println!("[V-02 迁移后] deferred 条目 = {deferred:?}");

    // 与 acceptance §2.3 的迁移前基线逐项对照：同一夹具、同一观察量、同一命令。
    // 基线 18 项；迁移后应仍为 18（三项由裸名换成 effective name，不增不减）。
    assert_eq!(
        names.len(),
        18,
        "首个请求的工具总数必须与迁移前基线一致（acceptance §2.3 记为 18；计数仅供对照）: {names:?}"
    );
    for (_, effective) in WEB_ARTIFACT_CAPABILITIES {
        assert_eq!(
            names.iter().filter(|name| *name == effective).count(),
            1,
            "`{effective}` 在首个请求中必须恰好出现一次（重复暴露即红）: {names:?}"
        );
    }
    assert!(
        !deferred.is_empty(),
        "deferred 条目必须非空（否则「三个 direct 工具不在 deferred」是空断言）: {system}"
    );
}

/// `## Deferred Tools` 段的条目名（`- <name>: <description>` 行）。
///
/// 只解析条目名，不把整段系统文本当断言面：direct 工具的声明段（A9）合法地包含
/// `mcp__web__*` 等 effective name，把它们算进 deferred 面会得到错误结论。
fn direct_deferred_entries(system: &str) -> Vec<String> {
    let start = match system.find("## Deferred Tools") {
        Some(index) => index,
        None => return Vec::new(),
    };
    system[start..]
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .filter_map(|entry| entry.split(':').next())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

// ── 能力关闭（IF-D10 面① / ARC-CAPABILITY-CLOSURE-001）────────────────────────

/// 关闭矩阵的**首个 LLM 请求**面：`WebMiddleware` / `ArtifactMiddleware` / 两者同时
/// 关闭时，direct 工具集合逐项变化，且用户 MCP 实例不受影响。
///
/// 同时验证 R8 的分层：关闭是 **turn 级注入** 事实 —— 被关闭的实例在 pool 级仍是
/// `Connected`（readiness 不因关闭而不成立），只有本 turn 的注入面归零。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn builtin_instance_closure_changes_only_the_first_model_request_surface() {
    // (disabled_middlewares, Web 两工具可见, artifact 可见)
    let cases: [(&[&str], bool, bool); 4] = [
        (&[], true, true),
        (&["WebMiddleware"], false, true),
        (&["ArtifactMiddleware"], true, false),
        (&["WebMiddleware", "ArtifactMiddleware"], false, false),
    ];
    let web_tools = ["mcp__web__WebSearch", "mcp__web__WebFetch"];

    for (disabled, web_visible, artifact_visible) in cases {
        let harness = WireFixtureHarness::initialized().await;
        harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;
        // pool 级 ready 与 turn 级注入是两件事：被关闭的实例仍必须是健康连接。
        harness.await_connected("web").await;
        harness.await_connected("artifact").await;

        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(WireScriptedModel::new(vec![]));
        let result = run_wire_prompt_with_frozen(
            harness.session_context("mcp-v4-builtin-closure"),
            &sink,
            &model,
            Some(frozen_with_disabled(disabled)),
        )
        .await;

        assert!(
            result.ok,
            "[{disabled:?}] 关闭 builtin 实例不得 fatal（readiness 仍按 pool 级判定）: {:?}",
            result.failure
        );
        assert_eq!(
            model.call_count(),
            1,
            "[{disabled:?}] 首个 prompt 恰好一次模型调用"
        );

        let names = model.first_request_tool_names();
        let system = model.first_request_system_text();
        let deferred = direct_deferred_entries(&system);
        println!("[{disabled:?}] tools = {names:?}");
        println!("[{disabled:?}] deferred = {deferred:?}");

        for tool in web_tools {
            assert_eq!(
                names.iter().any(|name| name == tool),
                web_visible,
                "[{disabled:?}] `{tool}` 在首个 LLM 请求中的 direct 可见性: {names:?}"
            );
        }
        assert_eq!(
            names.iter().any(|name| name == "mcp__artifact__artifact"),
            artifact_visible,
            "[{disabled:?}] `mcp__artifact__artifact` 在首个 LLM 请求中的 direct 可见性: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|name| name == WIRE_FIXTURE_ECHO_EFFECTIVE_NAME),
            "[{disabled:?}] 关闭 builtin 实例不得影响用户 MCP 的 direct 提升: {names:?}"
        );

        // 「键仍存在但不再生效」判失败：被关闭实例的工具必须同时从 direct 与 deferred 面归零。
        let closed_tools: Vec<&str> = [
            (web_visible, web_tools[0]),
            (web_visible, web_tools[1]),
            (artifact_visible, "mcp__artifact__artifact"),
        ]
        .into_iter()
        .filter(|(visible, _)| !visible)
        .map(|(_, tool)| tool)
        .collect();
        for tool in closed_tools {
            assert!(
                !names.iter().any(|name| name == tool),
                "[{disabled:?}] 被关闭的 `{tool}` 不得留在 direct 面: {names:?}"
            );
            assert!(
                !deferred.iter().any(|name| name == tool),
                "[{disabled:?}] 被关闭的 `{tool}` 不得留在 deferred 条目: {deferred:?}"
            );
        }
    }
}

/// PTC 的**真面**是 `PtcMiddleware::before_agent` 写进提示词的 `RPC-callable tool
/// catalog` 段（`peri-middlewares/src/ptc/mod.rs:310-338`）：它与 `collect_tools` 恒返回
/// `[RunPtcCode]`（`:304-308`）无关 —— catalog 由 `state.local_tools()`（session/turn 级
/// 工具视图）生成，故**必须**随 builtin 关闭集（IF-D10）同步收缩。
///
/// 本节是 acceptance §7 第 7 条「Goal / PTC 两个工具面」PTC 一侧的运行时证伪面：四个
/// 关闭 case 与 [`builtin_instance_closure_changes_only_the_first_model_request_surface`]
/// 逐位相同，但观察量换成**首个模型请求系统文本里的 catalog**（不是 `ModelRequest.tools`）。
///
/// 三条口径：
///
/// 1. **存在性守卫**：先证明 catalog 段确实出现在系统文本里、可解析为非空 JSON 数组
///    —— 否则「不含 `mcp__web__*`」是空断言假绿；
/// 2. **差分收缩**：以全开 case 的 catalog 长度为基线，关 web 精确 −2、关 artifact
///    精确 −1、两者同关 −3（用增量而非绝对计数，避免基线工具集漂移造成假红）；
/// 3. **对照锚点**：用户 MCP 的 `mcp__wire_fixture__echo` 在四个 case 里始终在 catalog
///    中 —— 反证「catalog 段不是整段消失」。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn ptc_catalog_section_follows_builtin_instance_closure() {
    // (disabled_middlewares, Web 两工具可见, artifact 可见)
    let cases: [(&[&str], bool, bool); 4] = [
        (&[], true, true),
        (&["WebMiddleware"], false, true),
        (&["ArtifactMiddleware"], true, false),
        (&["WebMiddleware", "ArtifactMiddleware"], false, false),
    ];
    let web_tools = ["mcp__web__WebSearch", "mcp__web__WebFetch"];
    let mut baseline_len: Option<usize> = None;

    for (disabled, web_visible, artifact_visible) in cases {
        let harness = WireFixtureHarness::initialized().await;
        harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;
        harness.await_connected("web").await;
        harness.await_connected("artifact").await;

        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(WireScriptedModel::new(vec![]));
        let result = run_wire_prompt_with_frozen(
            harness.session_context("mcp-v4-builtin-ptc-catalog"),
            &sink,
            &model,
            Some(frozen_with_disabled(disabled)),
        )
        .await;

        assert!(
            result.ok,
            "[{disabled:?}] 关闭 builtin 实例不得 fatal: {:?}",
            result.failure
        );
        assert_eq!(
            model.call_count(),
            1,
            "[{disabled:?}] 首个 prompt 恰好一次模型调用"
        );

        let system = model.first_request_system_text();
        let catalog = ptc_catalog_names(&system).unwrap_or_else(|| {
            panic!(
                "[{disabled:?}] 首个模型请求的系统文本必须含可解析的 `RPC-callable tool \
                 catalog` 段（否则本节全部断言都是空断言）；系统文本前 400 字符: {}",
                system.chars().take(400).collect::<String>()
            )
        });
        println!(
            "[{disabled:?}] ptc catalog ({} 项) = {catalog:?}",
            catalog.len()
        );

        // 守卫 1：catalog 非空 —— PTC 面不得是空贡献。
        assert!(
            !catalog.is_empty(),
            "[{disabled:?}] catalog 必须非空（PTC 的 local_tools 面必须真的被填过）"
        );
        // 守卫 2：对照锚点 —— 用户 MCP 的 direct 提升在任何关闭组合下都在 catalog 内。
        assert!(
            catalog
                .iter()
                .any(|name| name == WIRE_FIXTURE_ECHO_EFFECTIVE_NAME),
            "[{disabled:?}] 用户 MCP 的 `{WIRE_FIXTURE_ECHO_EFFECTIVE_NAME}` 必须始终在 catalog \
             中（否则「web 工具不在 catalog」可能只是整段消失）: {catalog:?}"
        );

        for tool in web_tools {
            assert_eq!(
                catalog.iter().any(|name| name == tool),
                web_visible,
                "[{disabled:?}] `{tool}` 在 PTC catalog 中的可见性: {catalog:?}"
            );
        }
        assert_eq!(
            catalog.iter().any(|name| name == "mcp__artifact__artifact"),
            artifact_visible,
            "[{disabled:?}] `mcp__artifact__artifact` 在 PTC catalog 中的可见性: {catalog:?}"
        );

        // 差分：关闭集在 catalog 上逐条收缩（增量口径，不用绝对计数）。
        let expected_delta =
            if web_visible { 0 } else { web_tools.len() } + if artifact_visible { 0 } else { 1 };
        if web_visible && artifact_visible {
            baseline_len = Some(catalog.len());
        } else {
            let base = baseline_len.expect("全开基线 case 必须先于关闭 case 执行");
            assert_eq!(
                base - catalog.len(),
                expected_delta,
                "[{disabled:?}] catalog 相对全开基线必须精确收缩 {expected_delta} 项 \
                 （base={base}，本 case={}）: {catalog:?}",
                catalog.len()
            );
        }
    }
}

/// PTC 提示词贡献里 `RPC-callable tool catalog` 段（JSON 数组）的工具名集合。
///
/// 返回 `None` 表示该段**缺失或不是合法 JSON 数组** —— 调用方必须把它当失败而不是空集，
/// 否则「某名字不在 catalog 里」会退化成空断言。
///
/// 用 serde_json 的流式入口读第一个 JSON 值：catalog 之后还跟着别的动态贡献段
/// （`## Deferred Tools` 等），流式读取不依赖段间边界的确切文本。
fn ptc_catalog_names(system: &str) -> Option<Vec<String>> {
    const MARKER: &str = "RPC-callable tool catalog: ";
    let after = &system[system.find(MARKER)? + MARKER.len()..];
    let catalog: Vec<serde_json::Value> = serde_json::Deserializer::from_str(after)
        .into_iter::<Vec<serde_json::Value>>()
        .next()?
        .ok()?;
    Some(
        catalog
            .iter()
            .filter_map(|entry| entry["name"].as_str().map(str::to_string))
            .collect(),
    )
}

/// `McpMiddleware=false` 的输入：整个 MCP 工具面（builtin 与用户 server）都不进入
/// 首个 LLM 请求 —— 关闭键必须有可观察效果，不得「键存在但无效」。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn mcp_middleware_closure_removes_every_mcp_tool_from_first_model_request() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result = run_wire_prompt_with_frozen(
        harness.session_context("mcp-v4-builtin-mcp-closure"),
        &sink,
        &model,
        Some(frozen_with_disabled(&["McpMiddleware"])),
    )
    .await;

    assert!(
        result.ok,
        "McpMiddleware 关闭不得 fatal: {:?}",
        result.failure
    );
    let names = model.first_request_tool_names();
    println!("[McpMiddleware=false] tools = {names:?}");
    assert!(
        !names.iter().any(|name| name.starts_with("mcp__")),
        "McpMiddleware 关闭后不得有任何 MCP 工具进入首个请求: {names:?}"
    );
    for (bare, effective) in WEB_ARTIFACT_CAPABILITIES {
        assert!(
            !names.iter().any(|name| name == bare || name == effective),
            "MCP 面关闭后 builtin capability 不得以任何命名回流（提供面已删除，不得回退旧实现）: {names:?}"
        );
    }
}

/// 关闭态的**差分**证据（acceptance §7 第 9 条运行时证伪面 / §4 关闭面矩阵第 3、4 面）：
/// `WebMiddleware=false` 时模型**编造**一个 web 工具调用，链路既不触达审批、也不触达
/// wire，而是以「未知工具」结算；关闭是**调用面的真事实**，不是「事件流里没有该名字」。
///
/// 三条口径（避免把本用例读成它不证明的东西）：
///
/// 1. **差分形态**：正面对照是同文件 `promoted_direct_tool_approval_calls_wire_exactly_once`
///    （同一夹具、同一 broker、同一脚本化模型，工具**可用** ⇒ 审批恰 1 次 + wire 恰 1 条）。
///    本用例在**同一条 turn** 内先调一个可用工具（正控制，证明审批面与 wire 面确实已装配
///    —— 否则「未触达」是空断言），随后再调被关闭的 `mcp__web__WebSearch`。
/// 2. 「事件流中不出现该名 `ToolStart`」**不可能成立**，本用例不这样断言：模型编造的调用
///    经 model bridge 的 `ToolCallDelta` 直接发 `ToolStarted`
///    （`peri-agent/src/agent/model_bridge.rs:316-337`），解析失败后 tool dispatch 还会补发
///    成对的 Started/Ended（`peri-agent/src/agent/stages/tool_dispatch.rs:153-157`）。
///    因此本用例断言的是「不触达审批 / 不触达 wire / 以未知工具结算」。
/// 3. TUI 侧「关闭后无卡片」**不构成独立证据**：TUI 对任意 `ToolStarted` 都建卡（含本条
///    的失败结算），卡片面的证据价值来自第 3 条断言（结算为 error）而非「无事件」。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn closed_web_tool_call_never_reaches_approval_or_wire() {
    /// 被关闭实例（`WebMiddleware=false`）的一等工具名：模型面里已经不存在的名字。
    const CLOSED_NAME: &str = "mcp__web__WebSearch";

    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let broker = Arc::new(RecordingBroker::new(BrokerDecision::Approve));
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![
        // 正控制：与 `promoted_direct_tool_approval_calls_wire_exactly_once` 同一个可用工具。
        ScriptedToolCall::new(
            WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
            serde_json::json!({ "query": "v02-host-open-control" }),
        ),
        // 关闭名：模型编造的调用（批准决策 ⇒ 若它触达审批，计数必然变化）。
        ScriptedToolCall::new(CLOSED_NAME, serde_json::json!({ "query": "closed" })),
    ]));
    let result = run_wire_prompt_with_frozen(
        session_context_with_broker(&harness, "mcp-v4-builtin-closed-diff", Arc::clone(&broker)),
        &sink,
        &model,
        Some(frozen_with_disabled(&["WebMiddleware"])),
    )
    .await;

    assert!(
        result.ok,
        "模型编造未知名只能结算为工具级错误，不得 fatal: {:?}",
        result.failure
    );
    assert_eq!(
        model.call_count(),
        3,
        "两条脚本调用 + 收尾必须都在**同一条 turn** 内跑完（Reason ×3）"
    );

    // ① 正控制：可用工具确实触达审批恰 1 次 ⇒ 审批面已装配（下面的「未触达」才非空）。
    assert_eq!(
        broker.requests(),
        1,
        "可用工具必须恰好触发一次审批（正控制；若关闭名也触达审批，这里会是 2）"
    );
    let seen: Vec<String> = broker.seen().into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        seen,
        vec![WIRE_FIXTURE_ECHO_EFFECTIVE_NAME.to_string()],
        "审批面只应看到可用工具"
    );

    // ② 关闭名：不触达审批（计数仍为 1、且审批项里没有它）。
    assert!(
        !seen.iter().any(|name| name == CLOSED_NAME),
        "被关闭的 `{CLOSED_NAME}` 不得进入审批面: {seen:?}"
    );

    // ③ 结算面：该名恰有一条模型可见结果，且是「未知工具」（非审批拒绝、非 server 错误）。
    let all_ends = tool_end_events(&sink);
    let closed_ends: Vec<(String, String, bool)> = all_ends
        .iter()
        .filter(|(name, _, _)| name == CLOSED_NAME)
        .cloned()
        .collect();
    assert_eq!(
        closed_ends.len(),
        1,
        "编造的关闭名调用必须恰有一条结算（说明该调用真的走到了结算面）: {all_ends:?}"
    );
    assert!(
        closed_ends[0].2,
        "未知工具结算必须是 error 结果: {closed_ends:?}"
    );
    assert!(
        closed_ends[0]
            .1
            .contains(&format!("Tool not found: {CLOSED_NAME}")),
        "结算文案必须是「未知工具」（不是审批拒绝 / 不是 server 侧失败）: {:?}",
        closed_ends[0].1
    );

    // ④ wire 面：关闭名 0 次；available 工具恰一条（同一 turn 内的正控制）。
    let calls = wire_tool_calls(&harness);
    assert!(
        !calls
            .iter()
            .any(|call| call["params"]["name"] == CLOSED_NAME),
        "关闭名不得出现在 wire 上: {calls:?}"
    );
    assert_eq!(
        calls.len(),
        1,
        "本 turn 的 wire 只应有可用工具那一条: {calls:?}"
    );
    assert_eq!(
        calls[0]["params"]["name"], "echo",
        "wire 上是可用工具的原始名（正控制）: {calls:?}"
    );

    // ⑤ 模型面：关闭名不可见（差分的前提），可用工具仍可见。
    let names = model.first_request_tool_names();
    assert!(
        !names.iter().any(|name| name == CLOSED_NAME),
        "`WebMiddleware=false` 时 `{CLOSED_NAME}` 不得进入首个 LLM 请求: {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|name| name == WIRE_FIXTURE_ECHO_EFFECTIVE_NAME),
        "关闭只作用于被关实例，可用工具面必须不受影响: {names:?}"
    );
}

// ── `PERI_MCP_BUILTIN=off`（A2）───────────────────────────────────────────────

/// off 语义：两实例根本不被注入（不是「注册成 Disabled」），其工具既不在首个 LLM
/// 请求、也不在 ToolSearch 摘要；**不是**回退到 middleware 旧实现（提供面已删除）；
/// 用户 MCP 配置与其它 capability 不受影响。
///
/// 追加「判定面」口径（acceptance §7 第 4 条的运行时证伪面：off 只关注入，不得改变
/// 策略判定）：`default_requires_approval` / `is_edit_tool` 都是**纯查表**、不读 env，
/// 其 effective name ↔ 原始名的 parity（IF-D6 / A4）也不得随 env 变化。本用例在 env
/// 生效**之前**取基线、生效期间复算并逐位比较 —— 一旦有人让这两个函数读 env（或让
/// off 参与判定），本用例立即变红。`is_mutation_tool` 是 `peri-middlewares/src/
/// subagent/mod.rs` 的私有 fn、本 crate 不可调用，故不在本文件覆盖；它的同名 parity
/// 由 crate 内 `peri-middlewares/src/subagent/mod_test.rs:507` 覆盖。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn builtin_injection_off_removes_capabilities_without_fallback() {
    // 判定面基线：必须在 `PERI_MCP_BUILTIN=off` 生效**之前**取值。
    let policy_probe = |name: &str| (default_requires_approval(name), is_edit_tool(name));
    let policy_baseline: Vec<(bool, bool)> = WEB_ARTIFACT_CAPABILITIES
        .iter()
        .map(|(_, effective)| policy_probe(effective))
        .collect();

    let _off = BuiltinInjectionOff::set();
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    // off 生效期间复算：这两个函数是纯查表、不读 env；本断言的价值是 —— 一旦有人
    // 让它们读 env，测试变红。
    let policy_off: Vec<(bool, bool)> = WEB_ARTIFACT_CAPABILITIES
        .iter()
        .map(|(_, effective)| policy_probe(effective))
        .collect();
    assert_eq!(
        policy_off, policy_baseline,
        "`PERI_MCP_BUILTIN=off` 只关注入，不得改变策略判定面（判定函数纯查表、不读 env）"
    );

    // 逐名 parity（IF-D6 / A4 判定型）：effective name 的判定必须等于其原始名的判定。
    for (bare, effective) in WEB_ARTIFACT_CAPABILITIES {
        assert_eq!(
            original_tool_name_of_effective(effective),
            Some(bare),
            "`{effective}` 必须命中归一表并给出原始名 `{bare}`（IF-D15 唯一归一入口）"
        );
        assert_eq!(
            default_requires_approval(effective),
            default_requires_approval(bare),
            "off 下 `{effective}` 的审批判定仍须等于原始名 `{bare}` 的判定"
        );
        assert_eq!(
            is_edit_tool(effective),
            is_edit_tool(bare),
            "off 下 `{effective}` 的编辑类判定仍须等于原始名 `{bare}` 的判定"
        );
    }
    // 反证：与冻结名仅差大小写的**未知名**不命中归一表 ⇒ 走既有 `mcp__*` 保守路径。
    assert_eq!(
        original_tool_name_of_effective("mcp__web__fetch"),
        None,
        "未知名不得被归一（不得含 sanitize / 大小写折叠 / `mcp__` 反拆）"
    );
    assert!(
        default_requires_approval("mcp__web__fetch"),
        "未知 `mcp__*` 在 off 下仍保守要求审批（保守语义不得被 env 放宽）"
    );

    assert!(
        harness.pool().get_client("web").is_none()
            && harness.pool().get_client("artifact").is_none(),
        "off 时两个 builtin 实例都不得被注册（`BuiltinInjectionPolicy::none()`）"
    );
    let mut servers: Vec<String> = harness
        .pool()
        .all_server_infos()
        .into_iter()
        .map(|info| info.name)
        .collect();
    servers.sort();
    assert_eq!(
        servers,
        vec![WIRE_FIXTURE_SERVER_NAME.to_string()],
        "off 时 pool 恰有用户夹具声明的 server"
    );

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result =
        run_wire_prompt(harness.session_context("mcp-v4-builtin-off"), &sink, &model).await;

    assert!(result.ok, "off 不得使 prompt 失败: {:?}", result.failure);
    assert_eq!(model.call_count(), 1, "off 后仍正常进入 Reason");

    let names = model.first_request_tool_names();
    let system = model.first_request_system_text();
    let deferred = direct_deferred_entries(&system);
    println!("[PERI_MCP_BUILTIN=off] tools = {names:?}");
    println!("[PERI_MCP_BUILTIN=off] deferred = {deferred:?}");

    for (bare, effective) in WEB_ARTIFACT_CAPABILITIES {
        assert!(
            !names.iter().any(|name| name == effective),
            "off 时 `{effective}` 不得进入首个 LLM 请求: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == bare),
            "off 的退回态**没有** Web/Artifact 能力（A2：middleware 提供面已删除，不存在旧实现回退）: {names:?}"
        );
        assert!(
            !deferred
                .iter()
                .any(|name| name == effective || name == bare),
            "off 时 `{effective}` 也不得出现在 deferred 条目: {deferred:?}"
        );
    }
    assert!(
        names
            .iter()
            .any(|name| name == WIRE_FIXTURE_ECHO_EFFECTIVE_NAME),
        "off 是 builtin 注入开关，不得影响用户 MCP 的 direct 提升: {names:?}"
    );
}

// ── 启动 fatal 的用户可见投影（§8「builtin 运行时」行的 V-02 部分）────────────

/// 启动期 system 依赖不满足的 fatal 投影：在**两个 builtin 实例已连上**的前提下，
/// 用户声明的 system MCP 缺必需工具仍必须 fatal ——
/// 模型 0 次调用、`ExecutionFailureKind::Internal`、ACP `-32000` + `data.kind=internal`、
/// `TurnEnded(Error)` 恰好一次。builtin 注入不得掩盖或改写该路径。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_dependency_failure_is_fatal_even_with_builtin_instances_live() {
    let harness = WireFixtureHarness::initialized_with_servers(&[ExtraServer {
        name: "wire_fixture_missing",
        tools: "alpha",
        required: Some(&["echo"]),
    }])
    .await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;
    // 两个 builtin 实例是健康的：本轮的 fatal 只能来自用户声明的 system 依赖。
    harness.await_connected("web").await;
    harness.await_connected("artifact").await;

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result = run_wire_prompt(
        harness.session_context("mcp-v4-builtin-fatal"),
        &sink,
        &model,
    )
    .await;

    assert!(!result.ok, "system 依赖不满足必须使首个 prompt 失败");
    assert_eq!(model.call_count(), 0, "闸门失败时模型调用次数必须为 0");

    let failure = result.failure.as_ref().expect("fatal 必须产生 failure");
    assert_eq!(failure.kind, ExecutionFailureKind::Internal);
    assert!(
        failure.public_message.contains("McpMiddleware"),
        "失败必须归属于 McpMiddleware: {}",
        failure.public_message
    );
    assert!(
        failure.public_message.contains("未提供必需工具 \"echo\""),
        "失败文案必须保留准入事实（缺必需工具）: {}",
        failure.public_message
    );

    // 真实失败经生产投影函数落到 ACP error 形态。
    let wire = crate::host::prompt::execution_failure_to_acp_error(failure);
    assert_eq!(
        wire.code,
        crate::host::prompt::ACP_TURN_EXECUTION_FAILED_CODE
    );
    assert_eq!(
        wire.data,
        Some(serde_json::json!({ "kind": "internal" })),
        "MCP 准入失败只能投影为 internal 类别"
    );

    let operations = sink.operations();
    let ended: Vec<&String> = operations
        .iter()
        .filter(|operation| operation.contains("\"turn_ended\""))
        .collect();
    assert_eq!(ended.len(), 1, "terminal 事件必须唯一");
    let turn_end: ExecutorEvent = serde_json::from_str(ended[0]).expect("terminal 事件必须可解析");
    assert!(
        matches!(
            turn_end,
            ExecutorEvent::TurnEnded {
                status: TurnStatus::Error,
                ..
            }
        ),
        "准入失败必须是 Error 终态，不是 Interrupted: {turn_end:?}"
    );
    assert_eq!(sink.push_done_count(), 1, "push_done 恰好一次");
}

// ── 契约 6 BLOCKED 缺口复证（A10/R19）：提升 + 审批 + wire ────────────────────

/// BLOCKED 缺口复证（approve）：走**真实配置路径**提升为 direct 的用户 System MCP
/// 工具被模型调用时，审批恰好发生一次，**线路上恰好一条 `tools/call`**，且 wire 上的
/// 工具名是原始名（归一只作用于判定/匹配，不得泄漏到 wire —— IF-D15 / §9 规则 11）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn promoted_direct_tool_approval_calls_wire_exactly_once() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let broker = Arc::new(RecordingBroker::new(BrokerDecision::Approve));
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![ScriptedToolCall::new(
        WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
        serde_json::json!({ "query": "v02-host-approve" }),
    )]));
    let result = run_wire_prompt(
        session_context_with_broker(&harness, "mcp-v4-builtin-approve", Arc::clone(&broker)),
        &sink,
        &model,
    )
    .await;

    assert!(result.ok, "批准路径必须正常收束: {:?}", result.failure);
    assert_eq!(
        broker.requests(),
        1,
        "被提升为 direct 的工具必须恰好触发一次审批"
    );
    assert_eq!(
        broker
            .seen()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        vec![WIRE_FIXTURE_ECHO_EFFECTIVE_NAME.to_string()],
        "审批 broker 收到的必须是模型面 effective name"
    );

    let calls = wire_tool_calls(&harness);
    assert_eq!(
        calls.len(),
        1,
        "批准后 wire 上恰好一条 tools/call: {calls:?}"
    );
    assert_eq!(
        calls[0]["params"]["name"], "echo",
        "wire 上必须是**原始工具名**（effective name 不得泄漏到 wire）: {calls:?}"
    );
    assert_eq!(
        calls[0]["params"]["arguments"],
        serde_json::json!({ "query": "v02-host-approve" }),
        "server 侧必须收到模型给出的入参（参数真的过了 wire）: {calls:?}"
    );

    let tool_ends = tool_end_events(&sink);
    assert_eq!(tool_ends.len(), 1, "工具结果必须唯一: {tool_ends:?}");
    assert_eq!(
        tool_ends[0].0, WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
        "事件载荷仍为 effective name（归一不污染投影真值）"
    );
    assert!(!tool_ends[0].2, "批准后的调用不得是错误结果");
    assert!(
        tool_ends[0].1.contains("wire_fixture:echo:ok"),
        "工具结果内容必须可辨认: {:?}",
        tool_ends[0].1
    );
}

/// BLOCKED 缺口复证（reject）：拒绝后链路**不得**触碰 wire（`tools/call` 计数为 0），
/// 结果为拒绝语义且只审批一次（不重试）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn promoted_direct_tool_rejection_never_reaches_wire() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let broker = Arc::new(RecordingBroker::new(BrokerDecision::Reject));
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![ScriptedToolCall::new(
        WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
        serde_json::json!({ "query": "v02-host-reject" }),
    )]));
    let result = run_wire_prompt(
        session_context_with_broker(&harness, "mcp-v4-builtin-reject", Arc::clone(&broker)),
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "用户拒绝是正常终态（模型可见的 error 结果），不是 fatal: {:?}",
        result.failure
    );
    assert_eq!(broker.requests(), 1, "拒绝路径同样只审批一次，不得重试");

    let calls = wire_tool_calls(&harness);
    assert!(
        calls.is_empty(),
        "拒绝后 wire 上不得出现任何 tools/call: {calls:?}"
    );

    let tool_ends = tool_end_events(&sink);
    assert_eq!(
        tool_ends.len(),
        1,
        "拒绝必须产生一条模型可见的工具结果: {tool_ends:?}"
    );
    assert!(tool_ends[0].2, "拒绝必须是 error 结果: {tool_ends:?}");
    assert!(
        tool_ends[0].1.contains("v02 host seam reject"),
        "结果文案必须表达用户拒绝语义: {:?}",
        tool_ends[0].1
    );
}

/// 夹具对端 wire 日志里的 `tools/call` 请求（真实到达 server 的帧，不是 mock 计数）。
fn wire_tool_calls(harness: &WireFixtureHarness) -> Vec<serde_json::Value> {
    harness
        .wire_received_requests()
        .into_iter()
        .filter(|request| request["method"] == "tools/call")
        .collect()
}

/// `ToolEnd` 事件的三元组（name / output / is_error）。
fn tool_end_events(sink: &MockEventSink) -> Vec<(String, String, bool)> {
    sink.operations()
        .iter()
        .filter_map(|operation| serde_json::from_str::<ExecutorEvent>(operation).ok())
        .filter_map(|event| match event {
            ExecutorEvent::ToolEnd {
                name,
                output,
                is_error,
                ..
            } => Some((name, output, is_error)),
            _ => None,
        })
        .collect()
}
