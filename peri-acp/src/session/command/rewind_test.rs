//! RewindCommand 单元测试。
//!
//! 覆盖：
//! - revert_files 的 Write 分支（删除文件）
//! - revert_files 的 Edit 分支（ASCII / CJK UTF-8 边界 / new_string 不匹配）
//! - validate_tool_pairing（未配对的 ToolUse / ToolResult，仅告警不 panic）
//! - execute 的三种场景：未找到目标 / 末尾截断 / 中间截断
//!
//! 注：CJK UTF-8 边界场景回归保护 p1-w5a（commit 6d76824d）— revert_files Edit 分支
//! 使用 `content.replacen` 而非字节切片 `&content[..idx]`，避免多字节字符 panic。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    event::ExecutorEvent,
    messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest},
};

use super::super::{
    CommandContext, CommandHandler, CommandOutcome, CommandResult, FeedbackChannel, FeedbackLevel,
};
use super::{extract_file_changes, validate_tool_pairing, FileChange, RewindCommand};
use crate::session::executor::PromptStopReason;

// ── Mock EventSink ────────────────────────────────────────────────────────

/// Mock EventSink，记录所有推送的事件。
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

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
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

// ── Test Data Builder ────────────────────────────────────────────────────

/// 构造 Write 工具调用的 AI 消息（OpenAI tool_calls 格式）。
fn make_ai_write_call(path: &str, content: &str) -> BaseMessage {
    let args = serde_json::json!({
        "file_path": path,
        "content": content,
    });
    BaseMessage::ai_with_tool_calls(
        "推理中...",
        vec![ToolCallRequest::new("call_write_1", "Write", args)],
    )
}

/// 构造 Edit 工具调用的 AI 消息（OpenAI tool_calls 格式）。
fn make_ai_edit_call(path: &str, old_string: &str, new_string: &str) -> BaseMessage {
    let args = serde_json::json!({
        "file_path": path,
        "old_string": old_string,
        "new_string": new_string,
    });
    BaseMessage::ai_with_tool_calls(
        "推理中...",
        vec![ToolCallRequest::new("call_edit_1", "Edit", args)],
    )
}

/// 构造 Write 工具调用的 AI 消息（Anthropic ContentBlock 格式）。
fn make_ai_write_block(path: &str, content: &str) -> BaseMessage {
    let input = serde_json::json!({
        "file_path": path,
        "content": content,
    });
    BaseMessage::ai_from_blocks(vec![ContentBlock::tool_use(
        "toolu_write_1",
        "Write",
        input,
    )])
}

/// 构造 Edit 工具调用的 AI 消息（Anthropic ContentBlock 格式）。
fn make_ai_edit_block(path: &str, old_string: &str, new_string: &str) -> BaseMessage {
    let input = serde_json::json!({
        "file_path": path,
        "old_string": old_string,
        "new_string": new_string,
    });
    BaseMessage::ai_from_blocks(vec![ContentBlock::tool_use("toolu_edit_1", "Edit", input)])
}

/// 构造最小 CommandContext，允许覆盖 history / cwd / args。
fn make_ctx(
    sink: Arc<dyn crate::session::event_sink::EventSink>,
    history: Vec<BaseMessage>,
    cwd: String,
    args: String,
) -> CommandContext {
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 非默认旧字段显式赋值（args）。
    let mut ctx = CommandContext::new(
        "test-session".to_string(),
        history,
        cwd,
        sink,
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );
    ctx.args = args.clone();
    // P1-1：参数统一解析——测试直调 handler 路径无拦截层，按声明 schema
    // 解析填充 parsed_args（解析失败 → None，等价拦截层拒绝进入 handler；
    // 空参数回落 handler 内 missing target_message_id 防御分支）。
    ctx.parsed_args = RewindCommand::args_schema().parse(&args).ok();
    ctx
}

// ── RewindCommand 属性测试 ────────────────────────────────────────────────

