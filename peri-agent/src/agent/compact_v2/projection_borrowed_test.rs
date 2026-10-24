//! Borrowed projection golden tests.
//!
//! The serialized message list is the contract: fixed `MessageId`s and a fixed
//! action set make the bytes reproducible. The goldens are replayed against the
//! pre-refactor renderer at baseline `e598d178`; passing both renderers keeps the
//! borrowed implementation byte-identical.

use super::*;
use crate::messages::ToolCallRequest;
use peri_acp_types::compact::CONTINUATION_HINT;
use peri_acp_types::context_archive::build_catalog;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

fn message_id(value: u128) -> MessageId {
    MessageId::from(uuid::Uuid::from_u128(value))
}

fn human_text(id: u128, text: impl Into<String>) -> BaseMessage {
    BaseMessage::Human {
        id: message_id(id),
        content: MessageContent::text(text),
    }
}

fn human_blocks(id: u128, blocks: Vec<ContentBlock>) -> BaseMessage {
    BaseMessage::Human {
        id: message_id(id),
        content: MessageContent::Blocks(blocks),
    }
}

fn ai_blocks(id: u128, blocks: Vec<ContentBlock>, calls: &[(&str, &str)]) -> BaseMessage {
    BaseMessage::Ai {
        id: message_id(id),
        content: MessageContent::Blocks(blocks),
        tool_calls: calls
            .iter()
            .map(|(call_id, name)| {
                ToolCallRequest::new(*call_id, *name, serde_json::json!({"path": "a.rs"}))
            })
            .collect(),
    }
}

fn tool_text(id: u128, call_id: &str, text: impl Into<String>) -> BaseMessage {
    BaseMessage::Tool {
        id: message_id(id),
        tool_call_id: call_id.into(),
        content: MessageContent::text(text),
        is_error: false,
        execution: None,
        subagent_failure: None,
    }
}

fn directive(
    policy_version: u32,
    entries: Vec<ProjectionActionEntry>,
    catalog: Option<peri_acp_types::context_archive::ArchiveCatalog>,
) -> MessageProjectionDirective {
    MessageProjectionDirective {
        policy_version,
        archive_catalog: catalog,
        entries,
        ..Default::default()
    }
}

fn catalog(
    group: &str,
    anchor: MessageId,
    payloads: &[&BaseMessage],
    receipt: &str,
) -> peri_acp_types::context_archive::ArchiveCatalog {
    let payloads = payloads
        .iter()
        .map(|message| PersistedPayload::Message((*message).clone()))
        .collect::<Vec<_>>();
    build_catalog(
        "session".into(),
        group.into(),
        anchor,
        &payloads,
        receipt.into(),
    )
    .unwrap()
}

fn rendered(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
    caps: &ProviderCapabilities,
) -> String {
    let projected = render_llm_view(transcript, actions, caps).expect("projection must render");
    serde_json::to_string_pretty(&projected).unwrap()
}

fn assert_golden(actual: &str, golden: &str) {
    assert_eq!(
        actual, golden,
        "\n--- actual serialized projection ---\n{actual}\n--- end actual ---"
    );
}

