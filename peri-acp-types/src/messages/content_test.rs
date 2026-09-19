use super::*;

#[test]
fn test_content_block_text() {
    let b = ContentBlock::text("hello");
    assert_eq!(b.as_text(), Some("hello"));
}

#[test]
fn test_message_content_text_content() {
    let mc = MessageContent::Blocks(vec![
        ContentBlock::reasoning("let me think..."),
        ContentBlock::text("final answer"),
    ]);
    assert_eq!(mc.text_content(), "final answer");
}

#[test]
fn test_content_blocks_from_string() {
    let mc = MessageContent::text("hello");
    let blocks = mc.content_blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].as_text(), Some("hello"));
}

#[test]
fn test_message_content_serde_roundtrip() {
    let mc = MessageContent::Blocks(vec![
        ContentBlock::text("hello"),
        ContentBlock::reasoning_with_signature("think", "sig123"),
    ]);
    let json = serde_json::to_string(&mc).unwrap();
    let mc2: MessageContent = serde_json::from_str(&json).unwrap();
    assert_eq!(mc, mc2);
}

#[test]
fn test_tool_use_blocks_consistency_with_has_tool_use() {
    // Blocks 变体
    let mc = MessageContent::Blocks(vec![
        ContentBlock::tool_use("id1", "Bash", serde_json::json!({"cmd": "ls"})),
        ContentBlock::text("text"),
    ]);
    assert!(mc.has_tool_use());
    assert_eq!(mc.tool_use_blocks().len(), 1);
    assert_eq!(mc.tool_use_blocks()[0].1, "Bash");

    // Text 变体 — 无工具调用
    let mc = MessageContent::text("plain text");
    assert!(!mc.has_tool_use());
    assert!(mc.tool_use_blocks().is_empty());

    // Raw 变体 — 含 tool_use
    let mc = MessageContent::Raw(vec![
        serde_json::json!({"type": "text", "text": "calling"}),
        serde_json::json!({"type": "tool_use", "id": "tc1", "name": "Read", "input": {"path": "a.rs"}}),
    ]);
    assert!(
        mc.has_tool_use(),
        "Raw 含 tool_use 时 has_tool_use 应为 true"
    );
    assert_eq!(
        mc.tool_use_blocks().len(),
        1,
        "tool_use_blocks 应与 has_tool_use 一致"
    );
}

#[test]
fn test_is_empty_variants() {
    assert!(MessageContent::text("").is_empty());
    assert!(!MessageContent::text("x").is_empty());
    assert!(MessageContent::Blocks(vec![]).is_empty());
    assert!(!MessageContent::Blocks(vec![ContentBlock::text("x")]).is_empty());
    assert!(MessageContent::Raw(vec![]).is_empty());
    assert!(
        !MessageContent::Raw(vec![serde_json::json!({"type": "text", "text": "x"})]).is_empty()
    );
}

#[test]
fn test_content_block_unknown_serde_roundtrip() {
    let raw = serde_json::json!({
        "type": "redacted_thinking",
        "data": "encrypted_content_here"
    });
    let block = ContentBlock::Unknown(raw.clone());
    let json = serde_json::to_string(&block).unwrap();
    let block2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(block, block2, "Unknown block 应完整保留原始 JSON");
    assert!(
        json.contains("redacted_thinking"),
        "序列化应保留原始 type 字段"
    );
}

#[test]
fn test_content_block_all_variants_roundtrip() {
    let blocks = vec![
        ContentBlock::text("hello"),
        ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/img.png".into(),
            },
        },
        ContentBlock::Document {
            source: DocumentSource::Text {
                text: "doc content".into(),
            },
            title: Some("My Doc".into()),
        },
        ContentBlock::tool_use("id1", "Bash", serde_json::json!({"cmd": "ls"})),
        ContentBlock::tool_result("id1", vec![ContentBlock::text("output")], false),
        ContentBlock::reasoning_with_signature("think", "sig123"),
        ContentBlock::Unknown(serde_json::json!({"type": "custom_block", "value": 42})),
    ];
    let json = serde_json::to_string(&blocks).unwrap();
    let blocks2: Vec<ContentBlock> = serde_json::from_str(&json).unwrap();
    assert_eq!(blocks, blocks2, "所有变体应完整 round-trip");
}

#[test]
fn test_content_block_unknown_deserialize_unknown_type() {
    let json = r#"{"type": "future_block_type", "data": {"nested": true}}"#;
    let block: ContentBlock = serde_json::from_str(json).unwrap();
    match block {
        ContentBlock::Unknown(v) => {
            assert_eq!(v["type"], "future_block_type");
            assert_eq!(v["data"]["nested"], true);
        }
        other => panic!("应为 Unknown，实际: {:?}", other),
    }
}