#[test]
fn test_rewind_command_name_and_aliases() {
    // Phase 5 Step 6：旧 AgentCommand trait 已删，元数据取命令关联常量
    //（注册条目挂载的单一事实源）。
    assert_eq!(RewindCommand::NAME, "rewind");
    assert!(RewindCommand::ALIASES.contains(&"undo"), "应包含 undo 别名");
    assert!(!RewindCommand::DESCRIPTION.is_empty());
}

/// 执行并解包：rewind 恒 Done，其他变体 panic（与旧 AgentCommand 转发
/// unreachable! 同语义；Phase 5 Step 6 旧契约删除后直接经新契约执行）。
async fn execute_rewind_cmd(cmd: &RewindCommand, ctx: CommandContext) -> CommandResult {
    match CommandHandler::execute(cmd, ctx).await {
        CommandOutcome::Done(r) => r,
        _ => panic!("rewind 应恒 Done"),
    }
}

// ── extract_file_changes 测试 ─────────────────────────────────────────────

#[test]
fn test_extract_file_changes_openai_write_format() {
    // Arrange: OpenAI 格式的 Write 工具调用
    let msgs = vec![make_ai_write_call("a.txt", "hello")];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 提取出 1 个 Write 变更
    assert_eq!(changes.len(), 1, "应提取出 1 个 Write 变更");
}

#[test]
fn test_extract_file_changes_openai_edit_format() {
    // Arrange: OpenAI 格式的 Edit 工具调用
    let msgs = vec![make_ai_edit_call("a.txt", "old", "new")];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 提取出 1 个 Edit 变更
    assert_eq!(changes.len(), 1, "应提取出 1 个 Edit 变更");
}

#[test]
fn test_extract_file_changes_anthropic_write_format() {
    // Arrange: Anthropic 格式的 Write 工具调用（通过 ai_from_blocks 构造）
    // 注意：ai_from_blocks 会把 ToolUse 同步到 tool_calls 字段（见 message.rs），
    // 因此 extract_file_changes 同时遍历 tool_calls 和 content_blocks 时会
    // 遇到同一变更两次。P1 修复后按 ToolUse id 去重，只计一次。
    let msgs = vec![make_ai_write_block("a.txt", "hello")];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 修复后同一变更只计一次（按 ToolUse id 去重）
    assert_eq!(
        changes.len(),
        1,
        "ai_from_blocks 构造的消息在 tool_calls + content_blocks 双路径应去重"
    );
}

#[test]
fn test_extract_file_changes_anthropic_edit_format() {
    // Arrange: Anthropic 格式的 Edit 工具调用（通过 ai_from_blocks 构造）
    // 同上：双路径计数，P1 修复后去重为 1。
    let msgs = vec![make_ai_edit_block("a.txt", "old", "new")];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 修复后同一变更只计一次（按 ToolUse id 去重）
    assert_eq!(
        changes.len(),
        1,
        "ai_from_blocks 构造的消息在 tool_calls + content_blocks 双路径应去重"
    );
}

#[test]
fn test_extract_file_changes_ignores_non_ai_messages() {
    // Arrange: Human / System / Tool 消息不携带 Write/Edit
    let msgs = vec![
        BaseMessage::human("请修改文件"),
        BaseMessage::system("系统提示"),
        BaseMessage::tool_result("call_x", "结果"),
    ];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 不应提取任何变更
    assert!(changes.is_empty(), "非 AI 消息不应被提取");
}

#[test]
fn test_extract_file_changes_multiple_calls_in_one_message() {
    // Arrange: 同一条 AI 消息包含 Write + Edit 两个工具调用
    let write_args = serde_json::json!({"file_path": "a.txt", "content": "hello"});
    let edit_args = serde_json::json!({"file_path": "b.txt", "old_string": "x", "new_string": "y"});
    let msg = BaseMessage::ai_with_tool_calls(
        "推理中...",
        vec![
            ToolCallRequest::new("c1", "Write", write_args),
            ToolCallRequest::new("c2", "Edit", edit_args),
        ],
    );

    // Act
    let changes = extract_file_changes(&[msg]);

    // Assert: 提取出 2 个变更
    assert_eq!(changes.len(), 2, "同一条消息的两个工具调用都应被提取");
}