#[test]
fn text_blocks_raw_and_reminders_keep_their_projection_bytes() {
    let mut transcript = MessageTranscript::new();
    let text = human_text(0x01, "用户请求：检查 a.rs 🙂\n保留换行");
    let blocks = human_blocks(
        0x02,
        vec![
            ContentBlock::text("block 正文"),
            ContentBlock::image_base64("image/png", "AAAA"),
            ContentBlock::Document {
                source: crate::messages::DocumentSource::Text {
                    text: "doc 正文".into(),
                },
                title: Some("说明".into()),
            },
        ],
    );
    let raw = BaseMessage::Human {
        id: message_id(0x03),
        content: MessageContent::Raw(vec![
            serde_json::json!({"type": "text", "text": "raw 正文"}),
            serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "BBBB"}}),
        ]),
    };
    let legacy = human_text(0x04, format!("{CONTINUATION_HINT}\nlegacy 正文 🙂"));
    transcript.append(text);
    transcript.append(blocks);
    transcript.append(raw);
    transcript.append(legacy);
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("projection_borrowed_test".into()),
            kind: "done".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "canonical reminder 中文".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    assert!(transcript.mirror_committed_reminder(message_id(0x05), reminder));

    let actions = vec![
        ProjectionActionEntry {
            message_id: message_id(0x01),
            target: ProjectionTarget::ContentBlock { index: 0 },
            action: ProjectionAction::CompactText { max_chars: 5 },
        },
        ProjectionActionEntry {
            message_id: message_id(0x02),
            target: ProjectionTarget::ContentBlock { index: 1 },
            action: ProjectionAction::ReplaceMedia {
                placeholder: "图像占位".into(),
            },
        },
        ProjectionActionEntry {
            message_id: message_id(0x02),
            target: ProjectionTarget::ContentBlock { index: 2 },
            action: ProjectionAction::ReplaceMedia {
                placeholder: "文档占位".into(),
            },
        },
        ProjectionActionEntry {
            message_id: message_id(0x03),
            target: ProjectionTarget::ContentBlock { index: 0 },
            action: ProjectionAction::CompactText { max_chars: 2 },
        },
    ];

    assert_golden(
        &rendered(&transcript, &actions, &ProviderCapabilities::openai()),
        GOLDEN_TEXT_BLOCKS_RAW_REMINDERS,
    );
}

#[test]
fn v2_legacy_tool_results_keep_their_projection_bytes() {
    let mut transcript = MessageTranscript::new();
    let call = ai_blocks(
        0x10,
        vec![ContentBlock::text("调用")],
        &[("call-legacy", "Read")],
    );
    let unicode = format!("头部🙂{}尾部", "中间".repeat(50));
    let text_result = tool_text(0x11, "call-legacy", unicode);
    let block_result = BaseMessage::Tool {
        id: message_id(0x12),
        tool_call_id: "call-legacy-2".into(),
        content: MessageContent::Blocks(vec![
            ContentBlock::text("A".repeat(60)),
            ContentBlock::text("B".repeat(60)),
        ]),
        is_error: false,
        execution: None,
        subagent_failure: None,
    };
    transcript.append(call);
    transcript.append(text_result.clone());
    transcript.append(block_result.clone());
    let action_for = |message: &BaseMessage| ProjectionActionEntry {
        message_id: message.id(),
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 4,
            keep_tail: 3,
            preserve_recovery_handle: false,
        },
    };
    let text_action = action_for(&text_result);
    let block_action = action_for(&block_result);
    transcript.set_flags_projection(
        text_result.id(),
        directive(2, vec![text_action.clone()], None),
    );
    transcript.set_flags_projection(
        block_result.id(),
        directive(2, vec![block_action.clone()], None),
    );

    assert_golden(
        &rendered(
            &transcript,
            &[text_action, block_action],
            &ProviderCapabilities::openai(),
        ),
        GOLDEN_LEGACY_V2,
    );
}

