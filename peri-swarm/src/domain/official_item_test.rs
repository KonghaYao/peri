use serde_json::json;

use super::*;
use crate::domain::ItemSource;

fn row(seq: u64, kind: ItemKind, payload: Value) -> ItemView {
    ItemView {
        id: "itim_example".into(),
        seq,
        item_id: "itim_example".into(),
        kind,
        source: ItemSource::root(),
        payload,
        created_at: "2026-01-01T00:00:00Z".into(),
        truncated: false,
    }
}

fn project(rows: &[ItemView], status: Option<TurnStatus>) -> Option<Value> {
    let group = ItemGroup::new(rows.iter().collect()).expect("nonempty group");
    let item = map_group(
        &group,
        Some(TurnContext {
            id: "turn_opener",
            status,
        }),
    )?;
    let wire = serde_json::to_value(&item).unwrap();
    assert_eq!(
        serde_json::from_value::<OfficialSessionItem>(wire.clone()).unwrap(),
        item
    );
    Some(wire)
}

#[test]
fn user_message_has_the_official_fields_even_with_empty_text() {
    let rows = [
        row(1, ItemKind::Message, json!({"role":"user","text":"old"})),
        row(2, ItemKind::Message, json!({"role":"user","text":""})),
    ];
    assert_eq!(
        project(&rows, None),
        Some(json!({
            "type":"message", "id":"itim_example", "turn_id":"turn_opener", "role":"user",
            "status":"completed", "phase":null, "content":[{"type":"input_text","text":""}]
        }))
    );
}

#[test]
fn assistant_appends_rows_after_the_first_complete_frame() {
    // 真实写入形状：完成帧是 flush 时的累计文本，flush 之后同一 itemId 仍可能
    // 追加迟到 delta，或由下一次 flush 写一条只含新增文本的完成帧。
    let complete = row(
        2,
        ItemKind::Message,
        json!({"role":"assistant","text":"hel","complete":true,"phase":"commentary"}),
    );
    let late_delta = row(
        3,
        ItemKind::Message,
        json!({"role":"assistant","text":"lo","delta":true}),
    );
    let expected = json!({
        "type":"message", "id":"itim_example", "turn_id":"turn_opener", "role":"assistant",
        "status":"completed", "phase":"commentary",
        "content":[{"type":"output_text","text":"hello"}]
    });
    assert_eq!(
        project(
            &[late_delta.clone(), complete.clone()],
            Some(TurnStatus::Cancelled)
        ),
        Some(expected)
    );
    // 第二个完成帧只带迟到文本：同样按 seq 追加。
    let late_complete = row(
        4,
        ItemKind::Message,
        json!({"role":"assistant","text":"!","complete":true}),
    );
    assert_eq!(
        project(&[complete.clone(), late_delta, late_complete], None).unwrap()["content"],
        json!([{"type":"output_text","text":"hello!"}])
    );
    // 迟到帧被截断（正文不可得）：跳过它，不把完成帧文本判为不可读。
    let mut truncated = row(5, ItemKind::Message, json!({"role":"assistant","text":"?"}));
    truncated.truncated = true;
    assert_eq!(
        project(&[complete.clone(), truncated], None).unwrap()["content"],
        json!([{"type":"output_text","text":"hel"}])
    );
    // 缺少合法 phase 时用 final_answer。
    let mut no_phase = complete;
    no_phase.payload.as_object_mut().unwrap().remove("phase");
    assert_eq!(project(&[no_phase], None).unwrap()["phase"], "final_answer");
}

