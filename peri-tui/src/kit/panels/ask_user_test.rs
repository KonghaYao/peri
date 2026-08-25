#![cfg(test)]

use super::*;
use peri_acp_types::event_data::{AskUser, Question};
use serde_json::json;

#[test]
fn test_ask_user_same_question_ids_new_owner_resets_answers() {
    use crate::kit::acp_types::PendingInteraction;

    let questions = vec![make_question("same", false, &["A", "B"])];
    let owner_a = crate::acp_client::InteractionOwner {
        client_instance_id: 1,
        token: 7,
        session_id: "s1".into(),
        generation: 3,
        prompt_epoch: 5,
        ..Default::default()
    };
    let owner_b = crate::acp_client::InteractionOwner {
        token: 8,
        ..owner_a.clone()
    };
    let a = PendingInteraction {
        owner: owner_a,
        request_id_json: "\"same-wire-id\"".into(),
        payload: AskUser {
            questions: questions.clone(),
        },
    };
    let b = PendingInteraction {
        owner: owner_b,
        request_id_json: "\"same-wire-id\"".into(),
        payload: AskUser { questions },
    };

    let mut fingerprint = interaction_fingerprint(Some(&a));
    let mut focused = 2;
    let mut answers = vec![vec![1]];
    let mut focused_option = 1;
    let mut is_typing = true;
    let mut typing_state = TextAreaState::default();
    typing_state.insert_str("answer owned by A");
    let mut custom_answers = vec![Some("A custom".into())];
    let mut scroll =
        ScrollViewState::with_offset(ratatui_kit::ratatui::layout::Position::new(0, 9));

    assert!(reset_for_owner_change(
        &mut fingerprint,
        Some(&b),
        1,
        &mut focused,
        &mut answers,
        &mut focused_option,
        &mut is_typing,
        &mut typing_state,
        &mut custom_answers,
        &mut scroll,
    ));
    assert_eq!(focused, 0);
    assert_eq!(answers, vec![Vec::<usize>::new()]);
    assert_eq!(focused_option, 0);
    assert!(!is_typing);
    assert_eq!(typing_state.all_text(), "");
    assert_eq!(typing_state.cursor_byte(), 0);
    assert_eq!(custom_answers, vec![None]);
    assert_eq!(scroll.offset().y, 0);
    assert_eq!(fingerprint, interaction_fingerprint(Some(&b)));
    assert_eq!(
        build_answers_map(Some(&b.payload), &answers, &custom_answers),
        json!({"same": ""})
    );
}

fn make_question(id: &str, multi_select: bool, labels: &[&str]) -> Question {
    use peri_acp_types::event_data::QuestionOption;
    Question {
        id: id.to_string(),
        header: id.to_string(),
        question: format!("Question {id}"),
        options: labels
            .iter()
            .map(|l| QuestionOption {
                label: l.to_string(),
                description: String::new(),
            })
            .collect(),
        multi_select,
    }
}

fn make_ask_user(questions: Vec<Question>) -> AskUser {
    AskUser { questions }
}

// ─── build_answers_map ──────────────────────────────────────────

#[test]
fn test_build_answers_map_single_select_preset() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B", "C"])]);
    let answers = vec![vec![1usize]];
    let custom = vec![None];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": "B"}));
}

#[test]
fn test_build_answers_map_multi_select_preset() {
    let au = make_ask_user(vec![make_question("q1", true, &["A", "B", "C"])]);
    let answers = vec![vec![0usize, 2]];
    let custom = vec![None];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": ["A", "C"]}));
}

#[test]
fn test_build_answers_map_custom_text() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B"])]);
    let answers = vec![vec![]];
    let custom = vec![Some("my custom answer".to_string())];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": "my custom answer"}));
}

#[test]
fn test_build_answers_map_custom_overrides_preset() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B"])]);
    let answers = vec![vec![0usize]];
    let custom = vec![Some("overridden text".to_string())];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": "overridden text"}));
}

#[test]
fn test_build_answers_map_empty_custom_not_override() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B"])]);
    let answers = vec![vec![1usize]];
    let custom = vec![Some(String::new())];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": ""}));
}

#[test]
fn test_build_answers_map_mixed_preset_and_custom() {
    let au = make_ask_user(vec![
        make_question("q1", false, &["A", "B"]),
        make_question("q2", true, &["X", "Y", "Z"]),
        make_question("q3", false, &["P", "Q"]),
    ]);
    // q1: custom only, q2: multi-select + custom, q3: preset only
    let answers = vec![vec![], vec![0usize, 1], vec![1usize]];
    let custom = vec![
        Some("custom answer".to_string()),
        Some("extra note".to_string()),
        None,
    ];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(
        result,
        json!({"q1": "custom answer", "q2": ["X", "Y", "extra note"], "q3": "Q"})
    );
}

#[test]
fn test_build_answers_map_multi_select_empty_with_custom() {
    // 多选：仅自定义文本，无预设选项
    let au = make_ask_user(vec![make_question("q1", true, &["A", "B"])]);
    let answers = vec![vec![]];
    let custom = vec![Some("only me".to_string())];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": ["only me"]}));
}

// ─── wrap_text ──────────────────────────────────────────────────

#[test]
fn test_wrap_text_short_returns_single_line() {
    let result = wrap_text("hello", 80);
    assert_eq!(result, vec!["hello"]);
}

#[test]
fn test_wrap_text_long_splits_at_whitespace() {
    // wrap_text prefers whitespace breaks: "hello world" fits (11≤12),
    // then "foo" & "bar" are separate because the space after "foo" is
    // the preferred break point.
    let result = wrap_text("hello world foo bar", 12);
    assert_eq!(result, vec!["hello world", "foo", "bar"]);
}

#[test]
fn test_wrap_text_cjk_splits_at_boundary() {
    // 12 CJK chars × 2 width = 24 total. max_width=10 → 5 chars per line.
    let result = wrap_text("你好世界你好世界你好世界", 10);
    assert_eq!(result, vec!["你好世界你", "好世界你好", "世界"]);
}

#[test]
fn test_wrap_text_empty_returns_single_empty() {
    let result = wrap_text("", 80);
    assert_eq!(result, vec![""]);
}

#[test]
fn test_wrap_text_zero_width_returns_original() {
    let result = wrap_text("hello", 0);
    assert_eq!(result, vec!["hello"]);
}

// ─── TextAreaState 基础行为 ─────────────────────────────────────
// 验证 TextAreaState 满足自定义文本输入所需的基本操作

#[test]
fn test_textarea_state_insert_and_retrieve() {
    use crate::components::textarea::TextAreaState;
    let mut state = TextAreaState::default();
    state.insert_char('h');
    state.insert_char('i');
    assert_eq!(state.text, "hi");
}

#[test]
fn test_textarea_state_backspace_clears() {
    use crate::components::textarea::TextAreaState;
    let mut state = TextAreaState::default();
    state.insert_char('x');
    state.backspace();
    assert!(state.text.is_empty());
}

#[test]
fn test_textarea_state_replace_all_no_undo_resets() {
    use crate::components::textarea::TextAreaState;
    let mut state = TextAreaState::default();
    state.insert_str("old text");
    state.replace_all_no_undo("new".to_string());
    assert_eq!(state.text, "new");
}

#[test]
fn test_textarea_state_delete_word_backward() {
    use crate::components::textarea::TextAreaState;
    let mut state = TextAreaState::default();
    state.insert_str("hello");
    state.cursor = state.text.len();
    state.delete_word_backward();
    assert!(state.text.is_empty());
}
