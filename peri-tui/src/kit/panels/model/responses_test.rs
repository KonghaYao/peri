//! Responses contextWindow 内联编辑与档位草稿的键盘回归。

use super::*;

#[test]
#[serial]
fn responses_context_enter_opens_numeric_editor_and_escape_restores() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let captured = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_CONTEXT_1M;
        let opened = panel.event(key(KeyCode::Enter), TEST_AREA, &captured);
        let alias = panel
            .window_editor
            .read()
            .as_ref()
            .map(|editor| editor.alias.clone());
        let before = handle.read().config.clone();

        // 输入数字；非数字被拒绝且不写入。
        let _ = panel.event(key(KeyCode::Char('9')), TEST_AREA, &captured);
        let _ = panel.event(key(KeyCode::Char('k')), TEST_AREA, &captured);
        let text = panel
            .window_editor
            .read()
            .as_ref()
            .map(|editor| editor.text.clone());

        // 没有配置 authority 时 Enter 不能发布任何变更，editor 保留错误。
        let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &captured);
        let after = handle.read().config.clone();
        let error = panel
            .window_editor
            .read()
            .as_ref()
            .and_then(|editor| editor.error.clone());

        // Esc 只关闭 editor，不改 active alias。
        let closed = panel.event(key(KeyCode::Esc), TEST_AREA, &captured);
        tx.send((
            opened,
            alias,
            text,
            before,
            after,
            error,
            closed,
            panel.window_editor.read().is_none(),
        ))
        .unwrap();
    });

    let (opened, alias, text, before, after, error, closed, editor_closed) = rx.try_recv().unwrap();
    assert_eq!(opened, EventResult::Consumed);
    assert_eq!(alias.as_deref(), Some("sonnet"));
    assert_eq!(text.as_deref(), Some("30009"));
    assert_eq!(
        error.as_deref(),
        Some(i18n::tr("model-panel-window-invalid").as_str()),
        "被拒绝的输入必须阻断提交，不能被后续保存失败覆盖"
    );
    assert_eq!(before, after, "失败不得发布或改动内存配置");
    assert_eq!(closed, EventResult::Consumed);
    assert!(editor_closed, "Esc 关闭 editor 返回右栏");
    assert!(
        switched.borrow().is_none(),
        "editor 打开期间方向键/字符不得切换 active alias"
    );
}

#[test]
#[serial]
fn responses_provider_or_model_change_opens_profile_draft_without_publishing() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let captured = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        let before = handle.read().config.clone();

        *panel.right_cursor.write() = FIELD_PROVIDER;
        let provider_result = panel.event(key(KeyCode::Right), TEST_AREA, &captured);
        let provider_draft = panel
            .profile_draft
            .read()
            .as_ref()
            .map(|draft| (draft.provider_id.clone(), draft.text.clone()));
        let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &captured);

        *panel.right_cursor.write() = FIELD_MODEL;
        let model_result = panel.event(key(KeyCode::Right), TEST_AREA, &captured);
        let model_draft = panel
            .profile_draft
            .read()
            .as_ref()
            .map(|draft| (draft.model.clone(), draft.text.clone()));
        let after = handle.read().config.clone();
        let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &captured);

        tx.send((
            provider_result,
            provider_draft,
            model_result,
            model_draft,
            before,
            after,
            panel.profile_draft.read().is_none(),
        ))
        .unwrap();
    });

    let (provider_result, provider_draft, model_result, model_draft, before, after, closed) =
        rx.try_recv().unwrap();
    assert_eq!(provider_result, EventResult::Consumed);
    assert_eq!(
        provider_draft,
        Some(("other".to_string(), String::new())),
        "更换到非 Responses provider 也必须先进入草稿且不自动继承旧容量"
    );
    assert_eq!(model_result, EventResult::Consumed);
    assert_eq!(
        model_draft,
        Some(("haiku-model".to_string(), String::new())),
        "模型切换后的容量必须重新填写"
    );
    assert_eq!(before, after, "草稿候选不得在保存前发布");
    assert!(closed, "Esc 放弃草稿");
    assert!(switched.borrow().is_none(), "草稿期间不得切换 active alias");
}