#[test]
fn unfinished_assistant_joins_legacy_deltas_or_reads_one_row() {
    let first = row(
        1,
        ItemKind::Message,
        json!({"role":"assistant","text":"hel","delta":true}),
    );
    let last = row(
        2,
        ItemKind::Message,
        json!({"role":"assistant","text":"lo","delta":true,"phase":"commentary"}),
    );
    let rows = [last, first];
    for (status, expected) in [
        (None, "in_progress"),
        (Some(TurnStatus::Queued), "in_progress"),
        (Some(TurnStatus::InProgress), "in_progress"),
        (Some(TurnStatus::Waiting), "in_progress"),
        (Some(TurnStatus::Cancelled), "incomplete"),
        (Some(TurnStatus::Failed), "incomplete"),
        (Some(TurnStatus::Completed), "incomplete"),
    ] {
        let item = project(&rows, status).unwrap();
        assert_eq!(
            item["content"],
            json!([{"type":"output_text","text":"hello"}])
        );
        assert_eq!(item["phase"], "commentary");
        assert_eq!(item["status"], expected);
    }
    let aggregate = row(
        3,
        ItemKind::Message,
        json!({"role":"assistant","text":"hello","phase":"unknown"}),
    );
    let single = project(&[aggregate], Some(TurnStatus::Cancelled)).unwrap();
    assert_eq!(single["content"][0]["text"], "hello");
    assert_eq!(single["phase"], Value::Null);
    assert_eq!(single["status"], "incomplete");
}

#[test]
fn reasoning_joins_deltas_without_duplicating_a_complete_frame() {
    let first = row(1, ItemKind::Thought, json!({"text":"rea","delta":true}));
    let last = row(2, ItemKind::Thought, json!({"text":"son","delta":true}));
    let done = row(
        3,
        ItemKind::Thought,
        json!({"text":"reason","complete":true}),
    );
    let expected = json!({"type":"reasoning","id":"itim_example","turn_id":"turn_opener",
        "status":null,"summary":[{"type":"summary_text","text":"reason"}]});
    assert_eq!(
        project(&[last.clone(), first.clone()], None),
        Some(expected.clone())
    );
    assert_eq!(
        project(&[first, last, done.clone()], None),
        Some(expected.clone())
    );
    assert_eq!(project(&[done], None), Some(expected));
    assert_eq!(
        project(&[row(1, ItemKind::Thought, json!({"text":""}))], None),
        None
    );
}

#[test]
fn tool_call_preserves_mcp_name_and_requires_title_and_call_id() {
    let mut call = row(
        1,
        ItemKind::ToolCall,
        json!({"toolCallId":"call-1", "title":"mcp__srv__tool",
        "input":{"x":1}, "body":"fallback", "status":"pending"}),
    );
    assert_eq!(
        project(&[call.clone()], None),
        Some(json!({
            "type":"function_call", "id":"itim_example", "turn_id":"turn_opener", "call_id":"call-1",
            "name":"mcp__srv__tool", "arguments":{"x":1}, "status":"in_progress"
        }))
    );
    call.payload["input"] = Value::Null;
    assert_eq!(
        project(&[call.clone()], None).unwrap()["arguments"],
        "fallback"
    );
    for value in [json!(""), Value::Null] {
        let mut missing_title = call.clone();
        missing_title.payload["title"] = value;
        assert_eq!(project(&[missing_title], None), None);
    }
    call.payload.as_object_mut().unwrap().remove("toolCallId");
    assert_eq!(project(&[call], None), None);
}

#[test]
fn tool_output_status_and_error_follow_the_official_shape() {
    for (status, raw, output, error, expected_status) in [
        (
            json!("completed"),
            json!("ok"),
            json!("ok"),
            Value::Null,
            "completed",
        ),
        (
            json!("failed"),
            json!("boom"),
            Value::Null,
            json!("boom"),
            "failed",
        ),
        (
            json!("failed"),
            json!({"x":1}),
            Value::Null,
            json!("{\"x\":1}"),
            "failed",
        ),
        (
            Value::Null,
            json!({"x":1}),
            json!("{\"x\":1}"),
            Value::Null,
            "completed",
        ),
        (
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            "incomplete",
        ),
        (
            json!("in_progress"),
            json!("partial"),
            json!("partial"),
            Value::Null,
            "in_progress",
        ),
    ] {
        let item = row(
            1,
            ItemKind::ToolResult,
            json!({"toolCallId":"call-1","status":status,"output":raw}),
        );
        assert_eq!(
            project(&[item], None),
            Some(json!({"type":"function_call_output", "id":"itim_example",
            "turn_id":"turn_opener", "call_id":"call-1", "status":expected_status,"error":error,"output":output}))
        );
    }
    assert_eq!(
        project(
            &[row(1, ItemKind::ToolResult, json!({"output":"ok"}))],
            None
        ),
        None
    );
}

