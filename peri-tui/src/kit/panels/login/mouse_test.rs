//! Login Edit/类型确认/认证动作的鼠标命中回归。

use super::*;

// ── 鼠标命中：Edit 动态字段 / choice / 类型确认 ─────────────────────────────

#[test]
#[serial]
fn mouse_click_focuses_every_visible_text_field() {
    let state = new_responses_mouse_state();
    let fields: Vec<LoginField> = state
        .visible_fields()
        .into_iter()
        .filter(|field| field.is_text())
        .collect();
    assert!(fields.len() >= 6, "动态文本字段必须全部覆盖");
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 1, move |index, panel| {
        if index == 0 {
            let area = tall_area();
            let mut observed = Vec::new();
            for field in &fields {
                let row = edit_hit_row(&panel, super::render::EditHit::Field(*field), area);
                let result = panel.event(click_at(row, 3), area);
                observed.push((*field, result, *panel.edit_focus.read()));
            }
            tx.send(observed).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    for (field, result, focus) in rx.try_recv().unwrap() {
        assert_eq!(result, EventResult::Consumed, "字段 {field:?} 点击必须消费");
        assert_eq!(focus, field, "点击文本行必须聚焦该字段");
    }
}

#[test]
#[serial]
fn mouse_click_focuses_plan_text_fields() {
    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "plan", None).unwrap();
    let fields: Vec<LoginField> = state
        .visible_fields()
        .into_iter()
        .filter(|field| field.is_text())
        .collect();
    assert!(
        fields.contains(&LoginField::PlanStore) && fields.contains(&LoginField::PlanAccount),
        "plan 模式的动态字段必须纳入覆盖"
    );
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::AuthMode, 1, move |index, panel| {
        if index == 0 {
            let area = tall_area();
            let mut observed = Vec::new();
            for field in &fields {
                let row = edit_hit_row(&panel, super::render::EditHit::Field(*field), area);
                let result = panel.event(click_at(row, 3), area);
                observed.push((*field, result, *panel.edit_focus.read()));
            }
            tx.send(observed).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    for (field, result, focus) in rx.try_recv().unwrap() {
        assert_eq!(result, EventResult::Consumed, "字段 {field:?} 点击必须消费");
        assert_eq!(focus, field, "点击 plan 文本行必须聚焦该字段");
    }
}

#[test]
#[serial]
fn mouse_click_choices_match_keyboard_actions() {
    let base = new_responses_mouse_state();
    let area = tall_area();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(
        base.clone(),
        LoginField::ProviderType,
        1,
        move |index, panel| {
            if index == 0 {
                let reset = |panel: &MountedPanel| {
                    *panel.edit_state.write() = Some(base.clone());
                    *panel.mode.write() = LoginPanelMode::Edit;
                    *panel.edit_focus.write() = LoginField::ProviderType;
                    *panel.edit_cursor.write() = 0;
                    *panel.auth.write() = super::auth::AuthController::new();
                    panel.sv.write().set_offset(Position::new(0, 0));
                };

                // 类型：鼠标点击与键盘 Enter 都只进入显式确认/循环，不静默改值。
                reset(&panel);
                *panel.edit_focus.write() = LoginField::ProviderType;
                let row = edit_hit_row(
                    &panel,
                    super::render::EditHit::Field(LoginField::ProviderType),
                    area,
                );
                let _ = panel.event(click_at(row, 3), area);
                let mouse_type = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .map(|state| (state.kind, state.type_change_candidate, *panel.mode.read()));
                reset(&panel);
                *panel.edit_focus.write() = LoginField::ProviderType;
                let _ = panel.event(key(KeyCode::Enter), area);
                let key_type = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .map(|state| (state.kind, state.type_change_candidate, *panel.mode.read()));
                assert_eq!(mouse_type, key_type, "类型点击与 Enter 必须同一语义");

                // 认证方式：鼠标 Enter 等价动作。
                reset(&panel);
                *panel.edit_focus.write() = LoginField::AuthMode;
                let row = edit_hit_row(
                    &panel,
                    super::render::EditHit::Field(LoginField::AuthMode),
                    area,
                );
                let _ = panel.event(click_at(row, 3), area);
                let mouse_auth = panel.edit_state.read().as_ref().map(|state| {
                    (
                        state.auth_choice,
                        state.plan.store.clone(),
                        state.plan.account.clone(),
                        *panel.edit_focus.read(),
                        *panel.mode.read(),
                    )
                });
                reset(&panel);
                *panel.edit_focus.write() = LoginField::AuthMode;
                let _ = panel.event(key(KeyCode::Enter), area);
                let key_auth = panel.edit_state.read().as_ref().map(|state| {
                    (
                        state.auth_choice,
                        state.plan.store.clone(),
                        state.plan.account.clone(),
                        *panel.edit_focus.read(),
                        *panel.mode.read(),
                    )
                });
                assert_eq!(mouse_auth, key_auth, "认证方式点击与 Enter 必须同一语义");

                // 档位选择器：键盘 Space 与鼠标点击都前移一档。
                reset(&panel);
                *panel.edit_focus.write() = LoginField::ProfileSelector;
                let row = edit_hit_row(
                    &panel,
                    super::render::EditHit::Field(LoginField::ProfileSelector),
                    area,
                );
                let _ = panel.event(click_at(row, 3), area);
                let mouse_profile =
                    panel.edit_state.read().as_ref().map(|state| {
                        (state.selected().alias.to_string(), *panel.edit_focus.read())
                    });
                reset(&panel);
                *panel.edit_focus.write() = LoginField::ProfileSelector;
                let _ = panel.event(key(KeyCode::Char(' ')), area);
                let key_profile =
                    panel.edit_state.read().as_ref().map(|state| {
                        (state.selected().alias.to_string(), *panel.edit_focus.read())
                    });
                assert_eq!(mouse_profile, key_profile, "档位点击与 Space 必须同一语义");

                // 绑定复选框：点击与 Space 都只切换当前档。
                reset(&panel);
                *panel.edit_focus.write() = LoginField::ProfileBind;
                let row = edit_hit_row(
                    &panel,
                    super::render::EditHit::Field(LoginField::ProfileBind),
                    area,
                );
                let _ = panel.event(click_at(row, 3), area);
                let mouse_bind = panel.edit_state.read().as_ref().map(|state| {
                    (
                        state.selected().alias.to_string(),
                        state.selected().bind,
                        *panel.edit_focus.read(),
                    )
                });
                reset(&panel);
                *panel.edit_focus.write() = LoginField::ProfileBind;
                let _ = panel.event(key(KeyCode::Char(' ')), area);
                let key_bind = panel.edit_state.read().as_ref().map(|state| {
                    (
                        state.selected().alias.to_string(),
                        state.selected().bind,
                        *panel.edit_focus.read(),
                    )
                });
                assert_eq!(mouse_bind, key_bind, "绑定点击与 Space 必须同一语义");

                // Confirm：鼠标点击与 Enter 走同一保存校验（无 authority 时同样保留草稿）。
                reset(&panel);
                *panel.edit_focus.write() = LoginField::Confirm;
                let row = edit_hit_row(
                    &panel,
                    super::render::EditHit::Field(LoginField::Confirm),
                    area,
                );
                let _ = panel.event(click_at(row, 3), area);
                let mouse_confirm = panel.edit_state.read().as_ref().map(|state| {
                    (
                        *panel.mode.read(),
                        state
                            .inline_error
                            .as_ref()
                            .map(|error| (error.field, error.kind, error.message.clone())),
                    )
                });
                reset(&panel);
                *panel.edit_focus.write() = LoginField::Confirm;
                let _ = panel.event(key(KeyCode::Enter), area);
                let key_confirm = panel.edit_state.read().as_ref().map(|state| {
                    (
                        *panel.mode.read(),
                        state
                            .inline_error
                            .as_ref()
                            .map(|error| (error.field, error.kind, error.message.clone())),
                    )
                });
                assert_eq!(
                    mouse_confirm, key_confirm,
                    "Confirm 点击与 Enter 必须走同一保存校验"
                );

                tx.send(()).unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );
    rx.try_recv().expect("鼠标/键盘对比必须完成");
}

#[test]
#[serial]
fn mouse_click_locked_type_requires_explicit_confirmation() {
    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let area = tall_area();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 1, move |index, panel| {
        if index == 0 {
            let row = edit_hit_row(
                &panel,
                super::render::EditHit::Field(LoginField::ProviderType),
                area,
            );
            let _ = panel.event(click_at(row, 3), area);
            let after_click = (
                *panel.mode.read(),
                panel.edit_state.read().as_ref().map(|state| state.kind),
            );

            let candidate_row =
                type_change_semantic_row(&panel, super::render::EditHit::TypeChangeCandidate)
                    as u16;
            let _ = panel.event(click_at(area.y + 1 + candidate_row, 3), area);
            let candidate = panel
                .edit_state
                .read()
                .as_ref()
                .and_then(|state| state.type_change_candidate);

            let confirm_row =
                type_change_semantic_row(&panel, super::render::EditHit::TypeChangeConfirm) as u16;
            let _ = panel.event(click_at(area.y + 1 + confirm_row, 3), area);
            let after_confirm = (
                *panel.mode.read(),
                panel.edit_state.read().as_ref().map(|state| state.kind),
            );
            tx.send((after_click, candidate, after_confirm)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (after_click, candidate, after_confirm) = rx.try_recv().unwrap();
    assert_eq!(
        after_click,
        (
            LoginPanelMode::ConfirmTypeChange,
            Some(Some(ProviderKind::OpenAiResponses))
        ),
        "点击已保存 provider 的类型行只能进入显式确认，不能直接改值"
    );
    assert_eq!(candidate, Some(ProviderKind::Anthropic));
    assert_eq!(
        after_confirm,
        (LoginPanelMode::Edit, Some(Some(ProviderKind::Anthropic))),
        "点击确认提示后才应用候选类型"
    );
}

#[test]
#[serial]
fn mouse_click_blank_border_and_scrollbar_leave_edit_state_unchanged() {
    let state = new_responses_mouse_state();
    let area = tall_area();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderId, 1, move |index, panel| {
        if index == 0 {
            let snapshot = |panel: &MountedPanel| {
                let state = panel.edit_state.read();
                let state = state.as_ref().expect("草稿必须保留");
                (
                    *panel.mode.read(),
                    *panel.edit_focus.read(),
                    state.kind,
                    state.provider_id.clone(),
                    state.auth_choice,
                    state.selected().alias.to_string(),
                    state.selected().bind,
                    state
                        .inline_error
                        .as_ref()
                        .map(|error| error.message.clone()),
                )
            };
            let before = snapshot(&panel);
            let layout = edit_layout(&panel);
            let blank_row = layout
                .hits
                .iter()
                .position(|hit| hit.is_none())
                .expect("布局中存在不可点击行") as u16;
            let hit_row = edit_hit_row(
                &panel,
                super::render::EditHit::Field(LoginField::ProviderId),
                area,
            );
            let targets = [
                (area.y, 3),                        // 顶部边框
                (area.y + 1 + blank_row, 3),        // 标题/空白
                (area.y + area.height - 1, 3),      // 底部边框
                (hit_row, area.x + area.width - 1), // 滚动条列
            ];
            for (row, column) in targets {
                let result = panel.event(click_at(row, column), area);
                assert_eq!(result, EventResult::Consumed, "区域内点击仍消费防穿透");
            }
            tx.send((before, snapshot(&panel))).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (before, after) = rx.try_recv().unwrap();
    assert_eq!(before, after, "空白/边框/滚动条不得改变草稿或焦点");
}

#[test]
#[serial]
fn mouse_click_after_scroll_hits_the_visible_semantic_row() {
    let state = new_responses_mouse_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::ProviderType, 1, move |index, panel| {
        if index == 0 {
            let row =
                hit_semantic_row(&panel, super::render::EditHit::Field(LoginField::OpusModel));
            let offset = row.saturating_sub(1) as u16;
            panel.sv.write().set_offset(Position::new(0, offset));
            let screen_row = area.y + 1 + row as u16 - offset;
            let result = panel.event(click_at(screen_row, 3), area);
            tx.send((result, *panel.edit_focus.read(), offset)).unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (result, focus, offset) = rx.try_recv().unwrap();
    assert!(offset > 0, "前置条件：视口必须已滚动");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(focus, LoginField::OpusModel, "滚动后点击仍命中同一语义行");
}

#[test]
#[serial]
fn mouse_click_auth_action_executes_selected_action() {
    let cfg = login_config();
    let state = LoginEditState::edit_provider(&cfg, "plan", None).unwrap();
    let area = tall_area();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state(state, LoginField::AuthActions, 1, move |index, panel| {
        if index == 0 {
            let row = edit_hit_row(
                &panel,
                super::render::EditHit::AuthAction(super::auth::AuthAction::Logout),
                area,
            );
            let result = panel.event(click_at(row, 3), area);
            tx.send((
                result,
                *panel.mode.read(),
                *panel.edit_focus.read(),
                panel
                    .auth
                    .read()
                    .logout_confirm()
                    .map(|confirm| confirm.target.account.clone()),
            ))
            .unwrap();
        }
        frame += 1;
        assert_eq!(frame, index + 1);
    });

    let (result, mode, focus, target) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        mode,
        LoginPanelMode::ConfirmLogout,
        "点击动作行必须执行该动作"
    );
    assert_eq!(focus, LoginField::AuthActions);
    assert_eq!(
        target.as_deref(),
        Some("work"),
        "注销确认绑定当前 plan 目标"
    );
}