#[test]
fn test_extract_file_changes_ignores_non_write_edit_tools() {
    // Arrange: Bash 工具调用不应被提取
    let args = serde_json::json!({"command": "ls"});
    let msg = BaseMessage::ai_with_tool_calls(
        "推理中...",
        vec![ToolCallRequest::new("c1", "Bash", args)],
    );

    // Act
    let changes = extract_file_changes(&[msg]);

    // Assert
    assert!(changes.is_empty(), "Bash 工具调用不应被提取");
}

/// P1：tool_calls 与 content_blocks 双路径对同一 id 只计一次；
/// 不同 id 的调用仍全部计入。
#[test]
fn test_extract_file_changes_deduplicates_by_id() {
    // ai_from_blocks：ToolUse 同步到 tool_calls，同 id 双路径
    let msgs = vec![
        make_ai_write_block("a.txt", "hello"),
        make_ai_edit_call("b.txt", "old", "new"), // ai_with_tool_calls：仅 tool_calls 路径
    ];
    let changes = extract_file_changes(&msgs);
    assert_eq!(changes.len(), 2, "两个不同 id 的调用各计一次");
}

// ── N9：builtin `workspace` effective name 归一 ────────────────────────────

/// 正向：模型面名字 `mcp__workspace__Write` / `mcp__workspace__Edit` 经归一后
/// 必须被收集（两种消息格式各一条断言），且内容与裸名路径一致。
#[test]
fn rewind_collects_workspace_effective_write_and_edit() {
    // Arrange: OpenAI 格式（仅 tool_calls 路径）的 effective Write
    let write_args = serde_json::json!({
        "file_path": "src/ws_write.rs",
        "content": "hello",
    });
    let openai_msg = BaseMessage::ai_with_tool_calls(
        "推理中...",
        vec![ToolCallRequest::new(
            "call_ws_write",
            "mcp__workspace__Write",
            write_args,
        )],
    );

    // Act
    let openai_changes = extract_file_changes(&[openai_msg]);

    // Assert: 归一命中 ⇒ 收集为 Write，file_path 逐字保留
    assert_eq!(
        openai_changes.len(),
        1,
        "`mcp__workspace__Write` 应被归一为 Write 并收集"
    );
    assert!(
        matches!(
            &openai_changes[0],
            FileChange::Write { path, .. } if path.as_str() == "src/ws_write.rs"
        ),
        "应为 Write 变体且 path == src/ws_write.rs"
    );

    // Arrange: Anthropic 格式的 effective Edit——只填 content blocks（tool_calls 留空），
    // 让 ToolUse 路径独立承载断言：`ai_from_blocks` 会把 ToolUse 同步进 tool_calls，
    // tool_calls 路径会先收集，从而掩盖 ToolUse 路径的归一失效（破坏实验已验证）。
    let edit_input = serde_json::json!({
        "file_path": "src/ws_edit.rs",
        "old_string": "old text",
        "new_string": "new text",
    });
    let anthropic_msg = BaseMessage::ai(MessageContent::Blocks(vec![ContentBlock::tool_use(
        "toolu_ws_edit",
        "mcp__workspace__Edit",
        edit_input,
    )]));

    // Act
    let anthropic_changes = extract_file_changes(&[anthropic_msg]);

    // Assert: 归一命中 ⇒ 收集为 Edit（仅 ToolUse 路径，产出 1 条），old/new 保留
    assert_eq!(
        anthropic_changes.len(),
        1,
        "`mcp__workspace__Edit` 应被归一为 Edit 并收集（仅 ToolUse 路径）"
    );
    assert!(
        matches!(
            &anthropic_changes[0],
            FileChange::Edit { path, old_string, new_string }
                if path.as_str() == "src/ws_edit.rs"
                    && old_string == "old text"
                    && new_string == "new text"
        ),
        "应为 Edit 变体且 path/old_string/new_string 逐字来自 arguments"
    );
}

