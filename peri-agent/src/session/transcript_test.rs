use super::*;
use crate::messages::MessageContent;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
}

fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
    BaseMessage::tool_result(
        tool_call_id.to_string(),
        MessageContent::text(text.to_string()),
    )
}

// ── 基础构造 ──────────────────────────────────────────────────────────────

#[test]
fn test_new_transcript_is_empty() {
    let t = MessageTranscript::new();
    assert!(t.is_empty());
    assert_eq!(t.len(), 0);
    assert_eq!(t.ancestor_len(), 0);
}

#[test]
fn test_with_ancestor_sets_boundary() {
    let a1 = make_human("ancestor-1");
    let a2 = make_human("ancestor-2");
    let t = MessageTranscript::new().with_ancestor(vec![a1.clone(), a2.clone()]);

    assert_eq!(t.len(), 2);
    assert_eq!(t.ancestor_len(), 2);
    assert!(t.get(a1.id()).is_some());
    assert!(t.get(a2.id()).is_some());
}

// ── ID 寻址 ─────────────────────────────────────────────────────────────────

#[test]
fn test_id_indexing_o1_lookup() {
    let mut t = MessageTranscript::new();
    let m1 = make_human("msg-1");
    let m2 = make_human("msg-2");
    let m3 = make_human("msg-3");

    let id1 = t.append(m1);
    let id2 = t.append(m2);
    let id3 = t.append(m3);

    assert_eq!(t.len(), 3);
    // 所有 id 可找到
    assert!(t.get(id1).is_some());
    assert!(t.get(id2).is_some());
    assert!(t.get(id3).is_some());
    // 不存在的 id 返回 None
    let ghost_id = MessageId::new();
    assert!(t.get(ghost_id).is_none());
}

#[test]
fn test_append_returns_correct_id() {
    let mut t = MessageTranscript::new();
    let msg = make_human("hello");
    let id = t.append(msg);
    // 返回的 id 应与消息内部 id 一致
    assert_eq!(t.get(id).unwrap().message.id(), id);
}

#[test]
fn test_append_batch() {
    let mut t = MessageTranscript::new();
    let msgs = vec![make_human("a"), make_human("b"), make_human("c")];
    let ids = t.append_batch(msgs);

    assert_eq!(ids.len(), 3);
    assert_eq!(t.len(), 3);
    // 按 append 顺序存储
    assert_eq!(t.entries()[0].message.content(), "a");
    assert_eq!(t.entries()[1].message.content(), "b");
    assert_eq!(t.entries()[2].message.content(), "c");
}

// ── Staging 两阶段写入 ────────────────────────────────────────────────────

#[test]
fn test_staging_commit_atomic() {
    let mut t = MessageTranscript::new();
    // 先追加一条用户消息
    t.append(make_human("user question"));

    // Stage AI 消息
    let ai_msg = make_ai("thinking...");
    t.stage_ai_message(ai_msg);
    assert!(t.has_staged());
    // Staging 期间主列表不变
    assert_eq!(t.len(), 1);

    // Stage ToolResult
    t.stage_tool_result(make_tool_result("call_1", "result-1"));
    t.stage_tool_result(make_tool_result("call_2", "result-2"));

    // Commit
    t.commit_staged();
    assert!(!t.has_staged());
    // AI + 2 个 ToolResult = 3 条新消息
    assert_eq!(t.len(), 4);
    // 顺序：user → ai → tool1 → tool2
    assert_eq!(t.entries()[1].message.content(), "thinking...");
    assert_eq!(t.entries()[2].message.content(), "result-1");
    assert_eq!(t.entries()[3].message.content(), "result-2");
}

#[test]
fn test_staging_discard() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));

    let ai_msg = make_ai("will be discarded");
    t.stage_ai_message(ai_msg);
    t.stage_tool_result(make_tool_result("call_1", "also discarded"));
    assert!(t.has_staged());

    t.discard_staged();
    assert!(!t.has_staged());
    // 主列表不变
    assert_eq!(t.len(), 1);
}

#[test]
fn test_stage_tool_result_without_ai_message_is_noop() {
    let mut t = MessageTranscript::new();
    t.stage_tool_result(make_tool_result("call_1", "ignored"));
    assert!(!t.has_staged(), "无 AI 消息时 tool_result 应被忽略");
}

#[test]
fn test_stage_ai_message_overwrites_previous_staging() {
    let mut t = MessageTranscript::new();

    let ai1 = make_ai("first ai");
    t.stage_ai_message(ai1);
    t.stage_tool_result(make_tool_result("call_1", "result for first"));

    // 新的 AI 消息覆盖旧的 staging
    let ai2 = make_ai("second ai");
    t.stage_ai_message(ai2);
    // 旧的 tool_results 被丢弃
    t.stage_tool_result(make_tool_result("call_2", "result for second"));

    t.commit_staged();
    assert_eq!(t.len(), 2, "只有 ai2 + tool2，ai1 和 tool1 被丢弃");
    assert_eq!(t.entries()[0].message.content(), "second ai");
    assert_eq!(t.entries()[1].message.content(), "result for second");
}