#[test]
fn v3_preview_receipt_omission_and_references_keep_their_projection_bytes() {
    let mut transcript = MessageTranscript::new();

    let call = ai_blocks(
        0x20,
        vec![
            ContentBlock::text("计划"),
            ContentBlock::tool_use("call-a", "Read", serde_json::json!({"path": "a.rs"})),
            ContentBlock::reasoning_with_signature("思考", "sig-1"),
        ],
        &[("call-a", "Read")],
    );
    let mixed = human_blocks(
        0x21,
        vec![
            ContentBlock::tool_result("call-a", vec![ContentBlock::text("A 结果")], false),
            ContentBlock::text("用户约束"),
        ],
    );
    let receipt_target = human_blocks(
        0x22,
        vec![
            ContentBlock::text("前缀"),
            ContentBlock::tool_result("call-b", vec![ContentBlock::text("B 结果")], false),
        ],
    );
    let preview = tool_text(0x23, "call-preview", format!("预览{}", "长".repeat(100)));
    let preserve = tool_text(0x24, "call-preserve", format!("保留{}", "块".repeat(40)));
    let omitted_call = ai_blocks(
        0x25,
        vec![ContentBlock::text("调用 c")],
        &[("call-c", "Read")],
    );
    let omitted_result = tool_text(0x26, "call-c", format!("整条省略{}", "省".repeat(80)));

    transcript.append(call.clone());
    transcript.append(mixed.clone());
    transcript.append(receipt_target.clone());
    transcript.append(preview.clone());
    transcript.append(preserve.clone());
    transcript.append(omitted_call.clone());
    transcript.append(omitted_result.clone());

    let omission_catalog = catalog("omit-a", mixed.id(), &[&call, &mixed], "归档 A 交换");
    let receipt_catalog = catalog(
        "receipt-b",
        receipt_target.id(),
        &[&receipt_target],
        "收据 B",
    );
    let preview_catalog = catalog("preview", preview.id(), &[&preview], "预览原文");
    let preserve_catalog = catalog("preserve", preserve.id(), &[&preserve], "保留原文");
    let omitted_catalog = catalog(
        "omit-c",
        omitted_call.id(),
        &[&omitted_call, &omitted_result],
        "归档 C 交换",
    );

    let omit_a = ProjectionActionEntry {
        message_id: call.id(),
        target: ProjectionTarget::ToolCall {
            tool_call_id: "call-a".into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: mixed.id(),
            group_id: omission_catalog.group_id.clone(),
            tool_call_ids: vec!["call-a".into()],
        },
    };
    let omit_a_result = ProjectionActionEntry {
        message_id: mixed.id(),
        target: ProjectionTarget::ToolCall {
            tool_call_id: "call-a".into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: mixed.id(),
            group_id: omission_catalog.group_id.clone(),
            tool_call_ids: vec!["call-a".into()],
        },
    };
    let receipt_b = ProjectionActionEntry {
        message_id: receipt_target.id(),
        target: ProjectionTarget::ContentBlock { index: 1 },
        action: ProjectionAction::ResultReceipt {
            anchor_message_id: receipt_target.id(),
            group_id: receipt_catalog.group_id.clone(),
        },
    };
    let preview_action = ProjectionActionEntry {
        message_id: preview.id(),
        target: ProjectionTarget::Message,
        action: ProjectionAction::PreviewArchiveRead {
            archive_ref: preview_catalog.reference().unwrap(),
            keep_head: 3,
            keep_tail: 3,
        },
    };
    let preserve_action = ProjectionActionEntry {
        message_id: preserve.id(),
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 2,
            keep_tail: 2,
            preserve_recovery_handle: true,
        },
    };
    let omit_c = ProjectionActionEntry {
        message_id: omitted_call.id(),
        target: ProjectionTarget::ToolCall {
            tool_call_id: "call-c".into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: omitted_call.id(),
            group_id: omitted_catalog.group_id.clone(),
            tool_call_ids: vec!["call-c".into()],
        },
    };
    let omit_c_result = ProjectionActionEntry {
        message_id: omitted_result.id(),
        target: ProjectionTarget::ToolCall {
            tool_call_id: "call-c".into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: omitted_call.id(),
            group_id: omitted_catalog.group_id.clone(),
            tool_call_ids: vec!["call-c".into()],
        },
    };

    transcript.set_flags_projection(
        mixed.id(),
        directive(3, vec![omit_a_result.clone()], Some(omission_catalog)),
    );
    transcript.set_flags_projection(
        receipt_target.id(),
        directive(3, vec![receipt_b.clone()], Some(receipt_catalog)),
    );
    transcript.set_flags_projection(
        preview.id(),
        directive(3, vec![preview_action.clone()], Some(preview_catalog)),
    );
    transcript.set_flags_projection(
        preserve.id(),
        directive(3, vec![preserve_action.clone()], Some(preserve_catalog)),
    );
    transcript.set_flags_projection(
        omitted_call.id(),
        directive(3, vec![omit_c.clone()], Some(omitted_catalog)),
    );

    let actions = vec![
        omit_a,
        omit_a_result,
        receipt_b,
        preview_action,
        preserve_action,
        omit_c,
        omit_c_result,
    ];

    assert_golden(
        &rendered(&transcript, &actions, &ProviderCapabilities::openai()),
        GOLDEN_V3_ARCHIVE,
    );
}

