//! 从 middleware_runner.rs 分离的测试模块
use super::*;
use crate::agent::stages::StageContext;
use crate::messages::{BaseMessage, MessageContent};
use crate::middleware::capabilities as hook_state;
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

#[test]
fn test_agent_context_add_message_dual_writes() {
    let ctx = make_context();
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("old")));

    let mut cx = make_context_from_stage(&ctx);
    cx.add_message(BaseMessage::human(MessageContent::text("new")));

    assert_eq!(cx.messages().len(), 2);
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 2);
}

#[test]
fn test_drain_recall_to_buffer() {
    let ctx = make_context();
    {
        let mut cx = make_context_from_stage(&ctx);
        cx.push_recall("recall-1".to_string());
        cx.push_recall("recall-2".to_string());
        let drained = cx.drain_recall();
        assert_eq!(drained.len(), 2);
        assert!(cx.drain_recall().is_empty());
        // 手动 drain 到 ctx.recall_buffer
        ctx.recall_buffer.write().extend(drained);
    }
    let recalls = ctx.recall_buffer.read();
    assert_eq!(recalls.len(), 2);
    assert_eq!(recalls[0], "recall-1");
    assert_eq!(recalls[1], "recall-2");
}

#[test]
fn test_recall_accumulates_across_hooks() {
    let ctx = make_context();
    {
        let mut cx = make_context_from_stage(&ctx);
        cx.push_recall("hook-1".to_string());
        let rec = cx.drain_recall();
        ctx.recall_buffer.write().extend(rec);
    }
    {
        let mut cx = make_context_from_stage(&ctx);
        cx.push_recall("hook-2".to_string());
        let rec = cx.drain_recall();
        ctx.recall_buffer.write().extend(rec);
    }
    let recalls = ctx.recall_buffer.read();
    assert_eq!(recalls.len(), 2);
    assert_eq!(recalls[0], "hook-1");
    assert_eq!(recalls[1], "hook-2");
}

#[test]
fn test_no_recall_keeps_buffer_empty() {
    let ctx = make_context();
    let mut cx = make_context_from_stage(&ctx);
    let drained = cx.drain_recall();
    assert!(drained.is_empty());
}

struct ReplaceAppendRecall {
    fail: bool,
}

#[async_trait::async_trait]
impl crate::middleware::Middleware for ReplaceAppendRecall {
    fn name(&self) -> &str {
        "ReplaceAppendRecall"
    }

    async fn before_agent(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        let original = state
            .messages()
            .iter()
            .find(|message| message.content() == "original")
            .unwrap()
            .clone();
        assert!(state.replace_message(original.clone_with_content(MessageContent::text("updated"))));
        state.add_message(BaseMessage::human(MessageContent::text("added")));
        state.push_recall("replacement recall".to_string());
        if self.fail {
            return Err(crate::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: "failure after replacement".to_string(),
            });
        }
        Ok(())
    }
}

struct ObserveReplacement(Arc<std::sync::atomic::AtomicBool>);

#[async_trait::async_trait]
impl crate::middleware::Middleware for ObserveReplacement {
    fn name(&self) -> &str {
        "ObserveReplacement"
    }

