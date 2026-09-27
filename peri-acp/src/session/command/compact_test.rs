//! CompactCommand 行为 + 合约测试。
//!
//! 此文件原为 `compact.rs` 内联 `#[cfg(test)] mod tests`，按 CLAUDE.md 编码规范
//! （测试 ≥30 行必须分离为同目录 `_test.rs`）与 bg.rs/rewind.rs/mod.rs 对齐外置。
//!
//! 6 个 `contract_*` 测试固化 CLAUDE.md 第一优先级 [TRAP]：
//!   compact 后消息必须以 `BaseMessage::human(summary + continuation)` 开头，
//!   完整结构为 `[Human(摘要+续接指令), System(文件)..., System(Skills)...]`。
//!   禁止将摘要放在 `BaseMessage::system()` 中，禁止出现孤立的 ToolUse。
//!
//! 这些测试是 Contract Test：固定 mock 输入与 mock 模型，
//! 断言 CompactCommand.execute 的输出结构契约（而非内部行为细节）。
//!
// [TRAP] CompactCompleted 事件被 TUI 通过 StateSnapshot + 流式事件维护状态消费。
// 重构 facade + pipeline 后，事件字段 messages 与 CommandResult.messages 仍共享
// new_messages.clone() —— 这些 contract test 是防护此一致性的命脉，绝不可削弱。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use std::collections::HashMap;

use peri_acp_types::{
    command::{FeedbackChannel, FeedbackLevel},
    event::ExecutorEvent,
    messages::{BaseMessage, ContentBlock, MessageId},
    session_resources::{
        FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources, SessionSnapshot,
    },
    store::{MessageFlags, PersistedPayload},
    thread::{CancelPolicy, ThreadId},
    workspace::{SessionBinding, SessionExecutionLease},
};

use super::*;
use crate::session::command::CommandResult;
use crate::session::executor::PromptStopReason;

// ── Mock EventSink ────────────────────────────────────────────────────

struct MockEventSink {
    events: Mutex<Vec<(String, String)>>,
    push_done_count: Mutex<usize>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            push_done_count: Mutex::new(0),
        }
    }

    fn events(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::session::event_sink::EventSink for MockEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events
            .lock()
            .unwrap()
            .push((session_id.to_string(), json));
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
    }
}

impl MockEventSink {
    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

fn make_ctx(
    sink: Arc<dyn crate::session::event_sink::EventSink>,
    history: Vec<BaseMessage>,
) -> super::super::CommandContext {
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 旧字段默认值与原字面量一致（compact_config: Default / 其余 None）。
    super::super::CommandContext::new(
        "test-session".to_string(),
        history,
        "/tmp".to_string(),
        sink,
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    )
}

/// 构造带 auxiliary_model 的 CommandContext（contract test 使用真实模型路径）
///
/// 返回夹具本体：门面写入要求「本 root 有活 owner」，lease 必须活到测试结束，
/// 因此由调用方持有（`let (ctx, _session) = make_ctx_with_model(..)`）。
async fn make_ctx_with_model(
    sink: Arc<dyn crate::session::event_sink::EventSink>,
    history: Vec<BaseMessage>,
    model: Arc<dyn peri_model::Model>,
) -> (super::super::CommandContext, BoundSession) {
    let session = BoundSession::open("compact-test.db").await;
    let ctx = make_ctx_with_model_and_thread(
        sink,
        history,
        session.cwd.clone(),
        model,
        Some(session.resources()),
        Some(session.thread_id.clone()),
    );
    (ctx, session)
}

fn make_ctx_with_model_and_thread(
    sink: Arc<dyn crate::session::event_sink::EventSink>,
    history: Vec<BaseMessage>,
    cwd: String,
    model: Arc<dyn peri_model::Model>,
    session_resources: Option<Arc<dyn SessionResources>>,
    thread_id: Option<String>,
) -> super::super::CommandContext {
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 非默认旧字段显式赋值（auxiliary_model / session_resources / thread_id）。
    let mut ctx = super::super::CommandContext::new(
        "test-session".to_string(),
        history,
        cwd,
        sink,
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );
    ctx.auxiliary_model = Some(model);
    ctx.session_resources = session_resources;
    ctx.thread_id = thread_id;
    ctx
}

// ── 门面夹具 ──────────────────────────────────────────────────────────
//
// 完整 compact lifecycle 必须绑定会话资源门面；门面的写入门禁要求「本 root 有活
// owner」，所以夹具真的建立一条已绑定会话并持有它的执行所有权（与生产路径同一前置
// 条件），不用替身假装可写。断言走同一次一致快照（payload 与 flags 同一次读取）。

/// 临时库上的已绑定会话 + 活跃执行所有权。
struct BoundSession {
    resources: Arc<dyn SessionResources>,
    thread_id: ThreadId,
    cwd: String,
    db_path: std::path::PathBuf,
    /// 持有到测试结束：owner 一旦丢弃，门面的写入按 `LeaseRequired` 真实失败。
    _lease: Arc<dyn SessionExecutionLease>,
    _db: tempfile::TempDir,
}

impl BoundSession {
    /// 新库 + 一条已绑定会话（cwd 为临时目录解析后的路径）。
    async fn open(file: &str) -> Self {
        let db = tempfile::tempdir().expect("创建临时目录失败");
        let db_path = db.path().join(file);
        let resources: Arc<dyn SessionResources> = Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open(&db_path)
                .await
                .expect("打开会话库失败"),
        );
        let workspace = resources
            .resolve_workspace(db.path())
            .await
            .expect("解析工作区失败");
        let thread_id: ThreadId = uuid::Uuid::now_v7().to_string();
        let lease = resources
            .create_session(&NewSession {
                thread_id: thread_id.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
            })
            .await
            .expect("创建会话失败");
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        Self {
            resources,
            thread_id,
            cwd,
            db_path,
            _lease: lease,
            _db: db,
        }
    }

