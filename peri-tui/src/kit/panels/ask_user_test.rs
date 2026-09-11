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

    let mut form = FormState::default();
    assert!(form.reset_for_owner_change(Some(&a)));
    form.focused = 2;
    form.answers = vec![vec![1]];
    form.focused_option = 1;
    form.is_typing = true;
    form.typing_state.insert_str("answer owned by A");
    form.custom_answers = vec![Some("A custom".into())];
    let mut scroll =
        ScrollViewState::with_offset(ratatui_kit::ratatui::layout::Position::new(0, 9));

    // Re-rendering the same owner must preserve both form edits and scroll.
    assert!(!reset_for_owner_change(&mut form, Some(&a), &mut scroll));
    assert_eq!(form.typing_state.all_text(), "answer owned by A");
    assert_eq!(scroll.offset().y, 9);
    assert!(reset_for_owner_change(&mut form, Some(&b), &mut scroll));
    assert_eq!(form.focused, 0);
    assert_eq!(form.answers, vec![Vec::<usize>::new()]);
    assert_eq!(form.focused_option, 0);
    assert!(!form.is_typing);
    assert_eq!(form.typing_state.all_text(), "");
    assert_eq!(form.typing_state.cursor_byte(), 0);
    assert_eq!(form.custom_answers, vec![None]);
    assert_eq!(scroll.offset().y, 0);
    assert!(!reset_for_owner_change(&mut form, Some(&b), &mut scroll));
    assert_eq!(
        build_answers_map(Some(&b.payload), &form.answers, &form.custom_answers),
        json!({"same": ""})
    );
    assert!(reset_for_owner_change(&mut form, None, &mut scroll));
    assert!(form.answers.is_empty());
    assert!(form.custom_answers.is_empty());
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

fn form_for(payload: &AskUser) -> FormState {
    let mut form = FormState::default();
    form.reset_for_owner_change(Some(&PendingInteraction {
        owner: crate::acp_client::InteractionOwner {
            token: 1,
            ..Default::default()
        },
        request_id_json: "1".into(),
        payload: payload.clone(),
    }));
    form
}

fn press(
    form: &mut FormState,
    payload: &AskUser,
    code: ratatui_kit::crossterm::event::KeyCode,
) -> FormOutcome {
    use ratatui_kit::crossterm::event::{KeyEvent, KeyModifiers};
    form.handle_key(&KeyEvent::new(code, KeyModifiers::NONE), Some(payload), 40)
}

#[test]
fn form_mouse_and_space_share_single_and_multi_selection_semantics() {
    use ratatui_kit::crossterm::event::KeyCode;
    for multi in [false, true] {
        let payload = make_ask_user(vec![make_question("q", multi, &["A", "B"])]);
        let mut mouse_form = form_for(&payload);
        let mut key_form = form_for(&payload);
        for index in [0, 1, 1, 0] {
            mouse_form.toggle_option(0, index, multi);
            key_form.focused_option = index;
            assert_eq!(
                press(&mut key_form, &payload, KeyCode::Char(' ')),
                FormOutcome::Consumed
            );
            assert_eq!(mouse_form.answers, key_form.answers);
        }
    }
}

#[test]
fn form_confirm_moves_to_unanswered_question_then_emits_complete_answers() {
    use ratatui_kit::crossterm::event::KeyCode;
    let payload = make_ask_user(vec![
        make_question("first", false, &["A", "B"]),
        make_question("optional", false, &[]),
        make_question("last", true, &["X", "Y"]),
    ]);
    let mut form = form_for(&payload);
    press(&mut form, &payload, KeyCode::Char(' '));
    assert_eq!(
        press(&mut form, &payload, KeyCode::Enter),
        FormOutcome::Consumed
    );
    assert_eq!(
        form.focused, 2,
        "confirmation skips questions without options"
    );
    assert_eq!(form.focused_option, 0);
    press(&mut form, &payload, KeyCode::Char(' '));
    press(&mut form, &payload, KeyCode::Down);
    press(&mut form, &payload, KeyCode::Char(' '));
    assert_eq!(
        press(&mut form, &payload, KeyCode::Enter),
        FormOutcome::Submit(json!({"first": "A", "optional": "", "last": ["X", "Y"]}))
    );
    // The decision leaves canonical answers available until the owner closes.
    assert_eq!(form.answers, vec![vec![0], vec![], vec![0, 1]]);
}