/// 反例：未注册实例的 `mcp__foo__Write` 不得被归一命中 ⇒ 不收集（保守语义：
/// 未知 / 外部 `mcp__*` 不按 Write/Edit 处理）。
#[test]
fn rewind_ignores_unknown_effective_names() {
    let args = serde_json::json!({"file_path": "a.txt", "content": "hello"});

    // OpenAI 格式：未注册实例名 + 大小写近似名（纯查表区分大小写）
    for name in ["mcp__foo__Write", "mcp__workspace__write"] {
        let openai_msg = BaseMessage::ai_with_tool_calls(
            "推理中...",
            vec![ToolCallRequest::new("call_unknown", name, args.clone())],
        );
        let changes = extract_file_changes(&[openai_msg]);
        assert!(
            changes.is_empty(),
            "`{name}` 未命中归一表，不得被收集（保守语义）"
        );

        // Anthropic 格式：同批断言，两格式行为不得分裂（只填 content blocks，
        // 让 ToolUse 路径独立承载断言，不被 tool_calls 路径掩盖）
        let anthropic_msg = BaseMessage::ai(MessageContent::Blocks(vec![ContentBlock::tool_use(
            "toolu_unknown",
            name,
            args.clone(),
        )]));
        let changes = extract_file_changes(&[anthropic_msg]);
        assert!(
            changes.is_empty(),
            "`{name}`（ToolUse 路径）未命中归一表，不得被收集"
        );
    }
}

// ── validate_tool_pairing 测试 ────────────────────────────────────────────
//
// validate_tool_pairing 仅打日志（warn），不返回值也不 panic。
// 这些测试主要验证不 panic 且能遍历所有消息类型。

#[test]
fn test_validate_tool_pairing_empty_messages_no_panic() {
    // Arrange
    let msgs: Vec<BaseMessage> = vec![];

    // Act: 不应 panic
    validate_tool_pairing(&msgs);
}

#[test]
fn test_validate_tool_pairing_paired_openai_format_no_panic() {
    // Arrange: OpenAI 格式的 ToolUse + 配对 ToolResult
    let ai_msg = BaseMessage::ai_with_tool_calls(
        "推理",
        vec![ToolCallRequest::new(
            "call_paired_1",
            "Bash",
            serde_json::json!({}),
        )],
    );
    let tool_msg = BaseMessage::tool_result("call_paired_1", "结果");
    let msgs = vec![ai_msg, tool_msg];

    // Act: 不应 panic
    validate_tool_pairing(&msgs);
}

#[test]
fn test_validate_tool_pairing_orphan_tool_use_no_panic() {
    // Arrange: ToolUse 无对应 ToolResult（如 rewind 截断点在工具调用之后、结果之前）
    let ai_msg = BaseMessage::ai_with_tool_calls(
        "推理",
        vec![ToolCallRequest::new(
            "call_orphan_use",
            "Bash",
            serde_json::json!({}),
        )],
    );
    let msgs = vec![ai_msg];

    // Act: 仅 warn，不应 panic
    validate_tool_pairing(&msgs);
}

#[test]
fn test_validate_tool_pairing_orphan_tool_result_no_panic() {
    // Arrange: ToolResult 无对应 ToolUse
    let tool_msg = BaseMessage::tool_result("call_orphan_result", "结果");
    let msgs = vec![tool_msg];

    // Act: 仅 warn，不应 panic
    validate_tool_pairing(&msgs);
}