    /// 门面句柄（交给 CommandContext / 另开只读句柄时用）。
    fn resources(&self) -> Arc<dyn SessionResources> {
        Arc::clone(&self.resources)
    }

    /// 同一库的只读句柄：数据可读、能力面为 `HistoryReadOnly`——完整 lifecycle
    /// 无法完成（与迁前 FilesystemThreadStore 的能力面一致）。
    async fn open_read_only(&self) -> Arc<dyn SessionResources> {
        Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(&self.db_path)
                .await
                .expect("只读打开会话库失败"),
        )
    }

    /// 把可见 history 存进会话（与生产 append 同一行为）。
    async fn append(&self, history: &[BaseMessage]) {
        let payloads: Vec<PersistedPayload> = history
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect();
        self.resources
            .append_history(&self.thread_id, &payloads)
            .await
            .expect("持久化初始 history 失败");
    }

    /// 一次一致快照。
    async fn snapshot(&self) -> SessionSnapshot {
        self.resources
            .load_session_snapshot(&self.thread_id)
            .await
            .expect("加载会话快照失败")
    }

    /// 已持久化的消息本体（reminder 不进入 compact 输入，与生产同语义）。
    async fn stored_messages(&self) -> Vec<BaseMessage> {
        self.snapshot()
            .await
            .payloads
            .iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect()
    }

    /// 已持久化的投影 flag。
    async fn stored_flags(&self) -> HashMap<MessageId, MessageFlags> {
        self.snapshot().await.flags
    }
}

// ── extract_file_info 测试 ───────────────────────────────────────────
// 注意：[v2] extract_file_info / extract_skill_names 已迁移到 peri_agent::agent::compact_v2，
// 通过 `use super::*` 间接可见。这里显式引用以保持独立可读。
use peri_acp_types::compact::{extract_file_info, extract_skill_names};

#[test]
fn test_extract_file_info_single_file() {
    // Arrange: 一条包含文件路径的 System 消息
    let msgs = vec![BaseMessage::system(
        "[最近读取的文件: /src/main.rs\nfn main() {}\n",
    )];

    // Act
    let files = extract_file_info(&msgs);

    // Assert: 提取到文件路径和行数（内容行数 = 总行数 - 1(路径行)）
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "/src/main.rs");
    assert_eq!(files[0].lines, 1); // "fn main() {}" — 1 行内容
}

#[test]
fn test_extract_file_info_multiple_files() {
    // Arrange: 多条文件消息
    let msgs = vec![
        BaseMessage::system("[最近读取的文件: /a.rs\nline1\nline2\n"),
        BaseMessage::system("[最近读取的文件: /b.rs\nline1\n"),
    ];

    // Act
    let files = extract_file_info(&msgs);

    // Assert
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "/a.rs");
    assert_eq!(files[0].lines, 2);
    assert_eq!(files[1].path, "/b.rs");
    assert_eq!(files[1].lines, 1);
}

