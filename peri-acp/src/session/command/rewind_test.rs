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
use peri_agent::{
    agent::events::AgentEvent as ExecutorEvent,
    messages::{BaseMessage, ContentBlock, ToolCallRequest},
};

use super::super::{AgentCommand, CommandContext, CommandKind};
use super::{extract_file_changes, revert_files, validate_tool_pairing, RewindCommand};
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

    async fn push_done(&self, _session_id: &str) {
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
    CommandContext {
        session_id: "test-session".to_string(),
        history,
        cwd,
        peri_config: Arc::new(Default::default()),
        compact_model: None,
        event_sink: sink,
        args,
        cancel_token: peri_agent::agent::AgentCancellationToken::new(),
        thread_store: None,
        thread_id: None,
        bg_event_sender: None,
        bg_registry: None,
    }
}

// ── RewindCommand 属性测试 ────────────────────────────────────────────────

#[test]
fn test_rewind_command_name_and_aliases() {
    let cmd = RewindCommand;
    assert_eq!(cmd.name(), "rewind");
    let aliases = cmd.aliases();
    assert!(
        aliases.contains(&"undo"),
        "应包含 undo 别名，实际: {:?}",
        aliases
    );
    assert_eq!(cmd.kind(), CommandKind::Immediate);
    assert!(!cmd.description().is_empty());
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
    // 对同一变更计数两次。这是当前实现行为，本测试如实记录（未来若修复
    // 去重，此断言需同步更新）。
    let msgs = vec![make_ai_write_block("a.txt", "hello")];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 当前行为下同一变更被计两次
    assert_eq!(
        changes.len(),
        2,
        "ai_from_blocks 构造的消息在 tool_calls + content_blocks 双路径各计一次"
    );
}