#[test]
fn form_custom_answer_enter_saves_trimmed_text_and_escape_only_exits_editor() {
    use ratatui_kit::crossterm::event::KeyCode;
    let payload = make_ask_user(vec![make_question("q", false, &["A"])]);
    let mut form = form_for(&payload);
    assert_eq!(
        press(&mut form, &payload, KeyCode::Down),
        FormOutcome::Consumed
    );
    assert!(form.is_typing);
    for c in " 中文 ".chars() {
        press(&mut form, &payload, KeyCode::Char(c));
    }
    assert_eq!(
        press(&mut form, &payload, KeyCode::Enter),
        FormOutcome::Consumed
    );
    assert_eq!(form.custom_answers, vec![Some("中文".into())]);
    assert!(!form.is_typing);
    press(&mut form, &payload, KeyCode::Char(' '));
    assert_eq!(form.typing_state.all_text(), "中文");
    press(&mut form, &payload, KeyCode::Char('!'));
    assert_eq!(
        press(&mut form, &payload, KeyCode::Esc),
        FormOutcome::Consumed
    );
    assert_eq!(form.custom_answers, vec![Some("中文".into())]);
    assert_eq!(
        press(&mut form, &payload, KeyCode::Esc),
        FormOutcome::RequestCancel
    );
    assert_eq!(
        press(&mut form, &payload, KeyCode::Enter),
        FormOutcome::Submit(json!({"q": "中文"}))
    );
}

#[test]
fn form_empty_edit_preserves_saved_answer_and_up_returns_to_last_preset() {
    use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let payload = make_ask_user(vec![make_question("q", false, &["A", "B"])]);
    let mut form = form_for(&payload);
    form.custom_answers[0] = Some("saved".into());
    form.begin_custom_input(0);
    form.handle_key(
        &KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        Some(&payload),
        40,
    );
    assert_eq!(
        press(&mut form, &payload, KeyCode::Enter),
        FormOutcome::Consumed
    );
    assert_eq!(form.custom_answers[0].as_deref(), Some("saved"));
    form.begin_custom_input(0);
    assert_eq!(
        press(&mut form, &payload, KeyCode::Up),
        FormOutcome::Consumed
    );
    assert!(!form.is_typing);
    assert_eq!(form.focused_option, 1);
    assert_eq!(form.custom_answers[0].as_deref(), Some("saved"));
}

#[test]
fn form_tab_restores_selected_option_and_does_not_leave_active_editor() {
    use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let payload = make_ask_user(vec![
        make_question("a", false, &["A", "B"]),
        make_question("b", false, &["X"]),
    ]);
    let mut form = form_for(&payload);
    form.toggle_option(0, 1, false);
    assert_eq!(
        press(&mut form, &payload, KeyCode::Tab),
        FormOutcome::Consumed
    );
    assert_eq!(form.focused, 1);
    assert_eq!(
        form.handle_key(
            &KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(&payload),
            40
        ),
        FormOutcome::Consumed
    );
    assert_eq!(form.focused, 0);
    assert_eq!(form.focused_option, 1);
    form.begin_custom_input(0);
    assert_eq!(
        press(&mut form, &payload, KeyCode::Tab),
        FormOutcome::Consumed
    );
    assert_eq!(form.focused, 0);
    assert!(form.is_typing);
}

#[test]
fn form_empty_questions_and_unrelated_keys_have_explicit_decisions() {
    use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let payload = make_ask_user(vec![]);
    let mut form = form_for(&payload);
    for code in [KeyCode::Tab, KeyCode::Char('x')] {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        assert!(!form.accepts_key(&key, Some(&payload)));
        assert_eq!(
            form.handle_key(&key, Some(&payload), 40),
            FormOutcome::Ignored
        );
    }
    assert_eq!(
        press(&mut form, &payload, KeyCode::Enter),
        FormOutcome::Submit(json!({}))
    );
}