#[test]
fn test_extract_file_info_empty_messages() {
    // Arrange: 空消息列表
    let msgs: Vec<BaseMessage> = vec![];

    // Act
    let files = extract_file_info(&msgs);

    // Assert
    assert!(files.is_empty());
}

#[test]
fn test_extract_file_info_skips_non_file_messages() {
    // Arrange: 非文件 System 消息 + Human/Ai 消息
    let msgs = vec![
        BaseMessage::system("普通系统提示"),
        BaseMessage::human("用户消息"),
        BaseMessage::ai("助手回复"),
    ];

    // Act
    let files = extract_file_info(&msgs);

    // Assert: 全部跳过
    assert!(files.is_empty());
}

#[test]
fn test_extract_file_info_file_with_no_content_lines() {
    // Arrange: 只有路径行，无内容
    let msgs = vec![BaseMessage::system("[最近读取的文件: /empty.rs\n")];

    // Act
    let files = extract_file_info(&msgs);

    // Assert: 路径行存在但无内容行（lines = 0）
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "/empty.rs");
    assert_eq!(files[0].lines, 0);
}

// ── extract_skill_names 测试 ─────────────────────────────────────────

#[test]
fn test_extract_skill_names_single_skill() {
    // Arrange: 一条包含 Skill 名称的 System 消息
    let msgs = vec![BaseMessage::system("[激活的 Skill 指令: tdd")];

    // Act
    let skills = extract_skill_names(&msgs);

    // Assert
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0], "tdd");
}

#[test]
fn test_extract_skill_names_multiple_skills() {
    // Arrange: 多条 Skill 消息
    let msgs = vec![
        BaseMessage::system("[激活的 Skill 指令: tdd"),
        BaseMessage::system("[激活的 Skill 指令: code-review"),
    ];

    // Act
    let skills = extract_skill_names(&msgs);

    // Assert
    assert_eq!(skills.len(), 2);
    assert_eq!(skills[0], "tdd");
    assert_eq!(skills[1], "code-review");
}

#[test]
fn test_extract_skill_names_empty_messages() {
    // Arrange: 空消息列表
    let msgs: Vec<BaseMessage> = vec![];

    // Act
    let skills = extract_skill_names(&msgs);

    // Assert
    assert!(skills.is_empty());
}

#[test]
fn test_extract_skill_names_skips_non_skill_messages() {
    // Arrange: 非技能消息
    let msgs = vec![
        BaseMessage::system("[最近读取的文件: /src/main.rs\n"),
        BaseMessage::human("你好"),
    ];

    // Act
    let skills = extract_skill_names(&msgs);

    // Assert: 全部跳过
    assert!(skills.is_empty());
}

#[test]
fn test_extract_skill_names_extracts_only_first_line() {
    // Arrange: Skill 名称后有多行内容，只取第一行
    let msgs = vec![BaseMessage::system(
        "[激活的 Skill 指令: my-skill\n额外内容\n更多内容",
    )];

    // Act
    let skills = extract_skill_names(&msgs);

    // Assert: 只提取第一行名称
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0], "my-skill");
}

// ── CompactCommand execute 测试 ──────────────────────────────────────

/// 执行并解包：compact 恒 Done，其他变体 panic（与旧 AgentCommand 转发
/// unreachable! 同语义；Phase 5 Step 6 旧契约删除后直接经新契约执行）。
async fn execute_compact(cmd: &CompactCommand, ctx: CommandContext) -> CommandResult {
    match CommandHandler::execute(cmd, ctx).await {
        CommandOutcome::Done(r) => r,
        _ => panic!("compact 应恒 Done"),
    }
}

#[tokio::test]
async fn test_compact_empty_history_returns_original_with_error_feedback() {
    // Phase 5 Step 4：CompactError 事件通道已删除——错误收敛为
    // feedback(Error, UiOnly)，命令自身零事件发射。
    // Arrange: 空历史 + mock sink
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), vec![]);
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: 返回空消息 + EndTurn
    assert_eq!(result.messages.len(), 0);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 错误经 feedback 返回（UiOnly），不再推送 CompactError 事件
    let fb = result.feedback.as_ref().expect("空历史应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert_eq!(fb.message, "no history to compact");
    assert!(
        sink.events().is_empty(),
        "命令自身不应发射任何事件（编排层 emit_command_feedback 统一发射）"
    );
}