#[test]
fn error_tool_results_keep_their_projection_bytes() {
    let mut transcript = MessageTranscript::new();
    let call = ai_blocks(
        0x30,
        vec![ContentBlock::text("失败调用")],
        &[("call-error", "Bash")],
    );
    let error_result = BaseMessage::Tool {
        id: message_id(0x31),
        tool_call_id: "call-error".into(),
        content: MessageContent::text(format!("头部🙂{}尾部", "错误".repeat(40))),
        is_error: true,
        execution: None,
        subagent_failure: None,
    };
    transcript.append(call);
    transcript.append(error_result.clone());
    let action = ProjectionActionEntry {
        message_id: error_result.id(),
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 4,
            keep_tail: 3,
            preserve_recovery_handle: false,
        },
    };
    transcript.set_flags_projection(error_result.id(), directive(3, vec![action.clone()], None));

    assert_golden(
        &rendered(&transcript, &[action], &ProviderCapabilities::openai()),
        GOLDEN_ERROR_TOOL_RESULT,
    );
}

/// Malformed raw blocks deserialize as `Unknown` in the baseline renderer. The
/// borrowed renderer must keep those bytes unchanged, ignore per-block actions on
/// raw content, and only truncate ordinary text messages.
#[test]
fn malformed_raw_blocks_keep_bytes_and_degrade_projection() {
    let mut transcript = MessageTranscript::new();
    let raw_human = BaseMessage::Human {
        id: message_id(0x30),
        content: MessageContent::Raw(vec![
            serde_json::json!({"type": "text"}),
            serde_json::json!({"type": "tool_result", "content": []}),
            serde_json::json!({"type": "tool_use", "name": "Read"}),
            serde_json::json!({"type": "reasoning", "signature": "sig"}),
            serde_json::json!({"type": "image", "source": {"type": "base64"}}),
            serde_json::json!({"type": "custom_unknown", "payload": 1}),
            serde_json::json!({"type": "text", "text": "幸存 raw 🙂"}),
        ]),
    };
    let raw_tool = BaseMessage::Tool {
        id: message_id(0x31),
        tool_call_id: "call-raw".into(),
        content: MessageContent::Raw(vec![
            serde_json::json!({"type": "text"}),
            serde_json::json!({"type": "image", "source": {}}),
        ]),
        is_error: false,
        execution: None,
        subagent_failure: None,
    };
    let text = human_text(0x32, "正常文本需要压缩");
    transcript.append(raw_human);
    transcript.append(raw_tool);
    transcript.append(text);

    let actions = vec![
        ProjectionActionEntry {
            message_id: message_id(0x30),
            target: ProjectionTarget::ContentBlock { index: 0 },
            action: ProjectionAction::CompactText { max_chars: 1 },
        },
        ProjectionActionEntry {
            message_id: message_id(0x31),
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 1,
                keep_tail: 1,
                preserve_recovery_handle: false,
            },
        },
        ProjectionActionEntry {
            message_id: message_id(0x32),
            target: ProjectionTarget::ContentBlock { index: 0 },
            action: ProjectionAction::CompactText { max_chars: 3 },
        },
    ];

    assert_golden(
        &rendered(&transcript, &actions, &ProviderCapabilities::openai()),
        GOLDEN_MALFORMED_RAW,
    );
}

