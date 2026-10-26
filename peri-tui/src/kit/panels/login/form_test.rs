//! Login typed 草稿、动态字段、类型锁定与四档子区回归。

use super::*;

// ── Login typed 草稿 / 动态字段 / 四档子区 ─────────────────────────────────

#[test]
#[serial]
fn new_provider_cycles_three_types_and_reveals_profile_fields() {
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
            let responses = panel.edit_state.read().as_ref().map(|state| {
                (
                    state.kind,
                    state
                        .visible_fields()
                        .contains(&LoginField::ProfileSelector),
                    state.selected().alias.to_string(),
                )
            });
            let _ = panel.event(key(KeyCode::Left), TEST_AREA);
            let third = panel.edit_state.read().as_ref().map(|state| state.kind);
            tx.send((second, responses, third)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (second, responses, third) = rx.try_recv().unwrap();
    assert_eq!(second, Some(Some(ProviderKind::OpenAi)));
    assert_eq!(
        responses,
        Some((
            Some(ProviderKind::OpenAiResponses),
            true,
            "sonnet".to_string()
        ))
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
    let visible = visible.unwrap();
    assert!(!visible.contains(&LoginField::ProfileSelector));
    assert!(!visible.contains(&LoginField::ProfileModel));
}

#[test]
#[serial]
fn profile_selector_changes_only_selector_and_not_active_alias() {
    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(
        state,
        LoginField::ProfileSelector,
        2,
        move |index, panel| {
            if index == 0 {
                let _ = panel.event(key(KeyCode::Right), TEST_AREA);
                let selected = panel.edit_state.read().as_ref().map(|state| {
                    (
                        state.selected().alias.to_string(),
                        state.active_alias.clone(),
                    )
                });
                // 既有绑定不可取消：Space 仅消费。
                *panel.edit_focus.write() = LoginField::ProfileBind;
                let _ = panel.event(key(KeyCode::Char(' ')), TEST_AREA);
                let bind = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .map(|state| (state.selected().alias.to_string(), state.selected().bind));
                tx.send((selected, bind)).unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );

    let (selected, bind) = rx.try_recv().unwrap();
    assert_eq!(
        selected,
        Some(("haiku".to_string(), "sonnet".to_string())),
        "切换子区档位不得调用 switch_active_alias"
    );
    assert_eq!(bind, Some(("haiku".to_string(), true)));
}

#[test]
#[serial]
fn invalid_profile_window_keeps_edit_and_focuses_the_field() {
    let cfg = login_config();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.kind = Some(ProviderKind::OpenAiResponses);
    state.provider_id = "new".into();
    state.api_key = "key".into();
    state.models.sonnet = "new-sonnet".into();
    state.selected_mut().window = "0".into();
    state.selected_mut().window_dirty = true;

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::Confirm, 2, move |index, panel| {
        if index == 0 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            let (mode, focus, error) = {
                let state = panel.edit_state.read();
                let state = state.as_ref().unwrap();
                (
                    *panel.mode.read(),
                    *panel.edit_focus.read(),
                    state
                        .inline_error
                        .as_ref()
                        .map(|error| (error.field, error.alias, error.message.clone())),
                )
            };
            tx.send((result, mode, focus, error)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (result, mode, focus, error) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Edit, "字段错误不得退出编辑");
    assert_eq!(focus, LoginField::ProfileWindow);
    let (field, alias, message) = error.expect("必须给出字段级错误");
    assert_eq!(field, LoginField::ProfileWindow);
    assert_eq!(alias, Some("sonnet"));
    assert!(message.contains("sonnet"), "{message}");
}

#[test]
#[serial]
fn typed_invalid_window_value_keeps_edit_and_focuses_the_field() {
    let cfg = login_config();
    let mut state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    state.selected_mut().window.clear();
    state.selected_mut().window_dirty = true;

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProfileWindow, 2, move |index, panel| {
        if index == 0 {
            for ch in ['1', '2', '8', 'k'] {
                let _ = panel.event(key(KeyCode::Char(ch)), TEST_AREA);
            }
            *panel.edit_focus.write() = LoginField::Confirm;
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            let (mode, focus, error) = {
                let state = panel.edit_state.read();
                let state = state.as_ref().unwrap();
                (
                    *panel.mode.read(),
                    *panel.edit_focus.read(),
                    state
                        .inline_error
                        .as_ref()
                        .map(|error| (error.field, error.alias)),
                )
            };
            tx.send((result, mode, focus, error)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (result, mode, focus, error) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Edit, "非法数值不得退出编辑");
    assert_eq!(focus, LoginField::ProfileWindow);
    assert_eq!(
        error,
        Some((LoginField::ProfileWindow, Some("sonnet"))),
        "直接输入的非法值必须定位到当前档位"
    );
}

#[test]
fn clean_paste_clears_the_single_line_error() {
    let cfg = login_config();
    let mut state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let mut cursor = 0;
    super::edit_handler::handle_login_paste(
        &mut cursor,
        &mut state,
        LoginField::ProfileWindow,
        "12\n34",
    );
    let error = state
        .inline_error
        .as_ref()
        .expect("含换行的粘贴必须给出单行提示");
    assert_eq!(error.field, LoginField::ProfileWindow);
    assert_eq!(error.message, crate::i18n::tr("login-error-single-line"));

    super::edit_handler::handle_login_paste(
        &mut cursor,
        &mut state,
        LoginField::ProfileWindow,
        "56",
    );
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
    use ratatui_kit::ratatui::style::Color;

    let cfg = login_config();
    let colors = LoginRenderColors {
        cursor: Color::Reset,
        dim: Color::Reset,
        text: Color::Reset,
        focus: Color::Reset,
    };

    let mut new_responses = LoginEditState::new_provider(&cfg, None);
    new_responses.kind = Some(ProviderKind::OpenAiResponses);
    new_responses.provider_id = "new".into();
    new_responses.api_key = "key".into();
    new_responses.models.sonnet = "new-sonnet".into();
    new_responses.selected_mut().window = "1000".into();
    new_responses.selected_mut().window_dirty = true;

    let mut new_openai = LoginEditState::new_provider(&cfg, None);
    new_openai.kind = Some(ProviderKind::OpenAi);
    new_openai.provider_id = "new".into();
    new_openai.api_key = "key".into();

    let mut plan = LoginEditState::edit_provider(&cfg, "plan", None).unwrap();
    plan.selected_mut().window = "4000".into();

    let responses = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();

    for state in [&new_responses, &new_openai, &plan, &responses] {
        let auth = super::auth::AuthController::new();
        for field in state.visible_fields() {
            let row = render::focus_row(state, &auth, field);
            let lines = render::build_edit_lines(state, &auth, field, 0, &colors);
            let marker_rows: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    line.spans
                        .first()
                        .is_some_and(|span| span.content.starts_with('❯'))
                })
                .map(|(index, _)| index)
                .collect();
            assert_eq!(
                marker_rows,
                vec![row],
                "field {field:?} focus row must match the rendered marker"
            );
            assert_eq!(lines.len(), render::line_count(state, &auth, field));
        }

        // 键盘可见字段与鼠标命中字段必须来自同一集合。
        let layout = render::build_edit_layout(state, &auth, LoginField::ProviderType, 0, &colors);
        let visible = state.visible_fields();
        for field in &visible {
            if *field == LoginField::AuthActions {
                assert!(
                    layout
                        .hits
                        .iter()
                        .any(|hit| matches!(hit, Some(render::EditHit::AuthAction(_)))),
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
fn keyboard_focus_scrolls_into_the_short_panel_viewport() {
    let cfg = login_config();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.kind = Some(ProviderKind::OpenAiResponses);
    state.provider_id = "new".into();
    state.api_key = "key".into();
    state.models.sonnet = "new-sonnet".into();
    state.selected_mut().window = "1000".into();
    state.selected_mut().window_dirty = true;

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
            let row = super::render::focus_row(&state, &auth, focus);
            let total = super::render::line_count(&state, &auth, focus);
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