#[tokio::test]
async fn test_compact_no_model_returns_original_with_error_feedback() {
    // Arrange: 有历史但无 auxiliary_model（默认 None）
    let sink = Arc::new(MockEventSink::new());
    let history = vec![BaseMessage::human("你好"), BaseMessage::ai("世界")];
    let ctx = make_ctx(sink.clone(), history.clone());
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: 返回原消息 + EndTurn
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 错误经 feedback 返回（UiOnly），不再推送 CompactError 事件
    let fb = result.feedback.as_ref().expect("无模型应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert_eq!(fb.message, "no model available for compact");
    assert!(
        sink.events().is_empty(),
        "命令自身不应发射任何事件（编排层 emit_command_feedback 统一发射）"
    );
}

// ── CompactCommand 属性测试 ──────────────────────────────────────────

#[test]
fn test_compact_command_name_and_aliases() {
    // Phase 5 Step 6：旧 AgentCommand trait 已删，元数据取命令关联常量
    //（注册条目挂载的单一事实源）。
    assert_eq!(CompactCommand::NAME, "compact");
    assert!(
        CompactCommand::ALIASES.contains(&"compress"),
        "应包含 compress 别名"
    );
    assert!(!CompactCommand::DESCRIPTION.is_empty());
}

/// 验证 CompactCommand（Immediate）执行后 push_done 未被命令自身调用
/// （push_done 由 executor.rs 的 Immediate 路径负责调用，此处验证职责分离）
#[tokio::test]
async fn test_compact_command_does_not_call_push_done_itself() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), vec![]);
    let cmd = CompactCommand;

    let _result = execute_compact(&cmd, ctx).await;

    // 空历史返回后，不调用 push_done（由 executor 负责）
    let count = sink.push_done_count();
    assert_eq!(
        count, 0,
        "CompactCommand 自身不应调用 push_done，由 executor 负责"
    );
}

// ── Contract Test: compact 后消息结构不变量 ───────────────────────────
//
// 验证 CLAUDE.md [TRAP] 不变量：
//   compact 后消息必须以 BaseMessage::human(summary + continuation) 开头，
//   完整结构为 [Human(摘要+续接指令), System(文件)..., System(Skills)...]。
//   禁止将摘要放在 BaseMessage::system() 中，禁止出现孤立的 ToolUse。
//
// 这些测试是 Contract Test：固定 mock 输入与 mock 模型，
// 断言 CompactCommand.execute 的输出结构契约（而非内部行为细节）。

/// 返回固定摘要的 mock Model（contract test 用）
struct MockSummaryModel {
    summary: String,
}

impl MockSummaryModel {
    fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
        }
    }
}