/// A raw `tool_result` that fails the shared deserializer is `Unknown`, so
/// `safe_action` must reject it instead of projecting its content.
#[test]
fn malformed_raw_projection_directives_are_rejected() {
    let mut transcript = MessageTranscript::new();
    let malformed = BaseMessage::Human {
        id: message_id(0x33),
        content: MessageContent::Raw(vec![
            serde_json::json!({"type": "tool_result", "content": []}),
        ]),
    };
    let id = malformed.id();
    transcript.append(malformed);
    let action = ProjectionActionEntry {
        message_id: id,
        target: ProjectionTarget::ContentBlock { index: 0 },
        action: ProjectionAction::CompactToolResult {
            keep_head: 1,
            keep_tail: 1,
            preserve_recovery_handle: false,
        },
    };
    transcript.set_flags_projection(id, directive(3, vec![action], None));

    let error = render_persisted_llm_view(&transcript, &ProviderCapabilities::openai())
        .expect_err("malformed raw tool_result must not be a safe projection target");
    assert!(
        format!("{error}").contains("invalid persisted context projection"),
        "unexpected error: {error}"
    );
}

const GOLDEN_MALFORMED_RAW: &str = r#"[
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000030",
    "content": [
      {
        "type": "text"
      },
      {
        "content": [],
        "type": "tool_result"
      },
      {
        "name": "Read",
        "type": "tool_use"
      },
      {
        "signature": "sig",
        "type": "reasoning"
      },
      {
        "source": {
          "type": "base64"
        },
        "type": "image"
      },
      {
        "payload": 1,
        "type": "custom_unknown"
      },
      {
        "text": "幸存 raw 🙂",
        "type": "text"
      }
    ]
  },
  {
    "role": "tool",
    "id": "00000000-0000-0000-0000-000000000031",
    "tool_call_id": "call-raw",
    "content": [
      {
        "type": "text"
      },
      {
        "source": {},
        "type": "image"
      }
    ],
    "is_error": false
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000032",
    "content": "正常文\n[内容已压缩]"
  }
]"#;