    async fn before_agent(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        assert_eq!(
            state
                .messages()
                .iter()
                .map(BaseMessage::content)
                .collect::<Vec<_>>(),
            vec!["updated", "added"]
        );
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

async fn assert_before_agent_reconciles_replacement(fail: bool) {
    let mut ctx = make_context();
    let (excluded_id, original_id, original_flags) = {
        let mut transcript = ctx.session.transcript.write();
        let excluded = transcript.append(BaseMessage::human(MessageContent::text("excluded")));
        transcript.set_excluded(excluded, true);
        let original = transcript.append(BaseMessage::human(MessageContent::text("original")));
        transcript.set_truncated(original, true);
        (excluded, original, transcript.flags(original))
    };
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(ReplaceAppendRecall { fail }));
    chain.add(Box::new(ObserveReplacement(Arc::clone(&observed))));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let result = run_before_agent(&ctx, &[]).await;
    assert_eq!(result.is_err(), fail);
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), !fail);
    assert_eq!(*ctx.recall_buffer.read(), vec!["replacement recall"]);
    let transcript = ctx.session.transcript.read();
    assert_eq!(
        transcript.len(),
        3,
        "replacement must not append or rebuild entries"
    );
    assert_eq!(transcript.entries()[0].message().id(), excluded_id);
    assert_eq!(transcript.entries()[1].message().id(), original_id);
    assert_eq!(
        transcript.get(original_id).unwrap().message().content(),
        "updated"
    );
    assert_eq!(transcript.flags(original_id), original_flags);
    assert_eq!(
        transcript
            .visible_messages()
            .iter()
            .map(|message| message.content())
            .collect::<Vec<_>>(),
        vec!["updated", "added"]
    );
    assert_eq!(
        transcript.get(excluded_id).unwrap().message().content(),
        "excluded"
    );
}

#[tokio::test]
async fn before_agent_reconciles_stable_id_replacement() {
    assert_before_agent_reconciles_replacement(false).await;
}

#[tokio::test]
async fn before_agent_reconciles_stable_id_replacement_after_error() {
    assert_before_agent_reconciles_replacement(true).await;
}

struct PrepareInput {
    fail: bool,
}

#[async_trait::async_trait]
impl crate::middleware::Middleware for PrepareInput {
    fn name(&self) -> &str {
        "PrepareInput"
    }

    async fn before_input(
        &self,
        state: &mut dyn hook_state::BeforeInputState,
    ) -> crate::error::AgentResult<()> {
        let id = state.input_message_ids().unwrap()[0];
        let message = state
            .messages()
            .iter()
            .find(|message| message.id() == id)
            .unwrap()
            .clone();
        assert!(state.replace_message(message.clone_with_content(MessageContent::text("prepared"))));
        if self.fail {
            return Err(crate::error::AgentError::MiddlewareError {
                middleware: self.name().to_owned(),
                reason: "input preparation failed".to_owned(),
            });
        }
        Ok(())
    }
}

struct ObservePreparedInput(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl crate::middleware::Middleware for ObservePreparedInput {
    fn name(&self) -> &str {
        "ObservePreparedInput"
    }

    async fn before_agent(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        assert_eq!(
            state.messages()[0].content(),
            "prepared",
            "后续初始化须看见首批转换结果"
        );
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn test_before_input_preserves_initial_order_without_reinitializing_later_batches() {
    let mut ctx = make_context();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(PrepareInput { fail: false }));
    chain.add(Box::new(ObservePreparedInput(Arc::clone(&count))));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let first = ctx
        .session
        .transcript
        .write()
        .append(BaseMessage::human("first"));
    run_before_agent(&ctx, &[first]).await.unwrap();
    let later = ctx
        .session
        .transcript
        .write()
        .append(BaseMessage::human("later"));
    run_before_input(&ctx, &[later]).await.unwrap();
    run_before_input(&ctx, &[]).await.unwrap();
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "初始化只执行一次"
    );
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 2, "准备不增删消息");
    assert_eq!(
        transcript.get(later).unwrap().message().content(),
        "prepared"
    );
}

#[tokio::test]
async fn test_before_input_reconciles_replacement_after_error() {
    let mut ctx = make_context();
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(PrepareInput { fail: true }));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let id = ctx
        .session
        .transcript
        .write()
        .append(BaseMessage::human("original"));
    let error = run_before_input(&ctx, &[id]).await.unwrap_err();
    assert!(
        matches!(error, crate::error::AgentError::MiddlewareError { middleware, reason }
        if middleware == "PrepareInput" && reason == "input preparation failed")
    );
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    assert_eq!(
        transcript.get(id).unwrap().message().content(),
        "prepared",
        "出错前已完成的转换仍须回写"
    );
}