#[test]
fn test_content_block_image_url() {
    let b = ContentBlock::image_url("https://example.com/img.png");
    assert!(b.as_text().is_none());
    assert!(b.as_tool_use().is_none());
    assert!(b.as_reasoning().is_none());
    // Verify roundtrip
    let json = serde_json::to_string(&b).unwrap();
    let b2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(b, b2);
}

#[test]
fn test_content_block_image_base64() {
    let b = ContentBlock::image_base64("image/png", "iVBORw0KGgo=");
    assert!(b.as_text().is_none());
    // Verify roundtrip
    let json = serde_json::to_string(&b).unwrap();
    let b2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(b, b2);
    assert!(json.contains("\"media_type\":\"image/png\""));
}

#[test]
fn test_content_block_reasoning() {
    let b = ContentBlock::reasoning("thinking step by step");
    assert_eq!(b.as_reasoning(), Some("thinking step by step"));
    assert!(b.as_text().is_none());
    // Roundtrip without signature
    let json = serde_json::to_string(&b).unwrap();
    assert!(!json.contains("signature"));
    let b2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(b, b2);
}

#[test]
fn test_content_block_reasoning_with_signature() {
    let b = ContentBlock::reasoning_with_signature("deep thought", "sig_abc");
    assert_eq!(b.as_reasoning(), Some("deep thought"));
    // Roundtrip with signature
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains("sig_abc"));
    let b2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(b, b2);
}

#[test]
fn test_content_block_as_reasoning_non_reasoning() {
    let text = ContentBlock::text("hello");
    assert!(text.as_reasoning().is_none());
    let tool = ContentBlock::tool_use("id1", "Bash", serde_json::json!({}));
    assert!(tool.as_reasoning().is_none());
}

#[test]
fn test_content_block_document() {
    let b = ContentBlock::Document {
        source: DocumentSource::Text {
            text: "doc content".into(),
        },
        title: Some("My Doc".into()),
    };
    assert!(b.as_text().is_none());
    assert!(b.as_reasoning().is_none());
    let json = serde_json::to_string(&b).unwrap();
    let b2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(b, b2);
}

#[test]
fn test_content_block_tool_result_error() {
    let b = ContentBlock::tool_result("tu1", vec![ContentBlock::text("permission denied")], true);
    assert!(b.as_text().is_none());
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains("\"is_error\":true"));
    let b2: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(b, b2);
}

#[test]
fn test_message_content_from_conversions() {
    let mc: MessageContent = "hello".into();
    assert_eq!(mc.text_content(), "hello");

    let mc: MessageContent = String::from("world").into();
    assert_eq!(mc.text_content(), "world");

    let mc: MessageContent = vec![ContentBlock::text("block")].into();
    assert_eq!(mc.text_content(), "block");
}

#[test]
fn test_message_content_default() {
    let mc = MessageContent::default();
    assert!(mc.is_empty());
    assert_eq!(mc.text_content(), "");
}

#[test]
fn test_message_content_raw_tool_use_blocks() {
    let mc = MessageContent::Raw(vec![
        serde_json::json!({"type": "tool_use", "id": "r1", "name": "Read", "input": {}}),
        serde_json::json!({"type": "text", "text": "result"}),
    ]);
    let tus = mc.tool_use_blocks();
    assert_eq!(tus.len(), 1);
    assert_eq!(tus[0].0, "r1");
    assert_eq!(tus[0].1, "Read");
}

#[test]
fn test_message_content_text_content_from_raw() {
    let mc = MessageContent::Raw(vec![
        serde_json::json!({"type": "text", "text": "hello "}),
        serde_json::json!({"type": "text", "text": "world"}),
    ]);
    assert_eq!(mc.text_content(), "hello world");
}

// ── strip_system_reminders ────────────────────────────────────────────────────

#[test]
fn test_strip_system_reminders_removes_trailing_block() {
    let text = "请用 Write 工具创建文件\n\
                <system-reminder>\nCurrent permission mode: Bypass: All tool calls are allowed without approval.\n\
                </system-reminder>";
    assert_eq!(
        strip_system_reminders(text),
        "请用 Write 工具创建文件\n",
        "尾部注入的 reminder 应被剥离，用户输入保留"
    );
}

#[test]
fn test_strip_system_reminders_multiple_blocks() {
    let text = "问题一<system-reminder>recall a</system-reminder>问题二\
                <system-reminder>recall b</system-reminder>";
    assert_eq!(strip_system_reminders(text), "问题一问题二");
}

#[test]
fn test_strip_system_reminders_no_tag_unchanged() {
    assert_eq!(strip_system_reminders("普通文本"), "普通文本");
    assert_eq!(strip_system_reminders(""), "");
}

#[test]
fn test_strip_system_reminders_unclosed_keeps_rest() {
    // 防御路径：未闭合标签不损坏用户输入
    assert_eq!(
        strip_system_reminders("前缀<system-reminder>未闭合"),
        "前缀<system-reminder>未闭合"
    );
}

