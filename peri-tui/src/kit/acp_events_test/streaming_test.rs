use super::*;

/// [回归测试] 块标记拆在 chunk 边界时仍须识别，UTF-8 回看不得切片 panic。
#[test]
fn test_block_boundary_split_markers_and_unicode() {
    for marker in ["```", "---", "***", "___", "# ", "\n\n", "\n中文\n"] {
        for split in marker.char_indices().map(|(offset, _)| offset).skip(1) {
            let mut text = String::from("已发布\n");
            let published = text.len();
            text.push_str(&marker[..split]);
            let chunk_start = text.len();
            text.push_str(&marker[split..]);
            assert!(
                has_md_block_boundary_since(&text, published, chunk_start),
                "{marker:?} at {split}"
            );
        }
    }
    let text = "已发布中文😀尾部";
    assert!(!has_md_block_boundary_since(
        text,
        "已发布".len(),
        "已发布中文".len()
    ));
}

/// [回归测试] 长单行逐 chunk 追加后仅在新块边界发布，不能重新命中已发布标记。
#[test]
fn test_block_boundary_long_unpublished_tail() {
    let mut text = String::from("# published\n");
    let mut published = text.len();
    for _ in 0..4096 {
        let chunk_start = text.len();
        text.push_str("中文😀x");
        assert!(!has_md_block_boundary_since(&text, published, chunk_start));
    }
    for (chunk, expected) in [("\n", false), ("下一行", false), ("\n", true)] {
        let chunk_start = text.len();
        text.push_str(chunk);
        assert_eq!(
            has_md_block_boundary_since(&text, published, chunk_start),
            expected
        );
        if expected {
            published = text.len();
        }
    }
    let chunk_start = text.len();
    text.push_str("正文");
    assert!(!has_md_block_boundary_since(&text, published, chunk_start));
}

struct BlockModeGuard(Option<String>);

/// [回归测试] None 模式积累的边界在切回 Block 时不能因增量扫描而漏发。
#[test]
#[serial]
fn test_block_stream_mode_switch_publishes_hidden_prefix() {
    let _mode = BlockModeGuard::new();
    let mut state = make_fold_test_state();
    let text = |chunk: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: chunk.into(),
            agent_id: None,
            message_id: None,
        })
    };
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("已发布")),
        PublicationIntent::Immediate
    );
    TUI_CONFIG_HANDLE.get().unwrap().write().streaming_mode = Some("none".into());
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("隐藏\n\n正文")),
        PublicationIntent::Hidden
    );
    TUI_CONFIG_HANDLE.get().unwrap().write().streaming_mode = Some("block".into());
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("追加")),
        PublicationIntent::Immediate
    );
    assert_eq!(state.last_pushed_text_len, state.current_turn.text.len());
}

impl BlockModeGuard {
    fn new() -> Self {
        let handle = TUI_CONFIG_HANDLE.get_or_init(|| {
            std::sync::Arc::new(parking_lot::RwLock::new(crate::config::TuiConfig::default()))
        });
        Self(handle.write().streaming_mode.replace("block".into()))
    }
}

impl Drop for BlockModeGuard {
    fn drop(&mut self) {
        TUI_CONFIG_HANDLE.get().unwrap().write().streaming_mode = self.0.take();
    }
}

/// [回归测试] 正文和推理的字节游标独立，工具 barrier 与新 turn 不保留旧扫描边界。
#[test]
#[serial]
fn test_block_stream_unicode_tool_barrier_and_turn_reset() {
    let _mode = BlockModeGuard::new();
    let mut state = make_fold_test_state();
    let text = |chunk: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: chunk.into(),
            agent_id: None,
            message_id: None,
        })
    };
    let reasoning = |chunk: &str| {
        AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: chunk.into(),
            agent_id: None,
            message_id: None,
        })
    };
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("中文\n")),
        PublicationIntent::Immediate
    );
    assert_eq!(state.last_pushed_text_len, "中文\n".len());
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("`")),
        PublicationIntent::Hidden
    );
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("`")),
        PublicationIntent::Hidden
    );
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("`")),
        PublicationIntent::Immediate
    );
    assert_eq!(
        dispatch_for_bridge(&mut state, &reasoning("推理")),
        PublicationIntent::Immediate
    );
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("未发布正文")),
        PublicationIntent::Hidden
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "tool".into(),
            tool_name: "Bash".into(),
            input_summary: "命令".into(),
            raw_input: serde_json::Value::Null,
            agent_id: None,
        }),
    );
    assert_eq!(state.last_pushed_text_len, state.current_turn.text.len());
    assert_eq!(
        state.last_pushed_reasoning_len,
        state.current_turn.reasoning.len()
    );
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("后续")),
        PublicationIntent::Hidden
    );
    dispatch_for_bridge(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(state.last_pushed_text_len, 0);
    assert_eq!(
        dispatch_for_bridge(&mut state, &text("新轮次")),
        PublicationIntent::Immediate
    );
}

// ── has_md_block_boundary_since 单元测试 ──

#[test]
fn test_boundary_published_bytes_zero_always_true() {
    assert!(
        has_md_block_boundary_since("hello", 0, 0),
        "published_bytes=0 应始终返回 true"
    );
}

#[test]
fn test_boundary_empty_string() {
    assert!(
        !has_md_block_boundary_since("", 1, 0),
        "空字符串不应触发边界"
    );
}

#[test]
fn test_boundary_paragraph_double_newline() {
    let text = "first paragraph\n\nsecond paragraph";
    // published_bytes=0 已推送；从字符 1 开始检查应有双换行
    assert!(
        has_md_block_boundary_since(text, 1, 1),
        "双换行应触发段落边界"
    );
}

#[test]
fn test_boundary_code_block() {
    let text = "some text\n```rust\nfn main() {}\n```";
    // 从 "some" 开始检查
    assert!(
        has_md_block_boundary_since(text, 1, 1),
        "代码块起止应触发边界"
    );
}

#[test]
fn test_boundary_heading() {
    let text = "intro\n# Heading\ncontent";
    assert!(has_md_block_boundary_since(text, 1, 1), "标题应触发边界");
}

#[test]
fn test_boundary_horizontal_rule() {
    let text = "text\n---\nmore";
    assert!(has_md_block_boundary_since(text, 1, 1), "水平线应触发边界");
}

#[test]
fn test_boundary_no_boundary_in_tail() {
    let text = "one line of text\nanother line without boundary";
    // published_bytes 越过已推送部分，尾部无边界
    let pushed = "one line of text".len();
    assert!(
        !has_md_block_boundary_since(text, pushed, pushed),
        "无分隔的连续文本不应触发边界"
    );
}

// ── current_streaming_mode 测试 ──

/// 默认（未设置 streaming_mode 或 PERI_CONFIG_HANDLE 未初始化）应返回 Streaming。
#[test]
#[serial]
fn test_mode_default_is_streaming() {
    // PERI_CONFIG_HANDLE 在测试中未初始化 → get() 返回 None → fallback 到 Streaming
    assert!(
        matches!(current_streaming_mode(), StreamingMode::Streaming),
        "未设置 streaming_mode 时应默认 Streaming"
    );
}
