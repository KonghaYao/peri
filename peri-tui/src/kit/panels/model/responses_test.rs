//! Responses provider 在 Model 面板与其它 provider 共用同一右栏与按键路径的回归。

use super::*;

/// 第五行对所有 provider 都是通用 `1m enable`：Enter 不打开专属面板，
/// 右栏保持同一张 K/V 表。
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
    for label in ["Provider", "Model", "Effort", "Max tokens", "1m enable"] {
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
    assert!(
        !text.contains("上下文窗口"),
        "第五行必须是通用 1m enable:\n{text}"
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