// ─── 启动闸门（before_react_start）──────────────────────────────────────────

/// 静态 MCP bridge 测试桩（名称与 `McpToolBridge` 同形）。
struct StartupStubTool {
    name: String,
    server: Option<String>,
    direct: bool,
}

#[async_trait::async_trait]
impl crate::tools::BaseTool for StartupStubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.name
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn mcp_server_name(&self) -> Option<&str> {
        self.server.as_deref()
    }

    fn is_direct(&self) -> bool {
        self.direct
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

fn startup_stub(name: &str, server: Option<&str>, direct: bool) -> Arc<dyn crate::tools::BaseTool> {
    Arc::new(StartupStubTool {
        name: name.to_string(),
        server: server.map(str::to_owned),
        direct,
    })
}

/// 本次准入的候选：静态 bridge + 必需工具身份。
fn startup_candidate(direct: bool) -> (Arc<dyn crate::tools::BaseTool>, StartupToolUpdate) {
    let tool = startup_stub("mcp__system__lookup", Some("system"), direct);
    let update = StartupToolUpdate {
        tools: vec![Arc::clone(&tool)],
        required: vec![crate::session::tool_catalog::StartupRequiredTool {
            server_name: "system".to_string(),
            original_tool_name: "lookup".to_string(),
            effective_tool_name: "mcp__system__lookup".to_string(),
        }],
    };
    (tool, update)
}

fn startup_catalog_with_core() -> Arc<crate::session::tool_catalog::SessionToolCatalog> {
    Arc::new(crate::session::tool_catalog::SessionToolCatalog::new(
        std::collections::BTreeMap::from([("Read".to_string(), startup_stub("Read", None, true))]),
        None,
    ))
}

fn working_tool_names(ctx: &StageContext) -> Vec<String> {
    ctx.runtime.tools.read().keys().cloned().collect()
}

struct StageStartupTools(StartupToolUpdate);

#[async_trait::async_trait]
impl crate::middleware::Middleware for StageStartupTools {
    fn name(&self) -> &str {
        "StartupStager"
    }

    async fn before_react_start(
        &self,
        state: &mut dyn hook_state::StartupState,
    ) -> crate::error::AgentResult<()> {
        state.stage_startup_tools(self.0.clone())
    }
}

struct ObserveUncommittedCatalog(
    Arc<crate::session::tool_catalog::SessionToolCatalog>,
    Arc<std::sync::atomic::AtomicBool>,
);

#[async_trait::async_trait]
impl crate::middleware::Middleware for ObserveUncommittedCatalog {
    fn name(&self) -> &str {
        "ObserveUncommittedCatalog"
    }

    async fn before_react_start(
        &self,
        _state: &mut dyn hook_state::StartupState,
    ) -> crate::error::AgentResult<()> {
        assert!(
            !self.0.snapshot().tools.contains_key("mcp__system__lookup"),
            "整条闸门链成功之前不得提交目录"
        );
        self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

struct SecondStartupStager(StartupToolUpdate);

#[async_trait::async_trait]
impl crate::middleware::Middleware for SecondStartupStager {
    fn name(&self) -> &str {
        "SecondStartupStager"
    }

    async fn before_react_start(
        &self,
        state: &mut dyn hook_state::StartupState,
    ) -> crate::error::AgentResult<()> {
        state.stage_startup_tools(self.0.clone())
    }
}

struct FailStartupGate;

#[async_trait::async_trait]
impl crate::middleware::Middleware for FailStartupGate {
    fn name(&self) -> &str {
        "FailStartupGate"
    }

    async fn before_react_start(
        &self,
        _state: &mut dyn hook_state::StartupState,
    ) -> crate::error::AgentResult<()> {
        Err(crate::error::AgentError::MiddlewareError {
            middleware: self.name().to_string(),
            reason: "gate failed after staging".to_string(),
        })
    }
}

#[tokio::test]
async fn startup_gate_commits_staged_candidate_to_static_base_only() {
    let mut ctx = make_context();
    let catalog = startup_catalog_with_core();
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let (_, update) = startup_candidate(true);
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(StageStartupTools(update)));
    chain.add(Box::new(ObserveUncommittedCatalog(
        Arc::clone(&catalog),
        Arc::clone(&observed),
    )));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let working_before = working_tool_names(&ctx);

    run_before_react_start(&ctx).await.unwrap();

    assert!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        "闸门必须按链序走完全部 middleware"
    );
    let snapshot = catalog.snapshot();
    assert!(
        snapshot
            .direct_definitions
            .iter()
            .any(|definition| definition.name == "mcp__system__lookup"),
        "准入候选必须直接出现在目录的模型可见工具中"
    );
    assert!(snapshot.tools.contains_key("Read"), "非 MCP 工具不被覆盖");
    assert_eq!(
        working_tool_names(&ctx),
        working_before,
        "startup 提交只更新 static base，working map 留给 Reason boundary 的 refresh/swap"
    );
}

#[tokio::test]
async fn startup_gate_discards_candidate_when_a_later_middleware_fails() {
    let mut ctx = make_context();
    let catalog = startup_catalog_with_core();
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let before = catalog.snapshot();
    let (_, update) = startup_candidate(true);
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(StageStartupTools(update)));
    chain.add(Box::new(FailStartupGate));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let error = run_before_react_start(&ctx).await.unwrap_err();

    assert!(
        matches!(error, crate::error::AgentError::MiddlewareError { middleware, reason }
            if middleware == "FailStartupGate" && reason == "gate failed after staging")
    );
    assert!(
        Arc::ptr_eq(&before, &catalog.snapshot()),
        "闸门链失败时候选必须整体丢弃，不提交部分目录"
    );
}