#[test]
#[serial]
fn plan_max_tokens_keeps_server_default_hint_without_writing() {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    config.config.providers[0].api_key.clear();
    config.config.providers[0].auth = Some(
        peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
            store: "/tmp/peri-model-plan".into(),
            account: "work".into(),
            subject: "subject".into(),
            client_id: "client".into(),
        },
    );
    guard.set_config(config.clone());
    let handle = Arc::clone(&guard.handle);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_MAX_TOKENS;
        let result = panel.event(key(KeyCode::Right), TEST_AREA, &switched());
        tx.send((
            result,
            handle.read().clone(),
            NOTIFICATION
                .state()
                .read()
                .as_ref()
                .map(|note| note.message.clone()),
        ))
        .unwrap();
    });

    let (result, after, notice) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(after, config, "plan max_tokens 不得写入或改变配置");
    assert_eq!(notice, Some(i18n::tr("model-panel-plan-output-hint")));
}

#[test]
#[serial]
fn non_responses_context_field_keeps_1m_toggle_path() {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    config.config.providers[0].provider_type = "openai".into();
    guard.set_config(config);
    let handle = Arc::clone(&guard.handle);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_CONTEXT_1M;
        let before = handle.read().config.clone();
        let result = panel.event(key(KeyCode::Right), TEST_AREA, &switched());
        tx.send((
            result,
            before,
            handle.read().config.clone(),
            panel.window_editor.read().is_none(),
        ))
        .unwrap();
    });

    let (result, before, after, no_editor) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert!(no_editor, "非 Responses 不进入数值 editor");
    assert_eq!(
        before, after,
        "即时 1m 保存缺少 authority 时不得改内存（失败路径由 commit_test 覆盖）"
    );
}

/// 设计 §7.6：Enter 进入任意 model 文本编辑；Enter 确认，Esc 放弃。
#[test]
#[serial]
fn responses_draft_model_enter_edits_arbitrary_text_and_escape_restores() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let captured = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_MODEL;
        let _ = panel.event(key(KeyCode::Right), TEST_AREA, &captured);
        {
            let mut draft = panel.profile_draft.write();
            let draft = draft.as_mut().expect("Responses model 行必须进入档位草稿");
            draft.focus = DraftFocus::Model;
            draft.model.clear();
            draft.model_cursor = 0;
        }
        let before = handle.read().config.clone();

        let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &captured);
        let editing = panel
            .profile_draft
            .read()
            .as_ref()
            .map(|draft| (draft.model_editing, draft.focus));
        for ch in "gpt-9-custom high".chars() {
            let _ = panel.event(key(KeyCode::Char(ch)), TEST_AREA, &captured);
        }
        let typed = panel
            .profile_draft
            .read()
            .as_ref()
            .map(|draft| draft.model.clone());

        let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &captured);
        let confirmed = panel
            .profile_draft
            .read()
            .as_ref()
            .map(|draft| (draft.model.clone(), draft.model_editing, draft.focus));

        // 再次编辑后 Esc：恢复确认后的文本，不关闭草稿。
        {
            let mut draft = panel.profile_draft.write();
            draft.as_mut().unwrap().focus = DraftFocus::Model;
        }
        let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &captured);
        for ch in "junk".chars() {
            let _ = panel.event(key(KeyCode::Char(ch)), TEST_AREA, &captured);
        }
        let esc = panel.event(key(KeyCode::Esc), TEST_AREA, &captured);
        let after_esc = panel
            .profile_draft
            .read()
            .as_ref()
            .map(|draft| (draft.model.clone(), draft.model_editing));
        let after = handle.read().config.clone();

        tx.send((
            editing,
            typed,
            confirmed,
            esc,
            after_esc,
            panel.profile_draft.read().is_some(),
            before,
            after,
        ))
        .unwrap();
    });

    let (editing, typed, confirmed, esc, after_esc, still_open, before, after) =
        rx.try_recv().unwrap();
    assert_eq!(editing, Some((true, DraftFocus::Model)));
    assert_eq!(typed.as_deref(), Some("gpt-9-custom high"));
    assert_eq!(
        confirmed,
        Some(("gpt-9-custom high".to_string(), false, DraftFocus::Save)),
        "Enter 确认文本后进入 Save，仍与容量一起原子保存"
    );
    assert_eq!(esc, EventResult::Consumed);
    assert_eq!(
        after_esc,
        Some(("gpt-9-custom high".to_string(), false)),
        "Esc 放弃本次文本编辑并恢复确认值"
    );
    assert!(still_open, "Esc 只退出文本编辑，不丢弃整个草稿");
    assert_eq!(before, after, "文本编辑与放弃都不得发布配置");
}
