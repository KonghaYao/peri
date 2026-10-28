//! Login typed 草稿、动态字段、类型锁定与四档子区回归。

use super::*;

// ── Login typed 草稿 / 动态字段 / 四档子区 ─────────────────────────────────

#[test]
#[serial]
fn new_provider_cycles_three_types_without_revealing_profile_fields() {
    let cfg = login_config();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.provider_id = "new".into();
    state.api_key = "key".into();
    state.models.sonnet = "new-sonnet".into();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 2, move |index, panel| {
        if index == 0 {
            let _ = panel.event(key(KeyCode::Right), TEST_AREA);
            let second = panel.edit_state.read().as_ref().map(|state| state.kind);
            let _ = panel.event(key(KeyCode::Right), TEST_AREA);
            let responses = panel
                .edit_state
                .read()
                .as_ref()
                .map(|state| (state.kind, state.visible_fields()));
            let _ = panel.event(key(KeyCode::Left), TEST_AREA);
            let third = panel.edit_state.read().as_ref().map(|state| state.kind);
            tx.send((second, responses, third)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (second, responses, third) = rx.try_recv().unwrap();
    assert_eq!(second, Some(Some(ProviderKind::OpenAi)));
    let (kind, visible) = responses.unwrap();
    assert_eq!(kind, Some(ProviderKind::OpenAiResponses));
    assert_eq!(
        visible,
        vec![
            LoginField::ProviderType,
            LoginField::ProviderId,
            LoginField::AuthMode,
            LoginField::BaseUrl,
            LoginField::ApiKey,
            LoginField::FableModel,
            LoginField::OpusModel,
            LoginField::SonnetModel,
            LoginField::HaikuModel,
            LoginField::Confirm,
        ],
        "Login must not expose any profile binding/window field"
    );
    assert_eq!(third, Some(Some(ProviderKind::OpenAi)));
}

#[test]
#[serial]
fn existing_responses_type_is_locked_until_explicit_confirmation() {
    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 2, move |index, panel| {
        if index == 0 {
            for code in [KeyCode::Left, KeyCode::Right, KeyCode::Char(' ')] {
                let _ = panel.event(key(code), TEST_AREA);
            }
            let locked = panel.edit_state.read().as_ref().map(|state| state.kind);
            let opened = panel.event(key(KeyCode::Enter), TEST_AREA);
            let mode = *panel.mode.read();
            let candidate = panel
                .edit_state
                .read()
                .as_ref()
                .and_then(|state| state.type_change_candidate);
            let right = panel.event(key(KeyCode::Right), TEST_AREA);
            let candidate_after = panel
                .edit_state
                .read()
                .as_ref()
                .and_then(|state| state.type_change_candidate);
            let esc = panel.event(key(KeyCode::Esc), TEST_AREA);
            let unchanged = panel.edit_state.read().as_ref().map(|state| state.kind);
            tx.send((
                locked,
                opened,
                mode,
                candidate,
                right,
                candidate_after,
                esc,
                unchanged,
            ))
            .unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (locked, opened, mode, candidate, right, candidate_after, esc, unchanged) =
        rx.try_recv().unwrap();
    assert_eq!(locked, Some(Some(ProviderKind::OpenAiResponses)));
    assert_eq!(opened, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::ConfirmTypeChange);
    assert_eq!(candidate, Some(ProviderKind::OpenAiResponses));
    assert_eq!(right, EventResult::Consumed);
    assert_eq!(candidate_after, Some(ProviderKind::Anthropic));
    assert_eq!(esc, EventResult::Consumed);
    assert_eq!(unchanged, Some(Some(ProviderKind::OpenAiResponses)));
}

#[test]
#[serial]
fn navigation_skips_hidden_fields_and_paste_on_choices_is_safe() {
    let cfg = login_config();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.kind = Some(ProviderKind::OpenAi);
    state.provider_id = "new".into();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 2, move |index, panel| {
        if index == 0 {
            let paste_type = panel.event(Event::Paste("evil".into()), TEST_AREA);
            let before = panel
                .edit_state
                .read()
                .as_ref()
                .map(|state| state.field_text(LoginField::ProviderId));
            *panel.edit_focus.write() = LoginField::Confirm;
            let paste_confirm = panel.event(Event::Paste("evil".into()), TEST_AREA);
            let _ = panel.event(key(KeyCode::Up), TEST_AREA);
            let after_up = *panel.edit_focus.read();
            let visible = panel
                .edit_state
                .read()
                .as_ref()
                .map(|state| state.visible_fields());
            tx.send((paste_type, before, paste_confirm, after_up, visible))
                .unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (paste_type, before, paste_confirm, after_up, visible) = rx.try_recv().unwrap();
    assert_eq!(paste_type, EventResult::Ignored);
    assert_eq!(paste_confirm, EventResult::Ignored);
    assert_eq!(before.as_deref(), Some("new"));
    assert_eq!(after_up, LoginField::HaikuModel);
    assert_eq!(
        visible.unwrap(),
        vec![
            LoginField::ProviderType,
            LoginField::ProviderId,
            LoginField::BaseUrl,
            LoginField::ApiKey,
            LoginField::FableModel,
            LoginField::OpusModel,
            LoginField::SonnetModel,
            LoginField::HaikuModel,
            LoginField::Confirm,
        ],
        "navigation set must contain provider fields only"
    );
}

#[test]
fn clean_paste_clears_the_single_line_error() {
    let cfg = login_config();
    let mut state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let mut cursor = 0;
    super::edit_handler::handle_login_paste(&mut cursor, &mut state, LoginField::BaseUrl, "12\n34");
    let error = state
        .inline_error
        .as_ref()
        .expect("含换行的粘贴必须给出单行提示");
    assert_eq!(error.field, LoginField::BaseUrl);
    assert_eq!(error.message, crate::i18n::tr("login-error-single-line"));

    super::edit_handler::handle_login_paste(&mut cursor, &mut state, LoginField::BaseUrl, "56");
    assert!(
        state.inline_error.is_none(),
        "干净粘贴必须清除上一轮单行错误，避免永久阻断保存"
    );
}

#[test]
#[serial]
fn plan_provider_hides_api_key_fields_and_browse_does_not_claim_missing_key() {
    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "plan", None).unwrap();
    let visible = state.visible_fields();
    assert!(!visible.contains(&LoginField::BaseUrl));
    assert!(!visible.contains(&LoginField::ApiKey));
    assert!(state.plan_reference().is_some());

    let _list = ProviderListGuard::install(vec![ProviderSummary {
        id: "plan".into(),
        provider_type: "openai_responses".into(),
        is_active: false,
        has_api_key: false,
        has_chatgpt_plan: true,
        base_url: None,
    }]);
    let text = buffer_text(&mount_panel_at(2, |_| {}));
    assert!(text.contains("ChatGPT plan"), "{text}");
    assert!(!text.contains("api key: missing"), "{text}");
}

#[test]
fn focus_row_matches_the_rendered_marker_for_every_visible_field() {
    use super::render::{self, LoginRenderColors};

    let cfg = login_config();
    let colors = LoginRenderColors::reset();

    let mut new_responses = LoginEditState::new_provider(&cfg, None);
    new_responses.kind = Some(ProviderKind::OpenAiResponses);
    new_responses.provider_id = "new".into();
    new_responses.api_key = "key".into();
    new_responses.models.sonnet = "new-sonnet".into();

    let mut new_openai = LoginEditState::new_provider(&cfg, None);
    new_openai.kind = Some(ProviderKind::OpenAi);
    new_openai.provider_id = "new".into();
    new_openai.api_key = "key".into();

    let plan = LoginEditState::edit_provider(&cfg, "plan", None).unwrap();

    let responses = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();

    for state in [&new_responses, &new_openai, &plan, &responses] {
        let auth = super::auth::AuthController::new();
        for field in state.visible_fields() {
            let width = 100;
            let row = render::focus_row(state, &auth, field, width);
            let layout = render::build_edit_layout(state, &auth, field, 0, &colors, width);
            let marker_rows: Vec<usize> = layout
                .lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.spans.iter().any(|span| span.content.contains('❯')))
                .map(|(index, _)| index)
                .collect();
            assert_eq!(
                marker_rows,
                vec![row],
                "field {field:?} focus row must match the rendered marker"
            );
            assert_eq!(layout.focus_row, Some(row), "focus_row 必须显式记录");
            assert_eq!(
                layout.lines.len(),
                render::line_count(state, &auth, field, width)
            );
        }

        // 键盘可见字段与鼠标命中字段必须来自同一集合。
        let layout =
            render::build_edit_layout(state, &auth, LoginField::ProviderType, 0, &colors, 100);
        let visible = state.visible_fields();
        for field in &visible {
            if *field == LoginField::AuthActions {
                assert!(
                    !layout.action_spans().is_empty(),
                    "AuthActions must have per-action mouse hit regions"
                );
                continue;
            }
            assert!(
                layout.hits.contains(&Some(render::EditHit::Field(*field))),
                "visible field {field:?} must have a mouse hit region"
            );
        }
        for field in layout.hits.iter().flatten().filter_map(|hit| match hit {
            render::EditHit::Field(field) => Some(*field),
            render::EditHit::AuthAction(_)
            | render::EditHit::TypeChangeCandidate
            | render::EditHit::TypeChangeConfirm => None,
        }) {
            assert!(
                visible.contains(&field),
                "mouse hit field {field:?} must be keyboard-visible"
            );
        }
    }
}

#[test]
#[serial]
fn tab_order_skips_removed_profile_fields_and_leaves_no_blank_gap() {
    use super::render::{self, LoginRenderColors};

    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let expected = state.visible_fields();
    assert_eq!(
        expected,
        vec![
            LoginField::ProviderType,
            LoginField::ProviderId,
            LoginField::AuthMode,
            LoginField::BaseUrl,
            LoginField::ApiKey,
            LoginField::FableModel,
            LoginField::OpusModel,
            LoginField::SonnetModel,
            LoginField::HaikuModel,
            LoginField::Confirm,
        ],
        "Responses Edit must expose provider fields only"
    );

    let auth = super::auth::AuthController::new();
    let layout = render::build_edit_layout(
        &state,
        &auth,
        LoginField::ProviderType,
        0,
        &LoginRenderColors::reset(),
        100,
    );
    let text: String = layout
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    for removed in [
        "档位配置",
        "编辑档位",
        "绑定此 Provider",
        "此档模型",
        "上下文窗口",
    ] {
        assert!(
            !text.contains(removed),
            "removed UI leaked: {removed}\n{text}"
        );
    }
    let mut blank_run = 0usize;
    for (row, line) in layout.lines.iter().enumerate() {
        let line_text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        if line_text.trim().is_empty() {
            blank_run += 1;
            assert!(blank_run <= 1, "consecutive blank rows at {row}");
        } else {
            blank_run = 0;
        }
    }

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let expected_for_closure = expected.clone();
    mount_edit_state(state, LoginField::ProviderType, 2, move |index, panel| {
        if index == 0 {
            let mut seen = vec![*panel.edit_focus.read()];
            for _ in 1..expected_for_closure.len() {
                let _ = panel.event(key(KeyCode::Tab), TEST_AREA);
                seen.push(*panel.edit_focus.read());
            }
            let _ = panel.event(key(KeyCode::Tab), TEST_AREA);
            tx.send((seen, *panel.edit_focus.read())).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (seen, wrapped) = rx.try_recv().unwrap();
    assert_eq!(seen, expected, "Tab order must equal the visible field set");
    assert_eq!(
        wrapped,
        LoginField::ProviderType,
        "Tab wraps to the first field"
    );
}

#[test]
#[serial]
fn keyboard_focus_scrolls_into_the_short_panel_viewport() {
    let cfg = login_config();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.kind = Some(ProviderKind::OpenAiResponses);
    state.provider_id = "new".into();
    state.api_key = "key".into();
    state.models.sonnet = "new-sonnet".into();

    let short = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 2, move |index, panel| {
        if index == 0 {
            for _ in 0..40 {
                if *panel.edit_focus.read() == LoginField::Confirm {
                    break;
                }
                let _ = panel.event(key(KeyCode::Down), short);
            }
            let focus = *panel.edit_focus.read();
            let offset = panel.sv.read().offset().y as usize;
            let state = panel.edit_state.read().clone().unwrap();
            let auth = panel.auth.read();
            let row = super::render::focus_row(&state, &auth, focus, short.width);
            let total = super::render::line_count(&state, &auth, focus, short.width);
            let height = short.height.saturating_sub(2) as usize;
            tx.send((focus, offset, row, total, height)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (focus, offset, row, total, height) = rx.try_recv().unwrap();
    assert_eq!(focus, LoginField::Confirm);
    assert!(
        total > height,
        "短面板内容必须超过视口：{total} vs {height}"
    );
    assert!(offset > 0, "焦点移动后必须滚动");
    assert!(
        row >= offset && row < offset + height,
        "焦点行 {row} 必须落在视口 [{offset}, {})",
        offset + height
    );
}