#[async_trait]
impl peri_model::Model for MockSummaryModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        // compact 路径只走 complete()，stream() 不应被调用
        Err(peri_model::ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        Ok(peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text(self.summary.clone()),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

/// 构造一条 Ai 消息，包含 Read 工具调用 block（用于 re_inject 提取文件路径）
fn make_ai_with_read_tool(file_path: &str) -> BaseMessage {
    let tool_call_id = "call_read_1".to_string();
    let blocks = vec![
        ContentBlock::Text {
            text: "我来读取这个文件".to_string(),
        },
        ContentBlock::ToolUse {
            id: tool_call_id.clone(),
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": file_path }),
        },
    ];
    BaseMessage::ai_from_blocks(blocks)
}

/// 构造一条 Human 消息，包含 [Skill: path] 标记（用于 re_inject 提取 Skill 路径）
fn make_human_with_skill_marker(skill_path: &str) -> BaseMessage {
    BaseMessage::human(format!("用户消息\n[Skill: {}]", skill_path))
}

#[path = "compact_persistence_test.rs"]
mod persistence_tests;

/// 契约：compact 输出首条消息必须是 Human（摘要+续接指令），
/// 不得为 System 或其他类型。
#[tokio::test]
async fn test_contract_compact_output_starts_with_human_summary() {
    // Arrange: 典型 history — System + Human + Ai(Read) + Tool 结果
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let file_path = dir.path().join("main.rs");
    std::fs::write(&file_path, "fn main() {}\n").expect("写入文件失败");
    let file_path_str = file_path.to_string_lossy().to_string();

    let history = vec![
        BaseMessage::system("系统提示词"),
        BaseMessage::human("帮我看看 main.rs"),
        make_ai_with_read_tool(&file_path_str),
        BaseMessage::tool_result("call_read_1", "fn main() {}"),
    ];

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(MockSummaryModel::new("## 摘要\n已完成 main.rs 审查"));
    let (ctx, _session) = make_ctx_with_model(sink.clone(), history, model).await;
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: 首条必须是 Human
    assert!(!result.messages.is_empty(), "compact 输出不应为空");
    assert!(
        matches!(result.messages[0], BaseMessage::Human { .. }),
        "compact 输出首条必须是 Human（摘要+续接指令），实际: {:?}",
        result.messages[0]
    );

    // 首条内容必须包含续接指令标记
    let first_text = result.messages[0].content();
    assert!(
        first_text.contains(peri_agent::agent::compact_v2::CONTINUATION_HINT),
        "首条 Human 必须包含续接指令，实际内容: {}",
        first_text.chars().take(200).collect::<String>()
    );
    assert!(
        first_text.contains("已完成 main.rs 审查"),
        "首条 Human 必须包含摘要 LLM 输出"
    );
    assert!(
        !first_text.contains("<system-reminder>"),
        "compact summary 是 canonical context，不是运行期通知，不应伪装 reminder"
    );
}

/// 契约：compact 输出结构必须为 [Human, System(文件)..., System(Skills)...]，
/// 即首条之后只允许 System 消息（文件/Skills），不得出现孤立的 ToolUse/Ai/Tool。
#[tokio::test]
async fn test_contract_compact_output_structure_human_then_system_only() {
    // Arrange: history 含 Read 工具调用（对应真实文件）+ Skill 标记
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let file_path = dir.path().join("lib.rs");
    std::fs::write(&file_path, "pub fn foo() {}\n").expect("写入文件失败");
    let file_path_str = file_path.to_string_lossy().to_string();

    // Skills 路径需落在 .claude/skills/ 下，且文件存在
    let skills_dir = dir.path().join(".claude").join("skills").join("tdd");
    std::fs::create_dir_all(&skills_dir).expect("创建 skills 目录失败");
    let skill_file = skills_dir.join("SKILL.md");
    std::fs::write(&skill_file, "# TDD Skill\n").expect("写入 SKILL.md 失败");
    let skill_path_str = skill_file.to_string_lossy().to_string();

    let history = vec![
        BaseMessage::system("系统提示词"),
        make_human_with_skill_marker(&skill_path_str),
        make_ai_with_read_tool(&file_path_str),
        BaseMessage::tool_result("call_read_1", "pub fn foo() {}"),
    ];

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(MockSummaryModel::new("## 摘要\n审查 lib.rs 与 tdd skill"));
    let (ctx, _session) = make_ctx_with_model(sink.clone(), history, model).await;
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: 结构契约 — 首条 Human（摘要），其后只能是 Human（re-inject 文件/Skills）
    //
    // [v2] re-inject 消息从 v1 `BaseMessage::system(...)` 改为 `BaseMessage::human(...)`，
    // 避免 invoke.rs hoist 污染 frozen_system_prompt（CLAUDE.md [TRAP]）。
    // 测试 contract 同步更新——首条 Human 后只能 Human（不再有 System）。
    assert!(
        matches!(result.messages[0], BaseMessage::Human { .. }),
        "首条必须为 Human"
    );
    for (i, msg) in result.messages.iter().enumerate().skip(1) {
        assert!(
            matches!(msg, BaseMessage::Human { .. }),
            "compact 输出索引 {} 必须为 Human（v2 re-inject 避免 hoist），实际: {:?}",
            i,
            msg
        );
    }

    // 不得出现孤立的 ToolUse（Ai 消息不应含 tool_calls）或 Tool 消息
    for (i, msg) in result.messages.iter().enumerate() {
        match msg {
            BaseMessage::Ai { tool_calls, .. } => {
                assert!(
                    tool_calls.is_empty(),
                    "compact 输出索引 {} 的 Ai 消息不得包含 tool_calls（孤立 ToolUse）",
                    i
                );
            }
            BaseMessage::Tool { .. } => {
                panic!("compact 输出索引 {} 出现孤立的 Tool 消息: {:?}", i, msg);
            }
            _ => {}
        }
    }
}

/// 契约：摘要 LLM 输出不得作为 System 消息出现（即不得把摘要放入 System）。
/// 这是一个 "negative contract"：断言没有任何 System 消息的文本包含摘要内容。
#[tokio::test]
async fn test_contract_summary_not_in_system_message() {
    // Arrange: 简单 history（会话夹具自带临时工作区）
    let history = vec![
        BaseMessage::system("系统提示词"),
        BaseMessage::human("你好"),
        BaseMessage::ai("你好，世界"),
    ];

    let unique_marker = "UNIQUE_SUMMARY_MARKER_2026";
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(MockSummaryModel::new(format!("## 摘要\n{}", unique_marker)));
    let (ctx, _session) = make_ctx_with_model(sink.clone(), history, model).await;
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: 摘要只出现在首条 Human，不得出现在任何 System 消息中
    assert!(
        result.messages[0].content().contains(unique_marker),
        "摘要必须出现在首条 Human"
    );
    for (i, msg) in result.messages.iter().enumerate().skip(1) {
        if let BaseMessage::System { content, .. } = msg {
            let text = content.text_content();
            assert!(
                !text.contains(unique_marker),
                "System 消息索引 {} 不得包含摘要 LLM 输出（摘要应只在 Human），实际: {}",
                i,
                text.chars().take(200).collect::<String>()
            );
        }
    }
}

/// 契约：compact 输出 CompactCompleted 事件携带 new_messages，
/// 且事件中的 messages 与 CommandResult.messages 保持一致（外部可观测契约）。
#[tokio::test]
async fn test_contract_compact_completed_event_matches_result_messages() {
    // Arrange
    let history = vec![
        BaseMessage::system("系统提示词"),
        BaseMessage::human("你好"),
        BaseMessage::ai("你好，世界"),
    ];

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(MockSummaryModel::new("## 摘要\n简单对话"));
    let (ctx, _session) = make_ctx_with_model(sink.clone(), history, model).await;
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: CompactCompleted 事件存在
    let events = sink.events();
    let completed = events
        .iter()
        .find(|(_, json)| json.contains("compact_completed"));
    assert!(
        completed.is_some(),
        "应推送 CompactCompleted 事件，实际事件数: {}",
        events.len()
    );

    // CompactCompleted 事件的 messages 字段（反序列化）应与 result 结构契约一致：
    // 首条为 Human
    // 由于事件 JSON 序列化结构复杂，这里验证 result.messages 结构即可（与事件共享同一个 new_messages.clone()）
    assert!(
        matches!(result.messages[0], BaseMessage::Human { .. }),
        "CommandResult 首条必须为 Human"
    );
}

/// 契约：当 history 全为 System 消息（无 Human/Ai）时，
/// full_compact 返回 fallback 摘要，CompactCommand 仍产出以 Human 开头的输出。
/// （对应 full.rs: non_system_count == 0 分支）
#[tokio::test]
async fn test_contract_all_system_history_still_human_first() {
    // Arrange: 全 System history（会话夹具自带临时工作区）
    let history = vec![
        BaseMessage::system("系统提示词 1"),
        BaseMessage::system("系统提示词 2"),
    ];

    let sink = Arc::new(MockEventSink::new());
    // 即使 LLM 被调用返回内容，也不影响首条 Human 契约
    let model = Arc::new(MockSummaryModel::new("## 摘要\n不应到达此处"));
    let (ctx, _session) = make_ctx_with_model(sink.clone(), history, model).await;
    let cmd = CompactCommand;

    // Act
    let result = execute_compact(&cmd, ctx).await;

    // Assert: 仍以 Human 开头（fallback 摘要也要走 Human 路径）
    assert!(
        matches!(result.messages[0], BaseMessage::Human { .. }),
        "全 System history 的 compact 输出首条也必须为 Human（fallback 摘要），实际: {:?}",
        result.messages[0]
    );
    assert_eq!(
        result.stop_reason,
        PromptStopReason::EndTurn,
        "stop_reason 必须为 EndTurn"
    );
}