#[tokio::test]
async fn startup_gate_rejects_second_staging_with_owner_attribution() {
    let mut ctx = make_context();
    let catalog = startup_catalog_with_core();
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let before = catalog.snapshot();
    let (_, first) = startup_candidate(true);
    let (_, second) = startup_candidate(true);
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(StageStartupTools(first)));
    chain.add(Box::new(SecondStartupStager(second)));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let error = run_before_react_start(&ctx).await.unwrap_err();

    assert!(
        matches!(error, crate::error::AgentError::MiddlewareError { middleware, reason }
            if middleware == "SecondStartupStager" && reason.contains("拒绝重复登记")),
        "重复登记必须归属到实际登记的 middleware"
    );
    assert!(Arc::ptr_eq(&before, &catalog.snapshot()));
}

#[tokio::test]
async fn startup_gate_commit_failure_is_attributed_to_staging_middleware() {
    let mut ctx = make_context();
    let catalog = Arc::new(
        crate::session::tool_catalog::SessionToolCatalog::with_filter(
            std::collections::BTreeMap::new(),
            None,
            Arc::new(|name| name != "mcp__system__lookup"),
        ),
    );
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let (_, update) = startup_candidate(true);
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(StageStartupTools(update)));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let error = run_before_react_start(&ctx).await.unwrap_err();

    match error {
        crate::error::AgentError::MiddlewareError { middleware, reason } => {
            assert_eq!(middleware, "StartupStager");
            assert!(
                reason.contains("工具目录发布被拒绝") && reason.contains("mcp__system__lookup"),
                "文案必须给出安全且可定位的失败类别，实际为：{reason}"
            );
        }
        other => panic!("expected MiddlewareError, got {other:?}"),
    }
    assert!(!catalog.snapshot().tools.contains_key("mcp__system__lookup"));
}

#[tokio::test]
async fn startup_gate_without_candidate_leaves_catalog_and_working_map() {
    let mut ctx = make_context();
    let catalog = startup_catalog_with_core();
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let before = catalog.snapshot();
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(crate::middleware::NoopMiddleware::new("noop")));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let working_before = working_tool_names(&ctx);

    run_before_react_start(&ctx).await.unwrap();

    assert!(Arc::ptr_eq(&before, &catalog.snapshot()));
    assert_eq!(working_tool_names(&ctx), working_before);
}