#[test]
fn test_validate_tool_pairing_anthropic_format_no_panic() {
    // Arrange: Anthropic 格式（ContentBlock::ToolUse）的 AI 消息
    let ai_msg = BaseMessage::ai_from_blocks(vec![ContentBlock::tool_use(
        "toolu_anthropic_1",
        "Bash",
        serde_json::json!({}),
    )]);
    let tool_msg = BaseMessage::tool_result("toolu_anthropic_1", "结果");
    let msgs = vec![ai_msg, tool_msg];

    // Act: 不应 panic
    validate_tool_pairing(&msgs);
}

#[test]
fn test_validate_tool_pairing_mixed_message_types_no_panic() {
    // Arrange: Human / System / Ai / Tool 混合
    let msgs = vec![
        BaseMessage::human("问题"),
        BaseMessage::system("系统"),
        BaseMessage::ai("回答"),
        BaseMessage::tool_result("call_mixed", "结果"),
    ];

    // Act: 不应 panic
    validate_tool_pairing(&msgs);
}

// ── execute 测试 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_execute_missing_target_returns_error_feedback() {
    // Phase 5 Step 5：参数形态由 serde_json 迁入 ArgsSchema（positional +
    // flag）；解析失败收敛为 feedback(Error, UiOnly)，RewindError 事件通道
    // 已删除——命令自身零事件发射（编排层 emit_command_feedback 统一发射）。
    // Arrange: 缺 target_message_id（空参数）
    let sink = Arc::new(MockEventSink::new());
    let history = vec![BaseMessage::human("你好")];
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), "".to_string());
    let cmd = RewindCommand;

    // Act
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 返回原始 history（未修改），EndTurn
    assert_eq!(result.messages.len(), 1, "参数错误应返回原始 history");
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 解析失败经 feedback 返回（UiOnly），不再推送 RewindError 事件
    let fb = result.feedback.as_ref().expect("解析失败应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert!(fb.message.contains("rewind 参数解析失败"));
    assert!(
        sink.events().is_empty(),
        "命令自身不应发射任何事件（RewindError 变体已删除）"
    );
}

#[tokio::test]
async fn test_execute_target_not_found_returns_error_feedback() {
    // Arrange: 目标 message_id 不在 history 中
    let sink = Arc::new(MockEventSink::new());
    let history = vec![
        BaseMessage::human("第一条"),
        BaseMessage::ai("回复"),
        BaseMessage::human("第二条"),
    ];
    let target_id = "nonexistent-uuid-0000-0000-000000000000";
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history.clone(), "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 返回完整 history，EndTurn
    assert_eq!(result.messages.len(), 3, "未找到目标时应返回完整 history");
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 未找到目标经 feedback 返回（UiOnly），不再推送 RewindError 事件
    let fb = result.feedback.as_ref().expect("未找到目标应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert!(
        fb.message.contains("未找到目标消息"),
        "错误消息应包含 '未找到目标消息'，实际: {}",
        fb.message
    );
    assert!(
        sink.events().is_empty(),
        "命令自身不应发射任何事件（RewindError 变体已删除）"
    );
}

#[tokio::test]
async fn test_execute_tail_truncation_keeps_messages_before_target() {
    // 场景：末尾截断 —— 目标是最后一条用户消息，
    // 其后的 AI 回复全部移除，保留前面的对话。
    //
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("第一问");
    let m2 = BaseMessage::ai("第一答");
    let m3 = BaseMessage::human("第二问"); // 目标：移除 m3 及之后
    let m4 = BaseMessage::ai("第二答");
    let target_id = m3.id().as_uuid().to_string();
    let history = vec![m1.clone(), m2.clone(), m3, m4];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 保留 m1, m2（目标之前）
    assert_eq!(result.messages.len(), 2, "末尾截断应保留目标前的 2 条");
    assert_eq!(result.messages[0].id(), m1.id());
    assert_eq!(result.messages[1].id(), m2.id());
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 成功 summary → feedback(Info, UiOnly)（Phase 5 Step 5 收敛）
    let fb = result.feedback.as_ref().expect("成功应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Info);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert_eq!(fb.message, "已回滚 2 条消息");

    // 应推送 rewind_completed 事件（重建信号保留）
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].1.contains("rewind_completed"),
        "应推送 rewind_completed，实际: {}",
        events[0].1
    );
    assert!(
        events[0].1.contains("已回滚 2 条消息"),
        "摘要应报告回滚 2 条，实际: {}",
        events[0].1
    );
}