#[test]
fn test_commit_without_staging_is_noop() {
    let mut t = MessageTranscript::new();
    t.append(make_human("existing"));
    t.commit_staged(); // 无 staging，不应 panic
    assert_eq!(t.len(), 1);
}

// ── 标记系统 ───────────────────────────────────────────────────────────────

#[test]
fn test_truncated_flag() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("truncatable"));
    assert_eq!(t.flags(id), MessageFlags::default());
    assert!(!t.flags(id).truncated);

    t.set_truncated(id, true);
    assert!(t.flags(id).truncated);
    assert!(!t.flags(id).excluded);

    t.set_truncated(id, false);
    assert!(!t.flags(id).truncated);
}

#[test]
fn test_excluded_flag() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("excludable"));

    t.set_excluded(id, true);
    assert!(t.flags(id).excluded);
    assert!(!t.flags(id).truncated);
}

#[test]
fn test_clear_flags() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("flagged"));
    t.set_truncated(id, true);
    t.set_excluded(id, true);

    t.clear_flags(id);
    let f = t.flags(id);
    assert!(!f.truncated);
    assert!(!f.excluded);
}

#[test]
fn test_visible_messages_skips_excluded() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("visible-1"));
    let id2 = t.append(make_human("will-be-excluded"));
    let id3 = t.append(make_human("visible-2"));

    t.set_excluded(id2, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 2, "excluded 消息应被过滤");
    assert_eq!(visible[0].id(), id1);
    assert_eq!(visible[1].id(), id3);
}

#[test]
fn test_visible_messages_keeps_truncated() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("truncated but visible"));
    t.set_truncated(id, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1, "truncated 消息仍然可见");
}

// ── Ancestor 边界 ──────────────────────────────────────────────────────────

#[test]
fn test_ancestor_boundary_is_readonly_concept() {
    let a1 = make_human("ancestor");
    let own = make_human("own message");
    let mut t = MessageTranscript::new().with_ancestor(vec![a1]);

    t.append(own);
    assert_eq!(t.ancestor_len(), 1);
    assert_eq!(t.len(), 2);
}

// ── Rewind ──────────────────────────────────────────────────────────────────

#[test]
fn test_rewind_to_truncates_correctly() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("keep-1"));
    let id2 = t.append(make_human("keep-2"));
    let _id3 = t.append(make_human("will-remove-1"));
    let _id4 = t.append(make_human("will-remove-2"));

    t.rewind_to(id2).unwrap();
    assert_eq!(t.len(), 2, "rewind 后应只保留 id1 + id2");
    assert!(t.get(id1).is_some());
    assert!(t.get(id2).is_some());
}

#[test]
fn test_rewind_clears_staging() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("target"));
    t.append(make_human("after"));

    t.stage_ai_message(make_ai("staged ai"));
    assert!(t.has_staged());

    t.rewind_to(id).unwrap();
    assert!(!t.has_staged(), "rewind 应清空 staging");
    assert_eq!(t.len(), 1);
}

#[test]
fn test_rewind_nonexistent_id_returns_error() {
    let mut t = MessageTranscript::new();
    t.append(make_human("only msg"));
    let ghost_id = MessageId::new();

    let result = t.rewind_to(ghost_id);
    assert!(result.is_err(), "rewind 不存在的 id 应返回错误");
}

#[test]
fn test_rewind_into_ancestor_returns_error() {
    let a1 = make_human("ancestor");
    let mut t = MessageTranscript::new().with_ancestor(vec![a1.clone()]);
    t.append(make_human("own"));

    let result = t.rewind_to(a1.id());
    assert!(result.is_err(), "rewind 到祖先区域应返回错误");
}

// ── Rebuild ───────────────────────────────────────────────────────────────

#[test]
fn test_rebuild_preserves_flags() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("msg-1"));
    let id2 = t.append(make_human("msg-2"));
    t.set_excluded(id1, true);

    // 重建：保留 id1 的 excluded 标记
    let entries = vec![
        (
            t.entries()[0].message.clone(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        ),
        (t.entries()[1].message.clone(), MessageFlags::default()),
    ];

    let t2 = t.rebuild(entries);
    assert_eq!(t2.len(), 2);
    assert!(t2.flags(id1).excluded, "rebuild 后标记应保留");
    assert!(!t2.flags(id2).excluded);
}

#[test]
fn test_rebuild_preserves_ancestor_and_persistence() {
    let mut t = MessageTranscript::new().with_ancestor(vec![make_human("ancestor")]);
    t.append(make_human("own-1"));
    t.append(make_human("own-2"));

    let entries: Vec<(BaseMessage, MessageFlags)> = t
        .entries()
        .iter()
        .map(|e| (e.message.clone(), MessageFlags::default()))
        .collect();

    let t2 = t.rebuild(entries);
    assert_eq!(t2.ancestor_len(), 1, "rebuild 应保留 ancestor_len");
    assert_eq!(t2.len(), 3);
}

#[test]
fn test_rebuild_clears_staging() {
    let mut t = MessageTranscript::new();
    t.append(make_human("msg"));
    t.stage_ai_message(make_ai("staged"));

    let entries = vec![(t.entries()[0].message.clone(), MessageFlags::default())];
    let t2 = t.rebuild(entries);
    assert!(!t2.has_staged(), "rebuild 应清空 staging");
}