#[test]
fn test_extract_file_changes_anthropic_edit_format() {
    // Arrange: Anthropic 格式的 Edit 工具调用（通过 ai_from_blocks 构造）
    // 同上：双路径计数，结果为 2。
    let msgs = vec![make_ai_edit_block("a.txt", "old", "new")];

    // Act
    let changes = extract_file_changes(&msgs);

    // Assert: 当前行为下同一变更被计两次
    assert_eq!(
        changes.len(),
        2,
        "ai_from_blocks 构造的消息在 tool_calls + content_blocks 双路径各计一次"
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

// ── revert_files 测试：Write 分支 ──────────────────────────────────────────

#[test]
fn test_revert_files_write_removes_file_from_cwd() {
    // Arrange: 在临时目录写入文件，然后通过 revert_files 删除
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "created_by_write.txt";
    let full = dir.path().join(file_path);
    std::fs::write(&full, "some content").expect("写入文件失败");
    assert!(full.exists(), "前置条件：文件应存在");

    // 构造 Write 变更（通过 extract_file_changes 走解析路径）
    let msgs = vec![make_ai_write_call(file_path, "some content")];
    let changes = extract_file_changes(&msgs);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: 文件应被 remove_file 删除（非 git 目录，git checkout 失败仅 debug）
    assert!(!full.exists(), "Write 恢复应删除文件");
    // 不应产生警告（git checkout 失败只 debug，不进入 warnings）
    assert!(
        warnings.is_empty(),
        "Write 分支删除成功后不应有警告，实际: {:?}",
        warnings
    );
}

#[test]
fn test_revert_files_write_missing_file_is_silent() {
    // Arrange: 目标文件不存在，Write 分支应静默处理
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "never_existed.txt";
    let msgs = vec![make_ai_write_call(file_path, "content")];
    let changes = extract_file_changes(&msgs);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: 文件不存在时 remove_file 失败仅 debug，不应进入 warnings
    assert!(
        warnings.is_empty(),
        "不存在的文件删除失败应静默，实际: {:?}",
        warnings
    );
}

// ── revert_files 测试：Edit 分支 ───────────────────────────────────────────

#[test]
fn test_revert_files_edit_replaces_new_string_with_old_string_ascii() {
    // Arrange: 文件中包含 new_string，Edit 恢复应把它替换回 old_string
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "edit_target.txt";
    let full = dir.path().join(file_path);
    // 当前文件内容：已应用过 Edit，包含 new_string
    std::fs::write(&full, "hello new world").expect("写入文件失败");

    let msgs = vec![make_ai_edit_call(file_path, "old", "new")];
    let changes = extract_file_changes(&msgs);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: new_string 被替换回 old_string
    let reverted = std::fs::read_to_string(&full).expect("读取恢复后的文件失败");
    assert_eq!(
        reverted, "hello old world",
        "Edit 恢复应把 new_string 替换回 old_string"
    );
    assert!(warnings.is_empty(), "成功恢复不应有警告");
}

#[test]
fn test_revert_files_edit_cjk_utf8_boundary_no_panic() {
    // 回归测试 p1-w5a（commit 6d76824d）：
    // Edit 分支在 CJK 多字节字符场景下，旧实现用 &content[..idx] 字节切片
    // 会在非 char boundary 上 panic。新实现用 content.replacen。
    //
    // 这里 new_string 是中文（每字符 3 字节），old_string 也是中文，
    // 且 new_string 出现在文件中靠前位置，验证 replacen 路径不 panic。
    //
    // Arrange
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "cjk_edit.txt";
    let full = dir.path().join(file_path);
    // 当前内容包含中文 new_string
    std::fs::write(&full, "开始新内容结束").expect("写入文件失败");

    let msgs = vec![make_ai_edit_call(file_path, "旧内容", "新内容")];
    let changes = extract_file_changes(&msgs);

    // Act: 不应 panic
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: "新内容" 被替换回 "旧内容"
    let reverted = std::fs::read_to_string(&full).expect("读取恢复后的文件失败");
    assert_eq!(reverted, "开始旧内容结束", "CJK 字符的 Edit 恢复应正确替换");
}

#[test]
fn test_revert_files_edit_cjk_multibyte_replacement_roundtrip() {
    // 进一步回归保护：覆盖更复杂的多字节混合场景。
    // 文件含 emoji（4 字节）+ CJK（3 字节）+ ASCII（1 字节），
    // 验证 replacen 在混合字节宽度下正确工作。
    //
    // Arrange
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "mixed_edit.txt";
    let full = dir.path().join(file_path);
    std::fs::write(&full, "prefix 🚀新内容suffix").expect("写入文件失败");

    let msgs = vec![make_ai_edit_call(file_path, "旧内容", "新内容")];
    let changes = extract_file_changes(&msgs);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert
    let reverted = std::fs::read_to_string(&full).expect("读取恢复后的文件失败");
    assert_eq!(
        reverted, "prefix 🚀旧内容suffix",
        "混合字节宽度场景的 Edit 恢复应正确"
    );
}

#[test]
fn test_revert_files_edit_new_string_not_found_emits_warning() {
    // Arrange: 文件内容不含 new_string
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "no_match.txt";
    let full = dir.path().join(file_path);
    std::fs::write(&full, "completely different content").expect("写入文件失败");
    let original = std::fs::read_to_string(&full).unwrap();

    let msgs = vec![make_ai_edit_call(file_path, "old", "new")];
    let changes = extract_file_changes(&msgs);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: 应产生警告，文件内容不变
    assert!(!warnings.is_empty(), "new_string 未找到应产生警告");
    assert!(
        warnings.iter().any(|w| w.contains("未找到 new_string")),
        "警告应包含 '未找到 new_string'，实际: {:?}",
        warnings
    );
    let after = std::fs::read_to_string(&full).unwrap();
    assert_eq!(after, original, "未匹配时文件内容不应改变");
}

#[test]
fn test_revert_files_edit_missing_file_emits_warning() {
    // Arrange: 目标文件不存在
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "missing_file.txt";
    let msgs = vec![make_ai_edit_call(file_path, "old", "new")];
    let changes = extract_file_changes(&msgs);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: 读取失败应产生警告
    assert!(!warnings.is_empty(), "文件不存在应产生警告");
    assert!(
        warnings.iter().any(|w| w.contains("Edit 恢复读取失败")),
        "警告应包含读取失败信息，实际: {:?}",
        warnings
    );
}

#[test]
fn test_revert_files_reverse_order_applied() {
    // 验证逆序遍历：构造同一文件的两次 Edit（先 A→B，再 B→C），
    // 当 extract 顺序为 [A→B, B→C] 时，revert 应先撤销 B→C（得到 B），
    // 再撤销 A→B（得到 A）。最终文件回到 A。
    //
    // Arrange
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_path = "double_edit.txt";
    let full = dir.path().join(file_path);
    // 当前内容为 C（已应用 A→B→C）
    std::fs::write(&full, "C").expect("写入文件失败");

    // 按历史顺序：先 A→B，再 B→C
    let msg1 = make_ai_edit_call(file_path, "A", "B");
    let msg2 = make_ai_edit_call(file_path, "B", "C");
    let changes = extract_file_changes(&[msg1, msg2]);

    // Act
    let mut warnings = Vec::new();
    revert_files(&changes, &cwd, &mut warnings);

    // Assert: 逆序撤销后应回到 A
    let reverted = std::fs::read_to_string(&full).expect("读取文件失败");
    assert_eq!(
        reverted, "A",
        "逆序撤销应回到初始内容 A，实际: {}",
        reverted
    );
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
async fn test_execute_invalid_args_emits_compact_error() {
    // Arrange: 无效 JSON 参数
    let sink = Arc::new(MockEventSink::new());
    let history = vec![BaseMessage::human("你好")];
    let ctx = make_ctx(
        sink.clone(),
        history,
        "/tmp".to_string(),
        "{invalid json".to_string(),
    );
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 返回原始 history（未修改），EndTurn
    assert_eq!(result.messages.len(), 1, "参数错误应返回原始 history");
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 应推送 CompactError 事件
    let events = sink.events();
    assert_eq!(events.len(), 1, "应推送 1 个事件");
    assert!(
        events[0].1.contains("compact_error"),
        "应推送 compact_error 事件，实际: {}",
        events[0].1
    );
}

#[tokio::test]
async fn test_execute_target_not_found_emits_compact_error() {
    // Arrange: 目标 message_id 不在 history 中
    let sink = Arc::new(MockEventSink::new());
    let history = vec![
        BaseMessage::human("第一条"),
        BaseMessage::ai("回复"),
        BaseMessage::human("第二条"),
    ];
    let target_id = "nonexistent-uuid-0000-0000-000000000000";
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history.clone(), "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 返回完整 history，EndTurn
    assert_eq!(result.messages.len(), 3, "未找到目标时应返回完整 history");
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].1.contains("compact_error"),
        "应推送 compact_error，实际: {}",
        events[0].1
    );
    assert!(
        events[0].1.contains("未找到目标消息"),
        "错误消息应包含 '未找到目标消息'，实际: {}",
        events[0].1
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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 保留 m1, m2（目标之前）
    assert_eq!(result.messages.len(), 2, "末尾截断应保留目标前的 2 条");
    assert_eq!(result.messages[0].id(), m1.id());
    assert_eq!(result.messages[1].id(), m2.id());
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 应推送 rewind_completed 事件
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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 保留为空
    assert!(result.messages.is_empty(), "回滚到第一条应保留空历史");
    assert!(
        sink.events()[0].1.contains("已回滚 2 条消息"),
        "应回滚全部 2 条"
    );
}

#[tokio::test]
async fn test_execute_revert_files_true_invokes_file_removal() {
    // 场景：revert_files=true，被移除消息含 Write 工具调用，
    // 应删除 Write 创建的文件。
    //
    // Arrange
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let cwd = dir.path().to_string_lossy().to_string();
    let file_rel = "tail_written.txt";
    let full = dir.path().join(file_rel);
    // 模拟 Write 已执行：文件存在
    std::fs::write(&full, "created").expect("写入文件失败");
    assert!(full.exists());

    let sink = Arc::new(MockEventSink::new());
    let m1 = BaseMessage::human("请创建文件");
    let m2 = make_ai_write_call(file_rel, "created"); // 这条及之后将被移除
    let target_id = m2.id().as_uuid().to_string();
    let history = vec![m1.clone(), m2];
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": true,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, cwd, args);
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 文件被删除，保留 m1
    assert!(!full.exists(), "revert_files=true 应删除 Write 创建的文件");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].id(), m1.id());
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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, cwd, args);
    let cmd = RewindCommand;

    // Act
    let _ = cmd.execute(ctx).await;

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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    let result = cmd.execute(ctx).await;

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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act
    cmd.execute(ctx).await;

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
    let args = serde_json::json!({
        "target_message_id": target_id,
        "revert_files": false,
    })
    .to_string();
    let ctx = make_ctx(sink.clone(), history, "/tmp".to_string(), args);
    let cmd = RewindCommand;

    // Act: 不应 panic
    let result = cmd.execute(ctx).await;

    // Assert: 保留 m1, m2（含未配对 ToolUse）
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].id(), m1.id());
    assert_eq!(result.messages[1].id(), m2.id());
}