#[tokio::test]
async fn test_execute_middle_truncation_keeps_prefix_only() {
    // 场景：中间截断 —— 目标在历史中间，
    // 其后的所有消息（含后续问答）全部移除。
    //
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("Q1");
    let m2 = BaseMessage::ai("A1");
    let m3 = BaseMessage::human("Q2"); // 目标
    let m4 = BaseMessage::ai("A2");
    let m5 = BaseMessage::human("Q3");
    let m6 = BaseMessage::ai("A3");
    let target_id = m3.id().as_uuid().to_string();
    let history = vec![m1.clone(), m2.clone(), m3, m4, m5, m6];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 保留 m1, m2
    assert_eq!(result.messages.len(), 2, "中间截断应保留前 2 条");
    assert_eq!(result.messages[0].id(), m1.id());
    assert_eq!(result.messages[1].id(), m2.id());

    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].1.contains("已回滚 4 条消息"),
        "应回滚 4 条（m3-m6），实际: {}",
        events[0].1
    );
}

#[tokio::test]
async fn test_execute_head_truncation_returns_empty() {
    // 场景：目标是第一条消息 —— 保留为空。
    //
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("Q1");
    let m2 = BaseMessage::ai("A1");
    let target_id = m1.id().as_uuid().to_string();
    let history = vec![m1, m2];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 保留为空
    assert!(result.messages.is_empty(), "回滚到第一条应保留空历史");
    assert!(
        sink.events()[0].1.contains("已回滚 2 条消息"),
        "应回滚全部 2 条"
    );
}

/// [回归测试] Workspace 能力缺失时不得修改计算宿主同名文件或裁剪历史。
#[tokio::test]
async fn test_execute_revert_files_requires_trusted_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let full = dir.path().join("tail_written.txt");
    std::fs::write(&full, "host-only").unwrap();
    let sink = Arc::new(MockEventSink::new());
    let first = BaseMessage::human("before");
    let write = make_ai_write_call("tail_written.txt", "remote-content");
    let target = write.id().as_uuid().to_string();
    let ctx = make_ctx(
        sink.clone(),
        vec![first, write],
        dir.path().display().to_string(),
        target,
    );
    let result = execute_rewind_cmd(&RewindCommand, ctx).await;
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.feedback.unwrap().level, FeedbackLevel::Error);
    assert_eq!(std::fs::read_to_string(full).unwrap(), "host-only");
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn test_execute_revert_files_false_preserves_files() {
    // 场景：revert_files=false，即使被移除消息含 Write 工具调用，
    // 文件也不应被恢复（删除）。
    //
    // Arrange
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_rel = "preserved.txt";
    let full = dir.path().join(file_rel);
    std::fs::write(&full, "created").expect("写入文件失败");

    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("请创建文件");
    let m2 = make_ai_write_call(file_rel, "created");
    let target_id = m2.id().as_uuid().to_string();
    let history = vec![m1, m2];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, cwd, args);
    let cmd = RewindCommand;

    // Act
    let _ = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 文件仍存在
    assert!(full.exists(), "revert_files=false 应保留文件");
}

