//! 派生收据与归档入口的 canonical 位置契约。
//!
//! 这些测试直接检查最终渲染数组的角色、顺序与配对：收据是派生历史数据，
//! 必须留在原步骤附近，不能越过后续真实用户消息。

use super::*;
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId, ToolCallRequest};
use peri_acp_types::context_archive::{build_catalog, ArchiveCatalog, ArchiveRoot};
use peri_acp_types::store::PersistedPayload;

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

fn assistant_blocks(id: u128, text: &str, calls: &[(&str, &str)]) -> BaseMessage {
    let mut blocks = vec![ContentBlock::text(text)];
    for (call_id, name) in calls {
        blocks.push(ContentBlock::tool_use(
            *call_id,
            *name,
            serde_json::json!({"path": "a.rs"}),
        ));
    }
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

fn catalog(
    group: &str,
    anchor: MessageId,
    payloads: &[&BaseMessage],
    receipt: &str,
) -> ArchiveCatalog {
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

fn directive(
    policy_version: u32,
    entries: Vec<ProjectionActionEntry>,
    catalog: Option<ArchiveCatalog>,
) -> MessageProjectionDirective {
    MessageProjectionDirective {
        policy_version,
        archive_catalog: catalog,
        entries,
        ..Default::default()
    }
}

fn omit(message: MessageId, anchor: MessageId, group: &str, call: &str) -> ProjectionActionEntry {
    ProjectionActionEntry {
        message_id: message,
        target: ProjectionTarget::ToolCall {
            tool_call_id: call.into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: anchor,
            group_id: group.into(),
            tool_call_ids: vec![call.into()],
        },
    }
}

fn root_for(last_anchor: MessageId, first_anchor: MessageId) -> ArchiveRoot {
    ArchiveRoot {
        source_thread_id: "session".into(),
        first_anchor,
        last_anchor,
        committed_tail: last_anchor,
    }
}

fn rendered(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
    caps: &ProviderCapabilities,
) -> Vec<BaseMessage> {
    render_llm_view(transcript, actions, caps).expect("projection must render")
}

fn receipt_index(messages: &[BaseMessage], marker: &str) -> usize {
    messages
        .iter()
        .position(|message| {
            matches!(message, BaseMessage::Ai { .. }) && message.content().contains(marker)
        })
        .unwrap_or_else(|| panic!("missing receipt {marker}"))
}

fn text_index(messages: &[BaseMessage], text: &str) -> usize {
    messages
        .iter()
        .position(|message| message.content().contains(text))
        .unwrap_or_else(|| panic!("missing message {text}"))
}

#[test]
fn receipt_stays_after_old_step_and_before_new_user_task() {
    let mut transcript = MessageTranscript::new();
    let call = assistant_blocks(0x10, "old step", &[("call-a", "Read")]);
    let result = tool_text(0x11, "call-a", "old result ".repeat(100));
    let task = human_text(0x12, "NEW TASK after the old step");
    transcript.append(call.clone());
    transcript.append(result.clone());
    transcript.append(task.clone());

    let catalog = catalog("group-a", result.id(), &[&call, &result], "old receipt");
    transcript.set_flags_projection(
        result.id(),
        directive(
            3,
            vec![omit(result.id(), result.id(), &catalog.group_id, "call-a")],
            Some(catalog.clone()),
        ),
    );
    let actions = vec![
        omit(call.id(), result.id(), &catalog.group_id, "call-a"),
        omit(result.id(), result.id(), &catalog.group_id, "call-a"),
    ];

    let messages = rendered(&transcript, &actions, &ProviderCapabilities::openai());
    // 真实用户内容保持相对顺序；收据只跟随它自己的旧步骤。
    assert!(text_index(&messages, "old step") < receipt_index(&messages, "old receipt"));
    assert!(
        receipt_index(&messages, "old receipt") < text_index(&messages, "NEW TASK"),
        "收据不能越过新的用户任务成为最新历史"
    );
}

#[test]
fn partial_batch_receipt_follows_the_retained_sibling_result() {
    let mut transcript = MessageTranscript::new();
    let call = assistant_blocks(0x10, "batch", &[("call-a", "Read"), ("call-b", "Read")]);
    let result_a = tool_text(0x11, "call-a", "A result");
    let result_b = tool_text(0x12, "call-b", "B result");
    let task = human_text(0x13, "NEW TASK after the batch");
    transcript.append(call.clone());
    transcript.append(result_a.clone());
    transcript.append(result_b.clone());
    transcript.append(task);

    let catalog = catalog("group-a", call.id(), &[&call, &result_a], "partial receipt");
    transcript.set_flags_projection(
        call.id(),
        directive(
            3,
            vec![omit(call.id(), call.id(), &catalog.group_id, "call-a")],
            Some(catalog.clone()),
        ),
    );
    let actions = vec![
        omit(call.id(), call.id(), &catalog.group_id, "call-a"),
        omit(result_a.id(), call.id(), &catalog.group_id, "call-a"),
    ];

    let messages = rendered(&transcript, &actions, &ProviderCapabilities::openai());
    assert_eq!(
        text_index(&messages, "B result"),
        1,
        "未归档的 B 结果保持原位置"
    );
    assert!(
        receipt_index(&messages, "partial receipt") > text_index(&messages, "B result"),
        "收据不得插入 B 调用与其结果之间"
    );
    assert!(receipt_index(&messages, "partial receipt") < text_index(&messages, "NEW TASK"));
}

#[test]
fn inline_result_receipt_follows_the_containing_user_message() {
    let mut transcript = MessageTranscript::new();
    let call = assistant_blocks(0x10, "inline step", &[("call-a", "Read")]);
    let mixed = human_blocks(
        0x11,
        vec![
            ContentBlock::tool_result("call-a", vec![ContentBlock::text("A result")], false),
            ContentBlock::text("user constraint"),
        ],
    );
    let task = human_text(0x12, "NEW TASK after inline result");
    transcript.append(call.clone());
    transcript.append(mixed.clone());
    transcript.append(task);

    let catalog = catalog("group-a", mixed.id(), &[&call, &mixed], "inline receipt");
    transcript.set_flags_projection(
        mixed.id(),
        directive(
            3,
            vec![omit(mixed.id(), mixed.id(), &catalog.group_id, "call-a")],
            Some(catalog.clone()),
        ),
    );
    let actions = vec![
        omit(call.id(), mixed.id(), &catalog.group_id, "call-a"),
        omit(mixed.id(), mixed.id(), &catalog.group_id, "call-a"),
    ];

    let messages = rendered(&transcript, &actions, &ProviderCapabilities::openai());
    let constraint = text_index(&messages, "user constraint");
    assert_eq!(constraint, 1, "同一消息内的真实用户内容保持原位");
    assert_eq!(receipt_index(&messages, "inline receipt"), 2);
    assert!(receipt_index(&messages, "inline receipt") < text_index(&messages, "NEW TASK"));
}

#[test]
fn receipt_budget_overflow_keeps_the_root_at_the_same_step() {
    let mut transcript = MessageTranscript::new();
    let call = assistant_blocks(0x10, "step", &[("call-a", "Read")]);
    let result = tool_text(0x11, "call-a", "result");
    let task = human_text(0x12, "NEW TASK after step");
    transcript.append(call.clone());
    transcript.append(result.clone());
    transcript.append(task);

    let catalog = catalog("group-a", result.id(), &[&call, &result], "receipt");
    let root = root_for(result.id(), call.id());
    transcript.set_flags_projection(
        result.id(),
        MessageProjectionDirective {
            policy_version: 3,
            archive_catalog: Some(catalog.clone()),
            entries: vec![omit(result.id(), result.id(), &catalog.group_id, "call-a")],
            archive_roots: vec![root],
            archive_receipt_budget_tokens: Some(1),
            ..Default::default()
        },
    );
    let actions = vec![
        omit(call.id(), result.id(), &catalog.group_id, "call-a"),
        omit(result.id(), result.id(), &catalog.group_id, "call-a"),
    ];

    let messages = rendered(&transcript, &actions, &ProviderCapabilities::openai());
    assert!(
        !messages
            .iter()
            .any(|message| message.content().contains("Historical receipt")),
        "收据超预算时只保留范围入口"
    );
    let root = text_index(&messages, "Historical data archive");
    assert!(
        root < text_index(&messages, "NEW TASK"),
        "root 不得落回请求尾部"
    );
}

#[test]
fn root_after_full_follows_the_summary_that_carries_it() {
    let mut transcript = MessageTranscript::new();
    let call = assistant_blocks(0x10, "old step", &[("call-a", "Read")]);
    let result = tool_text(0x11, "call-a", "old result");
    let summary = BaseMessage::Ai {
        id: message_id(0x12),
        tool_calls: Vec::new(),
        content: MessageContent::text("[Summary of earlier work]"),
    };
    let task = human_text(0x13, "NEW TASK after summary");
    transcript.append(call.clone());
    transcript.append(result.clone());
    transcript.append(summary.clone());
    transcript.append(task);

    // Full 已排除旧成员：root 必须锚到当前持有 directive 的摘要，而不是已 shrink 的旧锚点。
    transcript.set_excluded(call.id(), true);
    transcript.set_excluded(result.id(), true);
    let root = root_for(result.id(), call.id());
    transcript.set_flags_projection(
        summary.id(),
        MessageProjectionDirective {
            policy_version: 3,
            archive_roots: vec![root],
            ..Default::default()
        },
    );

    let messages = rendered(&transcript, &[], &ProviderCapabilities::openai());
    let summary_position = text_index(&messages, "[Summary of earlier work]");
    let root_position = text_index(&messages, "Historical data archive");
    assert_eq!(root_position, summary_position + 1);
    assert!(root_position < text_index(&messages, "NEW TASK"));
}

#[test]
fn legacy_v2_and_v3_directives_use_the_same_position_rule() {
    for policy_version in [2, 3] {
        let mut transcript = MessageTranscript::new();
        let call = assistant_blocks(0x10, "step", &[("call-a", "Read")]);
        let result = tool_text(0x11, "call-a", "result");
        let task = human_text(0x12, "NEW TASK after step");
        transcript.append(call.clone());
        transcript.append(result.clone());
        transcript.append(task);

        let catalog = catalog("group-a", result.id(), &[&call, &result], "legacy receipt");
        transcript.set_flags_projection(
            result.id(),
            directive(
                policy_version,
                vec![omit(result.id(), result.id(), &catalog.group_id, "call-a")],
                Some(catalog.clone()),
            ),
        );
        let actions = vec![
            omit(call.id(), result.id(), &catalog.group_id, "call-a"),
            omit(result.id(), result.id(), &catalog.group_id, "call-a"),
        ];
        let messages = rendered(&transcript, &actions, &ProviderCapabilities::openai());
        // 步骤结果被整组省略，但收据仍按原 canonical 位置追加（在第 2 条之前）。
        assert_eq!(
            receipt_index(&messages, "legacy receipt"),
            1,
            "policy {policy_version}: 收据紧跟步骤结果的位置"
        );
        assert!(receipt_index(&messages, "legacy receipt") < text_index(&messages, "NEW TASK"));
    }
}

#[test]
fn cold_restore_and_provider_switch_keep_receipt_positions() {
    let mut transcript = MessageTranscript::new();
    let call = assistant_blocks(0x10, "step", &[("call-a", "Read")]);
    let result = tool_text(0x11, "call-a", "result");
    let task = human_text(0x12, "NEW TASK after step");
    transcript.append(call.clone());
    transcript.append(result.clone());
    transcript.append(task);

    let catalog = catalog(
        "group-a",
        result.id(),
        &[&call, &result],
        "restored receipt",
    );
    transcript.set_flags_projection(
        result.id(),
        directive(
            3,
            vec![omit(result.id(), result.id(), &catalog.group_id, "call-a")],
            Some(catalog.clone()),
        ),
    );
    transcript.set_flags_projection(
        call.id(),
        directive(
            3,
            vec![omit(call.id(), result.id(), &catalog.group_id, "call-a")],
            None,
        ),
    );
    let actions = vec![
        omit(call.id(), result.id(), &catalog.group_id, "call-a"),
        omit(result.id(), result.id(), &catalog.group_id, "call-a"),
    ];
    let openai = rendered(&transcript, &actions, &ProviderCapabilities::openai());
    let anthropic = rendered(&transcript, &actions, &ProviderCapabilities::anthropic());
    assert_eq!(
        receipt_index(&openai, "restored receipt"),
        receipt_index(&anthropic, "restored receipt"),
        "切换 provider 后位置规则一致"
    );

    // 冷恢复：只保留 canonical payload 与持久化 flags，重新渲染同一位置。
    let payloads = transcript
        .entries()
        .iter()
        .map(|entry| entry.clone().into_payload())
        .collect::<Vec<_>>();
    let flags = transcript
        .entries()
        .iter()
        .map(|entry| (entry.id(), transcript.flags(entry.id())))
        .collect::<std::collections::HashMap<_, _>>();
    let mut restored = MessageTranscript::new().with_own_payloads(payloads);
    restored.set_flags_batch(flags);
    let messages =
        super::projection::render_persisted_llm_view(&restored, &ProviderCapabilities::openai())
            .expect("cold restore must render the committed projection");
    assert!(receipt_index(&messages, "restored receipt") < text_index(&messages, "NEW TASK"));
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.content().contains("call-a"))
            .count(),
        0,
        "整组省略后的调用与结果不再出现在最终请求中"
    );
}
