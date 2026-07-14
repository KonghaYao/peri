#![cfg(test)]

use super::*;
use peri_acp_types::event_data::{AskUser, Question};
use serde_json::json;

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
    let answers = vec![vec![], vec![0usize, 2], vec![1usize]];
    let custom = vec![Some("custom answer".to_string()), None, None];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(
        result,
        json!({"q1": "custom answer", "q2": ["X", "Z"], "q3": "Q"})
    );
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

// ─── InputMode ──────────────────────────────────────────────────

#[test]
fn test_input_mode_selecting_is_default() {
    let mode = InputMode::Selecting;
    assert_eq!(mode, InputMode::Selecting);
}

#[test]
fn test_input_mode_typing_holds_buffer() {
    let mode = InputMode::Typing {
        buffer: "hello".to_string(),
    };
    match mode {
        InputMode::Typing { buffer } => assert_eq!(buffer, "hello"),
        _ => panic!("expected Typing mode"),
    }
}
