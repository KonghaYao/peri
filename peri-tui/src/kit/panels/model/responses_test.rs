//! Responses provider 在 Model 面板与其它 provider 共用同一右栏与按键路径的回归。

use super::*;

#[test]
#[serial]
fn subscription_default_window_is_visible_and_prefilled_without_persisting() {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    config.config.providers[0].api_key.clear();
    config.config.providers[0].auth = Some(
        peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
            store: std::env::temp_dir().join("peri-model-plan"),
            account: "work".into(),
            subject: "subject".into(),
            client_id: "client".into(),
        },
    );
    config.config.providers[0].models.sonnet = "gpt-6.1-sol".into();
    config.config.profiles.sonnet.context_window = None;
    assert_eq!(effective_context_window(&config, "sonnet"), Some(1_000_000));
    guard.set_config(config);
    let handle = Arc::clone(&guard.handle);
    let (sender, receiver) = std::sync::mpsc::channel();
    let wide_area = Rect {
        width: 110,
        ..TEST_AREA
    };
    let buffer = mount_panel_frames(wide_area, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_CONTEXT_WINDOW;
        panel.event(key(KeyCode::Enter), wide_area, &switched());
        sender.send(panel.window_edit.read().clone()).unwrap();
    });
    assert!(
        buffer_text(&buffer).contains("1000000"),
        "{}",
        buffer_text(&buffer)
    );
    assert_eq!(receiver.recv().unwrap().as_deref(), Some("1000000"));
    assert_eq!(handle.read().config.profiles.sonnet.context_window, None);
}

#[test]
#[serial]
fn rendered_capacities_and_active_selection_are_consistent() {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    config.config.profiles.sonnet.context_window = Some(128_000);
    config.config.profiles.opus.provider = "other".into();
    config.config.profiles.opus.context_window = Some(128_000);
    config.config.profiles.opus.context_1m = true;
    config.config.profiles.haiku.context_window = None;
    guard.set_config(config);
    let area = Rect {
        width: 110,
        ..TEST_AREA
    };
    let buffer = mount_panel_frames(area, 2, |_, _| {});
    let summary = |row: u16| -> String {
        (0..area.width * 45 / 100)
            .map(|x| buffer[(x, row + 2)].symbol())
            .collect()
    };
    let (sonnet_row, _) = rendered_line_of(&buffer, "sonnet ·");
    let (opus_row, _) = rendered_line_of(&buffer, "opus ·");
    let (haiku_row, _) = rendered_line_of(&buffer, "haiku ·");
    assert!(summary(sonnet_row).contains("128k"));
    assert!(summary(opus_row).contains("1m"));
    assert!(summary(haiku_row).contains('—'));
    assert!(
        rendered_line_of(&buffer, "Context window")
            .1
            .contains("128000")
    );
    assert!(buffer_text(&buffer).contains(&i18n::tr("model-panel-selection-legend")));

    let one_million = mount_panel_frames(area, 3, move |frame, panel| {
        if frame == 1 {
            panel.event(left_down(opus_row, 5), area, &switched());
        }
    });
    assert!(
        rendered_line_of(&one_million, "Context window")
            .1
            .contains("1000000")
    );
    let buffer = mount_panel_frames(area, 3, move |frame, panel| {
        if frame == 1 {
            panel.event(left_down(haiku_row, 5), area, &switched());
        }
    });
    let (_, active) = rendered_line_of(&buffer, "sonnet ·");
    let (_, selected) = rendered_line_of(&buffer, "haiku ·");
    assert!(active.trim_start().starts_with("● "));
    assert!(selected.trim_start().starts_with("○❯"));
    assert!(rendered_line_of(&buffer, "Context window").1.contains('—'));
}

/// 所有 provider 共用 Context window 与 1m enable 行。
#[test]
#[serial]
fn responses_context_row_keeps_the_generic_kv_pane() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let switched = switched();
    let (tx, rx) = std::sync::mpsc::channel();

    let buffer = mount_panel_frames(TEST_AREA, 2, move |frame, panel| {
        if frame == 0 {
            *panel.right_focus.write() = true;
            *panel.right_cursor.write() = FIELD_CONTEXT_1M;
            let result = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            tx.send(result).unwrap();
        }
    });

    assert_eq!(rx.try_recv().unwrap(), EventResult::Consumed);
    let text = buffer_text(&buffer);
    for label in [
        "Provider",
        "Model",
        "Effort",
        "Max tokens",
        "Context window",
        "1m enable",
    ] {
        assert!(
            text.contains(label),
            "右栏必须保持统一 K/V 行 {label}:\n{text}"
        );
    }
    assert!(
        !text.contains("[保存]") && !text.contains("[取消]"),
        "不得再出现专属保存/取消按钮:\n{text}"
    );
    assert!(!text.contains("正在编辑"), "不得再出现专属标题:\n{text}");
}

#[test]
#[serial]
fn context_window_editor_accepts_digits_and_escape_discards_changes() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let (sender, receiver) = std::sync::mpsc::channel();
    mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        *panel.right_focus.write() = true;
        *panel.right_cursor.write() = FIELD_CONTEXT_WINDOW;
        assert_eq!(
            panel.event(key(KeyCode::Enter), TEST_AREA, &switched()),
            EventResult::Consumed
        );
        assert_eq!(panel.window_edit.read().as_deref(), Some("3000"));
        panel.event(Event::Paste("128".into()), TEST_AREA, &switched());
        panel.event(Event::Paste("bad".into()), TEST_AREA, &switched());
        assert_eq!(panel.window_edit.read().as_deref(), Some("3000128"));
        panel.event(key(KeyCode::Esc), TEST_AREA, &switched());
        sender
            .send((
                panel.window_edit.read().is_none(),
                handle.read().config.profiles.sonnet.context_window,
            ))
            .unwrap();
    });
    assert_eq!(receiver.recv().unwrap(), (true, Some(3000)));
}

fn max_tokens_after_right(plan_auth: bool) -> (u32, Option<String>) {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    if plan_auth {
        config.config.providers[0].api_key.clear();
        config.config.providers[0].auth = Some(
            peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
                store: std::env::temp_dir().join("peri-model-plan"),
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
fn plan_provider_renders_profile_max_tokens_like_other_providers() {
    let guard = ModelPanelGuard::install();
    let mut config = responses_config();
    config.config.providers[0].api_key.clear();
    config.config.providers[0].auth = Some(
        peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
            store: std::env::temp_dir().join("peri-model-plan"),
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