#[test]
fn test_strip_system_reminders_preserves_canonical_and_future_xml() {
    let canonical = "before<system-reminder version=\"1\">pasted</system-reminder>after";
    assert_eq!(strip_system_reminders(canonical), canonical);

    let future = "before<system-reminder version=\"2\">future</system-reminder>after";
    assert_eq!(strip_system_reminders(future), future);
}

#[test]
fn test_strip_system_reminders_standalone_block_becomes_empty() {
    assert_eq!(
        strip_system_reminders("<system-reminder>后台任务完成通知</system-reminder>"),
        "",
        "纯注入消息剥离后为空（候选过滤依据）"
    );
}

// ── Responses 原生历史载体 ────────────────────────────────────────────────────

/// 载荷含 provider 私有状态（reasoning 密文 + 来源 nonce/digest）。
fn native_history_payload() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "source": {"nonce": "NONCEHEX", "digest": "DIGESTHEX"},
        "items": [
            {"type": "reasoning", "id": "rs_1", "encrypted_content": "CIPHERTEXT"},
            {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
             "content": [{"type": "output_text", "text": "answer"}]}
        ],
    })
}

#[test]
fn test_responses_native_history_envelope_roundtrip_is_payload_faithful() {
    let payload = native_history_payload();
    let block = ContentBlock::responses_native_history(payload.clone());

    assert!(block.is_responses_native_history());
    assert_eq!(block.responses_native_history_payload(), Some(&payload));

    let json = serde_json::to_string(&block).expect("原生历史载体必须可序列化");
    assert!(
        json.contains("CIPHERTEXT"),
        "持久化载体必须保留载荷（脱敏只作用于可观测出口）"
    );
    let back: ContentBlock = serde_json::from_str(&json).expect("持久化载体必须可反序列化");
    assert_eq!(back, block);
    assert_eq!(back.responses_native_history_payload(), Some(&payload));
}

#[test]
fn test_responses_native_history_debug_redacts_private_state() {
    let block = ContentBlock::responses_native_history(native_history_payload());
    let debug = format!("{block:?}");

    assert!(!debug.contains("CIPHERTEXT"), "Debug 不得输出密文");
    assert!(!debug.contains("NONCEHEX"), "Debug 不得输出来源 nonce");
    assert!(!debug.contains("DIGESTHEX"), "Debug 不得输出来源 digest");
    assert!(
        debug.contains(RESPONSES_NATIVE_HISTORY_TAG),
        "Debug 仍应表明载体类型：{debug}"
    );
}

#[test]
fn test_observability_projection_drops_private_state_only() {
    let history = ContentBlock::responses_native_history(native_history_payload());
    let projected = history.redacted_for_observability();

    assert!(projected.is_responses_native_history());
    assert_ne!(projected, history);
    let json = serde_json::to_string(&projected).expect("投影可序列化");
    assert!(!json.contains("CIPHERTEXT"));
    assert!(!json.contains("NONCEHEX"));
    assert!(!json.contains("DIGESTHEX"));
    // 记录本身不被投影破坏：原 block 仍是事实源
    assert_eq!(
        history.responses_native_history_payload(),
        Some(&native_history_payload())
    );
}

#[test]
fn test_observability_projection_leaves_other_blocks_unchanged() {
    let text = ContentBlock::text("hello");
    assert_eq!(text.redacted_for_observability(), text);
    let unknown = ContentBlock::Unknown(serde_json::json!({"type": "other"}));
    assert_eq!(unknown.redacted_for_observability(), unknown);
}

#[test]
fn test_non_native_unknown_is_not_matched() {
    let other = ContentBlock::Unknown(serde_json::json!({"type": "other"}));
    assert!(!other.is_responses_native_history());
    assert_eq!(other.responses_native_history_payload(), None);
    // 缺少 history 载荷的载体不算原生历史：payload 访问器返回 None
    let empty = ContentBlock::Unknown(serde_json::json!({
        "type": RESPONSES_NATIVE_HISTORY_TAG,
    }));
    assert!(empty.is_responses_native_history());
    assert_eq!(empty.responses_native_history_payload(), None);
}

#[test]
fn test_message_content_observability_projection_keeps_visible_content() {
    let content = MessageContent::Blocks(vec![
        ContentBlock::reasoning("thought"),
        ContentBlock::text("answer"),
        ContentBlock::responses_native_history(native_history_payload()),
    ]);

    assert!(content.has_provider_native_history());
    let projected = content.redacted_for_observability();
    assert_eq!(projected.text_content(), "answer", "可见文本必须保留");
    assert!(projected.has_provider_native_history());
    let json = serde_json::to_string(&projected).expect("投影可序列化");
    assert!(!json.contains("CIPHERTEXT"));
    assert!(json.contains("answer"));
}

#[test]
fn test_message_content_without_native_history_is_unchanged() {
    let content = MessageContent::text("plain");
    assert!(!content.has_provider_native_history());
    assert_eq!(content.redacted_for_observability(), content);
}
