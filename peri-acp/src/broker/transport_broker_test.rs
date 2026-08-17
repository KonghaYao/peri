//! Tests for `broker/transport_broker.rs` 提取的 elicitation 纯函数
//! （`build_elicitation_params` / `parse_elicitation_response`）。
//!
//! `AcpTransportBroker`（mpsc/TUI 路径）与 `StdioQuestionBroker`（stdio 路径）
//! 共用这两处逻辑（行为零变化复用面），纯函数单测直接锁住 schema 构造
//! （单选 oneOf / 多选 array+items.anyOf / 自由文本 + option description 注入）
//! 与响应解析语义（accept/decline/cancel/未知 action/解析失败兜底）。

use peri_acp_types::interaction::{InteractionResponse, QuestionItem, QuestionOption};
use serde_json::json;

use super::*;

/// 三形态问题集（与 stdio 双端测试同构）：单选 / 多选 / 自由文本。
fn sample_questions() -> Vec<QuestionItem> {
    vec![
        QuestionItem {
            id: "choice".into(),
            question: "选择部署环境？".into(),
            header: "部署环境".into(),
            options: vec![
                QuestionOption {
                    label: "生产".into(),
                    description: Some("生产环境".into()),
                },
                QuestionOption {
                    label: "测试".into(),
                    description: None,
                },
            ],
            multi_select: false,
        },
        QuestionItem {
            id: "multi".into(),
            question: "选择功能？".into(),
            header: "功能".into(),
            options: vec![
                QuestionOption {
                    label: "a".into(),
                    description: Some("desc-a".into()),
                },
                QuestionOption {
                    label: "b".into(),
                    description: None,
                },
            ],
            multi_select: true,
        },
        QuestionItem {
            id: "text".into(),
            question: "补充说明？".into(),
            header: "补充".into(),
            options: vec![],
            multi_select: false,
        },
    ]
}

// ─── build_elicitation_params：schema 构造 ───

#[test]
fn test_build_elicitation_params_single_select_form() {
    let params = build_elicitation_params(&sample_questions(), SessionId::new("s1"));

    // 顶层协议形状：mode=form + sessionId scope + message
    assert_eq!(params["mode"], "form");
    assert_eq!(params["sessionId"], "s1");
    assert_eq!(
        params["message"],
        "Please provide the requested information"
    );

    // 单选：type=string + oneOf（const/title）+ title/description
    let choice = &params["requestedSchema"]["properties"]["choice"];
    assert_eq!(choice["type"], "string");
    assert_eq!(choice["title"], "部署环境");
    assert_eq!(choice["description"], "选择部署环境？");
    assert_eq!(choice["oneOf"][0]["const"], "生产");
    assert_eq!(choice["oneOf"][0]["title"], "生产");
    assert_eq!(choice["oneOf"][0]["description"], "生产环境");
    assert_eq!(choice["oneOf"][1]["const"], "测试");
}

#[test]
fn test_build_elicitation_params_multi_select_and_free_text() {
    let params = build_elicitation_params(&sample_questions(), SessionId::new("s1"));
    let props = &params["requestedSchema"]["properties"];

    // 多选：type=array + items.anyOf，option description 注入 items 层
    let multi = &props["multi"];
    assert_eq!(multi["type"], "array");
    assert_eq!(multi["title"], "功能");
    assert_eq!(multi["items"]["anyOf"][0]["const"], "a");
    assert_eq!(multi["items"]["anyOf"][0]["description"], "desc-a");
    assert!(multi["items"]["anyOf"][1].get("description").is_none());

    // 自由文本：type=string，无 oneOf/anyOf
    let text = &props["text"];
    assert_eq!(text["type"], "string");
    assert_eq!(text["title"], "补充");
    assert_eq!(text["description"], "补充说明？");
    assert!(text.get("oneOf").is_none());
}

#[test]
fn test_build_elicitation_params_empty_questions() {
    let params = build_elicitation_params(&[], SessionId::new("s1"));
    assert_eq!(params["mode"], "form");
    assert_eq!(params["sessionId"], "s1");
    assert_eq!(
        params["requestedSchema"]["properties"],
        json!({}),
        "空问题集 → 空 schema"
    );
}

// ─── parse_elicitation_response：accept ───

#[test]
fn test_parse_accept_maps_text_and_selected() {
    let response = parse_elicitation_response(
        json!({
            "action": "accept",
            "content": {
                "choice": "生产",
                "multi": ["a", "b"],
                "text": "自由补充",
            }
        }),
        sample_questions(),
    );

    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    // 单选 → text；多选 → selected；自由文本 → text
    assert_eq!(answers[0].id, "choice");
    assert_eq!(answers[0].text.as_deref(), Some("生产"));
    assert!(answers[0].selected.is_empty());
    assert_eq!(answers[1].id, "multi");
    assert_eq!(answers[1].selected, vec!["a", "b"]);
    assert!(answers[1].text.is_none());
    assert_eq!(answers[2].id, "text");
    assert_eq!(answers[2].text.as_deref(), Some("自由补充"));
}

#[test]
fn test_parse_accept_boolean_and_integer_to_text() {
    // map_elicitation_answer 的 Boolean/Integer 分支 → text（字符串化）。
    let response = parse_elicitation_response(
        json!({
            "action": "accept",
            "content": { "choice": true, "text": 42 }
        }),
        sample_questions(),
    );
    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    assert_eq!(answers[0].text.as_deref(), Some("true"));
    assert_eq!(answers[2].text.as_deref(), Some("42"));
}

#[test]
fn test_parse_accept_missing_content_keeps_none() {
    // content 缺失/缺 q_id → text=None（不兜底空串，区别于 cancel）。
    let response = parse_elicitation_response(json!({ "action": "accept" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    for a in &answers {
        assert!(a.text.is_none(), "缺 content → text=None: {a:?}");
        assert!(a.selected.is_empty());
    }
}

// ─── parse_elicitation_response：decline / cancel / 兜底 ───

#[test]
fn test_parse_decline_rejected() {
    let response = parse_elicitation_response(json!({ "action": "decline" }), sample_questions());
    assert!(
        matches!(response, InteractionResponse::Rejected),
        "decline 应返回 Rejected: {response:?}"
    );
}

#[test]
fn test_parse_cancel_empty_answers() {
    let response = parse_elicitation_response(json!({ "action": "cancel" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("cancel 应返回 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    for a in &answers {
        assert!(a.selected.is_empty());
        assert_eq!(a.text.as_deref(), Some(""), "cancel → 空 Answers");
    }
}

#[test]
fn test_parse_unknown_action_empty_answers() {
    // 未知 action（Other 变体）→ 空 Answers 兜底。
    let response = parse_elicitation_response(json!({ "action": "mystery" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("未知 action 应返回空 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].text.as_deref(), Some(""));
}

#[test]
fn test_parse_invalid_response_empty_answers() {
    // 响应解析失败 → 空 Answers 兜底（不 panic）。
    let response = parse_elicitation_response(json!({ "not": "a response" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("解析失败应返回空 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].text.as_deref(), Some(""));
}
