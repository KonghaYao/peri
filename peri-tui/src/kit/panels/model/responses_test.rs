//! Responses contextWindow 内联编辑与共享右栏路径的键盘回归。

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

/// 切到缺 contextWindow 的 Responses provider 时，先进入窗口编辑且不落盘；
/// 取消则 provider/model 也不变，避免“先切 provider 再保存失败”的半截状态。
#[test]
#[serial]
fn switching_to_responses_without_window_opens_window_editor_atomically() {
    let guard = ModelPanelGuard::install();
    let mut cfg = responses_config();
    cfg.config.profiles.sonnet.provider = "other".into();
    cfg.config.profiles.sonnet.context_window = None;
    guard.set_config(cfg);
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let before = handle.read().config.clone();
    let expected_before = before.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_PROVIDER;
        let result = panel.event(key(KeyCode::Right), TEST_AREA, &switched);
        let editor = panel
            .window_editor
            .read()
            .as_ref()
            .map(|editor| (editor.alias.clone(), editor.text.clone()));
        tx.send((result, editor, before.clone(), handle.read().config.clone()))
            .unwrap();
    });

    let (result, editor, before, after) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        editor,
        Some(("sonnet".to_string(), String::new())),
        "切到缺窗口的 Responses provider 必须先补窗口"
    );
    assert_eq!(before, expected_before);
    assert_eq!(
        after, expected_before,
        "editor 打开期间不得落盘 provider/model"
    );
}

/// ChatGPT plan / Responses 与其它 provider 共用右栏 K/V 路径：Provider / Model
/// 行即时编辑，不再出现“档位配置草稿”面板。
#[test]
#[serial]
fn responses_provider_and_model_rows_use_the_shared_inline_path() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let captured = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    let buffer = mount_panel_frames(TEST_AREA, 2, move |_, panel| {
        *panel.right_focus.write() = true;
        let before = handle.read().config.clone();

        *panel.right_cursor.write() = FIELD_PROVIDER;
        let provider = panel.event(key(KeyCode::Right), TEST_AREA, &captured);

        *panel.right_cursor.write() = FIELD_MODEL;
        let model = panel.event(key(KeyCode::Right), TEST_AREA, &captured);

        tx.send((
            provider,
            model,
            panel.window_editor.read().is_none(),
            before,
            handle.read().config.clone(),
        ))
        .unwrap();
    });

    let (provider, model, no_editor, before, after) = rx.try_recv().unwrap();
    assert_eq!(provider, EventResult::Consumed);
    assert_eq!(model, EventResult::Consumed);
    assert!(no_editor, "Provider/Model 行不得打开任何编辑面板");
    assert_eq!(before, after, "无 authority 时不得发布未落盘的切换");

    let text = buffer_text(&buffer);
    assert!(
        !text.contains("档位配置草稿") && !text.contains("Profile draft"),
        "右栏不得再出现档位配置草稿:\n{text}"
    );
}

fn max_tokens_after_right(plan_auth: bool) -> (u32, Option<String>) {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    if plan_auth {
        config.config.providers[0].api_key.clear();
        config.config.providers[0].auth = Some(
            peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
                store: "/tmp/peri-model-plan".into(),
                account: "work".into(),
                subject: "subject".into(),
                client_id: "client".into(),
            },
        );
    }
    guard.set_config(config);
    let handle = Arc::clone(&guard.handle);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_MAX_TOKENS;
        let result = panel.event(key(KeyCode::Right), TEST_AREA, &switched());
        tx.send((
            result,
            handle.read().config.profiles.sonnet.max_tokens,
            NOTIFICATION
                .state()
                .read()
                .as_ref()
                .map(|note| note.message.clone()),
        ))
        .unwrap();
    });

    let (result, max_tokens, notice) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    (max_tokens, notice)
}

#[test]
#[serial]
fn plan_provider_max_tokens_matches_generic_responses_provider() {
    let plan = max_tokens_after_right(true);
    let plain = max_tokens_after_right(false);
    assert_eq!(
        plan, plain,
        "ChatGPT plan provider 在 Model 面板必须走通用 Max tokens 路径，无订阅特例"
    );
    assert!(
        !plan
            .1
            .as_deref()
            .unwrap_or_default()
            .contains("model-panel-plan-output-hint"),
        "不得再出现订阅专属的 Max tokens 提示"
    );
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

#[test]
#[serial]
fn plan_provider_renders_profile_max_tokens_like_other_providers() {
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
    guard.set_config(config);

    let buffer = mount_panel_frames(TEST_AREA, 2, |_, _| {});
    let text = buffer_text(&buffer);
    assert!(
        text.contains("32000"),
        "plan provider 的 Max tokens 必须显示实际 profile 值:\n{text}"
    );
    assert!(
        !text.contains("Server default") && !text.contains("服务器默认"),
        "不得为订阅 provider 显示特殊占位:\n{text}"
    );
}