#[test]
fn dangling_call_status_follows_the_turn_terminal() {
    let call = row(
        1,
        ItemKind::ToolCall,
        json!({"toolCallId":"call-1","title":"tool","status":"in_progress"}),
    );
    for (status, expected) in [
        (None, "in_progress"),
        (Some(TurnStatus::Queued), "in_progress"),
        (Some(TurnStatus::InProgress), "in_progress"),
        (Some(TurnStatus::Waiting), "in_progress"),
        (Some(TurnStatus::Completed), "incomplete"),
        (Some(TurnStatus::Cancelled), "incomplete"),
        (Some(TurnStatus::Failed), "incomplete"),
    ] {
        assert_eq!(
            project(std::slice::from_ref(&call), status).unwrap()["status"],
            expected,
            "turn status {status:?}"
        );
    }
    // 调用行自身已带终态时以它为准（同页输出覆盖另由 `reconcile_calls` 负责）。
    let failed = row(
        1,
        ItemKind::ToolCall,
        json!({"toolCallId":"call-1","title":"tool","status":"failed"}),
    );
    assert_eq!(
        project(&[failed], Some(TurnStatus::Cancelled)).unwrap()["status"],
        "failed"
    );
}

#[test]
fn recovered_rows_are_never_projected_and_do_not_join_a_group() {
    let recovered = row(
        2,
        ItemKind::Message,
        json!({"role":"assistant","text":"recovered","complete":true,"recovered":true}),
    );
    assert!(
        ItemGroup::new(vec![&recovered]).is_none(),
        "整组都是恢复行时不参与投影"
    );
    // 与普通 delta 同组时，恢复行不参与：组仍按未完成 assistant 投影。
    let delta = row(
        1,
        ItemKind::Message,
        json!({"role":"assistant","text":"hi","delta":true}),
    );
    let group = ItemGroup::new(vec![&delta, &recovered]).expect("group");
    assert_eq!(group.class, GroupClass::UnfinishedAssistant);
    assert_eq!(
        project(&[delta, recovered], Some(TurnStatus::InProgress)).unwrap()["content"],
        json!([{"type":"output_text","text":"hi"}])
    );
}

#[test]
fn only_complete_root_facts_with_a_turn_can_be_projected() {
    for kind in [ItemKind::Receipt, ItemKind::Approval, ItemKind::ToolUpdate] {
        assert_eq!(
            project(
                &[row(1, kind, json!({"toolCallId":"x","text":"ignored"}))],
                None
            ),
            None
        );
    }
    let mut message = row(
        1,
        ItemKind::Message,
        json!({"role":"assistant","text":"hello","delta":true}),
    );
    let group = ItemGroup::new(vec![&message]).unwrap();
    assert_eq!(map_group(&group, None), None);
    message.source.agent_id = "child:agent-01".into();
    assert_eq!(project(&[message.clone()], None), None);
    message.source = ItemSource::root();
    message.truncated = true;
    let intact = row(
        2,
        ItemKind::Message,
        json!({"role":"assistant","text":"lo","delta":true}),
    );
    assert_eq!(project(&[message, intact], None), None);
    let mut thought = row(1, ItemKind::Thought, json!({"text":"secret"}));
    thought.truncated = true;
    assert_eq!(project(&[thought], None), None);
}