#[tokio::test]
async fn test_execute_rewind_completed_event_carries_retained_messages() {
    // 契约：RewindCompleted 事件的 messages 字段应与 CommandResult.messages 一致
    // （都来自 retained_messages.clone()）。
    //
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("Q1");
    let m2 = BaseMessage::ai("A1");
    let m3 = BaseMessage::human("Q2"); // 目标
    let target_id = m3.id().as_uuid().to_string();
    let history = vec![m1.clone(), m2.clone(), m3];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 事件存在且为 rewind_completed
    let events = sink.events();
    let completed = events
        .iter()
        .find(|(_, json)| json.contains("rewind_completed"));
    assert!(
        completed.is_some(),
        "应推送 rewind_completed 事件，实际事件数: {}",
        events.len()
    );

    // CommandResult.messages 与事件共享同一份 retained_messages
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].id(), m1.id());
    assert_eq!(result.messages[1].id(), m2.id());
}

#[tokio::test]
async fn test_execute_does_not_call_push_done_itself() {
    // 对应 TRAP: CLAUDE.md issue_2026-05-29-immediate-command-missing-push-done
    // RewindCommand 是 Immediate 命令，自身不应调用 push_done（由 executor 负责）。
    //
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("Q1");
    let target_id = m1.id().as_uuid().to_string();
    let history = vec![m1];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 自身不调用 push_done
    assert_eq!(
        sink.push_done_count(),
        0,
        "RewindCommand 自身不应调用 push_done，由 executor 负责"
    );
}

#[tokio::test]
async fn test_execute_with_orphan_tool_pairing_in_retained_does_not_panic() {
    // 场景：保留的消息中存在未配对的 ToolUse（目标消息恰好在 ToolResult 之前），
    // validate_tool_pairing 应仅 warn 不 panic，execute 正常返回。
    //
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("Q1");
    // m2 是带 ToolUse 但无 ToolResult 的 AI 消息（保留）
    let m2 = BaseMessage::ai_with_tool_calls(
        "推理",
        vec![ToolCallRequest::new(
            "call_orphan",
            "Bash",
            serde_json::json!({}),
        )],
    );
    // m3 是目标（被移除）
    let m3 = BaseMessage::human("Q2");
    let target_id = m3.id().as_uuid().to_string();
    let history = vec![m1.clone(), m2.clone(), m3];
    let args = format!("{target_id} --no-revert-files");
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act: 不应 panic
    let result = execute_rewind_cmd(&cmd, ctx).await;

    // Assert: 保留 m1, m2（含未配对 ToolUse）
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].id(), m1.id());
    assert_eq!(result.messages[1].id(), m2.id());
}

/// P0：参数缺 revert_files 时（TUI 旧版本/第三方客户端）应默认回退文件，
/// 而不是进入解析失败静默路径。
#[tokio::test]
async fn test_execute_missing_revert_files_defaults_true() {
    let sink = Arc::new(MockEventSink::new());
    let history = vec![
        BaseMessage::human("第一轮问题"),
        BaseMessage::ai("第一轮回答"),
        BaseMessage::human("第二轮问题"),
    ];
    let target_id = history[0].id().as_uuid().to_string();
    let ctx = make_ctx(
        sink.clone(),
        history.clone(),
        std::env::temp_dir().to_string_lossy().to_string(),
        // 只传 target_message_id（ArgsSchema positional），缺 --no-revert-files
        // → revert_files 默认 true
        target_id,
    );

    let result = execute_rewind_cmd(&RewindCommand, ctx).await;

    let events = sink.events();
    assert!(
        !events.iter().any(|(_, json)| json.contains("参数解析失败")),
        "缺 --no-revert-files 不应进入解析失败路径"
    );
    // 成功路径：feedback(Info, UiOnly) + RewindCompleted 事件仍发射
    let fb = result.feedback.as_ref().expect("成功应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Info);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert_eq!(
        result.messages.len(),
        0,
        "回退到第一条 → 保留 0 条（截断已执行）"
    );
    assert!(
        events
            .iter()
            .any(|(_, json)| json.contains("rewind_completed")),
        "成功路径必须仍发射 RewindCompleted 事件（TUI 重建信号）"
    );
}