const GOLDEN_TEXT_BLOCKS_RAW_REMINDERS: &str = r#"[
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000001",
    "content": "用户请求：\n[内容已压缩]"
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000002",
    "content": [
      {
        "type": "text",
        "text": "block 正文"
      },
      {
        "type": "text",
        "text": "[图片已压缩: 图像占位]"
      },
      {
        "type": "text",
        "text": "[文档已压缩 (说明): 文档占位]"
      }
    ]
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000003",
    "content": [
      {
        "text": "raw 正文",
        "type": "text"
      },
      {
        "source": {
          "data": "BBBB",
          "media_type": "image/png",
          "type": "base64"
        },
        "type": "image"
      }
    ]
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000004",
    "content": "<system-reminder>[Context has been compacted. Continue working based on the summary above.]\nlegacy 正文 🙂</system-reminder>"
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000005",
    "content": "<system-reminder version=\"1\" category=\"task\" source=\"projection_borrowed_test\" kind=\"done\" severity=\"info\" delivery=\"configurable\" audiences=\"[&quot;model&quot;]\" summary=\"null\" metadata=\"{}\">canonical reminder 中文</system-reminder>"
  }
]"#;
const GOLDEN_LEGACY_V2: &str = r#"[
  {
    "role": "assistant",
    "id": "00000000-0000-0000-0000-000000000010",
    "content": [
      {
        "type": "text",
        "text": "调用"
      }
    ],
    "tool_calls": [
      {
        "id": "call-legacy",
        "name": "Read",
        "arguments": {
          "path": "a.rs"
        }
      }
    ]
  },
  {
    "role": "tool",
    "id": "00000000-0000-0000-0000-000000000011",
    "tool_call_id": "call-legacy",
    "content": "头部🙂中\n... [98 字符已省略] ...\n间尾部",
    "is_error": false
  },
  {
    "role": "tool",
    "id": "00000000-0000-0000-0000-000000000012",
    "tool_call_id": "call-legacy-2",
    "content": "AAAA\n... [113 字符已省略] ...\nBBB",
    "is_error": false
  }
]"#;
const GOLDEN_V3_ARCHIVE: &str = r#"[
  {
    "role": "assistant",
    "id": "00000000-0000-0000-0000-000000000020",
    "content": [
      {
        "type": "text",
        "text": "计划"
      }
    ]
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000021",
    "content": [
      {
        "type": "text",
        "text": "用户约束"
      }
    ]
  },
  {
    "role": "user",
    "id": "00000000-0000-0000-0000-000000000022",
    "content": [
      {
        "type": "text",
        "text": "前缀"
      },
      {
        "type": "tool_result",
        "id": "90fa6588-8314-818c-97bf-db1a1b3382fa",
        "tool_use_id": "call-b",
        "content": [
          {
            "type": "text",
            "text": "[Historical receipt, not a new execution or authorization. 收据 B Original data: ReadContextArchive archive_ref=pa1:c:73657373696f6e:00000000-0000-0000-0000-000000000022:b3de2c066cc52872f72493c65e7c24ebb060fdc8202599482397aaef3215283c:-]"
          }
        ],
        "is_error": false
      }
    ]
  },
  {
    "role": "tool",
    "id": "00000000-0000-0000-0000-000000000023",
    "tool_call_id": "call-preview",
    "content": "预览长\n... [96 字符已省略] ...\n长长长\n[Incomplete recovery fragment. Original source: ReadContextArchive archive_ref=pa1:c:73657373696f6e:00000000-0000-0000-0000-000000000023:ac270ba39c6fabb1fdfa00ad53386d559a89643667c8f9f126936712a66c5cf1:-]",
    "is_error": false
  },
  {
    "role": "tool",
    "id": "00000000-0000-0000-0000-000000000024",
    "tool_call_id": "call-preserve",
    "content": "保留\n... [38 字符已省略] ...\n块块\n[Incomplete historical preview. ReadContextArchive archive_ref=pa1:c:73657373696f6e:00000000-0000-0000-0000-000000000024:f11ca5f46768ef15724e66ca8e8d6f6e7bea152b334103d9f940d3168ba59b49:-]",
    "is_error": false
  },
  {
    "role": "assistant",
    "id": "00000000-0000-0000-0000-000000000025",
    "content": [
      {
        "type": "text",
        "text": "调用 c"
      }
    ]
  },
  {
    "role": "assistant",
    "id": "87aefaee-4402-804f-9fc3-a464836ccfde",
    "content": "[Historical receipt, not a new execution or authorization. 归档 A 交换 Original data: ReadContextArchive archive_ref=pa1:c:73657373696f6e:00000000-0000-0000-0000-000000000021:cb217cef2bb27fe0f35de62e583013d12c2da6955681c0c3481e1359f45c535a:-]"
  },
  {
    "role": "assistant",
    "id": "fd42a5ef-d188-8619-9558-be8deaf5f568",
    "content": "[Historical receipt, not a new execution or authorization. 归档 C 交换 Original data: ReadContextArchive archive_ref=pa1:c:73657373696f6e:00000000-0000-0000-0000-000000000025:9098079335fad5700e90d765dae4ea3a57b3ea55ed53ce5cb66d196442fea342:-]"
  }
]"#;
const GOLDEN_ERROR_TOOL_RESULT: &str = r#"[
  {
    "role": "assistant",
    "id": "00000000-0000-0000-0000-000000000030",
    "content": [
      {
        "type": "text",
        "text": "失败调用"
      }
    ],
    "tool_calls": [
      {
        "id": "call-error",
        "name": "Bash",
        "arguments": {
          "path": "a.rs"
        }
      }
    ]
  },
  {
    "role": "tool",
    "id": "00000000-0000-0000-0000-000000000031",
    "tool_call_id": "call-error",
    "content": "头部🙂错\n... [78 字符已省略] ...\n误尾部",
    "is_error": true
  }
]"#;