/// 记录首次 Reason 实际入参工具名的 mock LLM（不复制 Reason 实现）。
struct CapturingReasonLlm(Arc<parking_lot::Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for CapturingReasonLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        *self.0.lock() = tools.iter().map(|tool| tool.name().to_string()).collect();
        Ok(crate::agent::react::Reasoning::with_answer(
            "thinking", "done",
        ))
    }

    fn model_name(&self) -> String {
        "capturing-reason".to_string()
    }
}

#[tokio::test]
async fn startup_catalog_update_reaches_first_reason_tool_list() {
    let mut ctx = make_context();
    let catalog = startup_catalog_with_core();
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let required = startup_stub("mcp__system__lookup", Some("system"), true);
    let deferred = startup_stub("mcp__system__extra", Some("system"), false);
    let update = StartupToolUpdate {
        tools: vec![Arc::clone(&required), Arc::clone(&deferred)],
        required: vec![crate::session::tool_catalog::StartupRequiredTool {
            server_name: "system".to_string(),
            original_tool_name: "lookup".to_string(),
            effective_tool_name: "mcp__system__lookup".to_string(),
        }],
    };
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(StageStartupTools(update)));
    ctx.runtime.middleware_chain = Arc::new(chain);
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("question")));
    run_before_react_start(&ctx).await.unwrap();
    assert!(
        !ctx.runtime.tools.read().contains_key("mcp__system__lookup"),
        "闸门提交阶段不先动 working map"
    );

    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    ctx.runtime.llm = Arc::new(CapturingReasonLlm(Arc::clone(&seen)));
    crate::agent::stages::reason::run_reason(crate::agent::stages::ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();

    let names = seen.lock().clone();
    assert!(
        names.contains(&"mcp__system__lookup".to_string()),
        "首个 Reason 的 LLM 入参必须含 required 工具，实际为：{names:?}"
    );
    assert!(
        !names.contains(&"mcp__system__extra".to_string()),
        "普通 deferred 工具不得直接进入 LLM tools"
    );
    assert!(names.contains(&"Read".to_string()), "core 工具不受影响");
    let snapshot = ctx.runtime.tool_catalog.snapshot();
    assert!(
        snapshot.tools.contains_key("mcp__system__extra"),
        "deferred bridge 仍在目录中，留给 ToolSearch 发现"
    );
    assert!(
        ctx.runtime.tools.read().contains_key("mcp__system__lookup"),
        "Reason boundary 完成 refresh → working map swap"
    );
}

struct InterruptStartupGate;

#[async_trait::async_trait]
impl crate::middleware::Middleware for InterruptStartupGate {
    fn name(&self) -> &str {
        "InterruptStartupGate"
    }

    async fn before_react_start(
        &self,
        _state: &mut dyn hook_state::StartupState,
    ) -> crate::error::AgentResult<()> {
        Err(crate::error::AgentError::Interrupted)
    }
}

#[tokio::test]
async fn startup_gate_interrupted_is_propagated_without_commit() {
    let mut ctx = make_context();
    let catalog = startup_catalog_with_core();
    ctx.runtime.tool_catalog = Arc::clone(&catalog);
    let before = catalog.snapshot();
    let (_, update) = startup_candidate(true);
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(StageStartupTools(update)));
    chain.add(Box::new(InterruptStartupGate));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let error = run_before_react_start(&ctx).await.unwrap_err();

    assert!(
        matches!(error, crate::error::AgentError::Interrupted),
        "取消必须按 Interrupted 原样上报（timeout 等 fatal 不在此列），实际为 {error:?}"
    );
    assert!(
        Arc::ptr_eq(&before, &catalog.snapshot()),
        "取消时不得发布 ready"
    );
}
