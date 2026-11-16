//! Paste validation regression scenarios.

use super::Arc;
use super::Event;
use super::Fixture;
use super::Future;
use super::KeyCode;
use super::LoginField;
use super::LoginPanelMode;
use super::NOTIFICATION;
use super::PROVIDER_LIST;

/// 多行粘贴被清洗后仍可能改变 provider 字段：错误未修正前不得落盘或推送 ACP。
#[test]
fn login_commit_multiline_provider_paste_blocks_save_until_corrected() {
    isolated(
        "login_commit_multiline_provider_paste_blocks_save_until_corrected",
        async {
            let fixture = Fixture::new(false);
            let mut state = super::new_responses_state(&fixture);
            state.base_url = "https://api.openai.com/v1/responses".into();
            let config = Arc::clone(&fixture.config);
            let target = fixture.target.clone();
            let before = config.read().clone();
            let disk_before = std::fs::read(&target).expect("login commit multiline provider paste blocks save until corrected: isolated fixture workspace must remain accessible");

            let (tx, rx) = std::sync::mpsc::channel();
            super::super::mount_edit_state(state, LoginField::BaseUrl, 2, move |index, panel| {
                if index != 0 {
                    return;
                }
                // 光标置于末尾：清洗后的 "7" 追加到 Base URL。
                let base_url = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .expect("login commit multiline provider paste blocks save until corrected: isolated fixture workspace must remain accessible")
                    .base_url
                    .chars()
                    .count();
                *panel.edit_cursor.write() = base_url;
                let paste_result = panel.event(Event::Paste("7\n".into()), super::super::TEST_AREA);
                let pasted = {
                    let guard = panel.edit_state.read();
                    let state = guard.as_ref().expect("login commit multiline provider paste blocks save until corrected: fixture must have the expected variant");
                    (
                        state.base_url.clone(),
                        state
                            .inline_error
                            .as_ref()
                            .map(|error| (error.field, error.message.clone())),
                    )
                };

                let save_result = panel.event(super::ctrl_s(), super::super::TEST_AREA);
                let blocked = {
                    let guard = panel.edit_state.read();
                    let state = guard.as_ref();
                    (
                        *panel.mode.read(),
                        *panel.edit_focus.read(),
                        state.and_then(|state| {
                            state
                                .inline_error
                                .as_ref()
                                .map(|error| error.message.clone())
                        }),
                        state.map(|state| state.base_url.clone()),
                    )
                };
                let handle_unchanged = *config.read() == before;
                let disk_unchanged = std::fs::read(&target).expect("login commit multiline provider paste blocks save until corrected: isolated fixture workspace must remain accessible") == disk_before;
                let listed_after_block = PROVIDER_LIST
                    .state()
                    .read()
                    .iter()
                    .any(|provider| provider.id == "new-responses");
                let notice = NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone());

                // 修正：删除粘贴进来的数字，错误随实际编辑清除。
                let _ = panel.event(
                    super::super::key(KeyCode::Backspace),
                    super::super::TEST_AREA,
                );
                let corrected = {
                    let guard = panel.edit_state.read();
                    let state = guard.as_ref().expect("login commit multiline provider paste blocks save until corrected: fixture must have the expected variant");
                    (state.base_url.clone(), state.inline_error.is_some())
                };
                let corrected_result = panel.event(super::ctrl_s(), super::super::TEST_AREA);
                let finished = (
                    *panel.mode.read(),
                    panel.edit_state.read().is_none(),
                    config.read().config.profiles.clone(),
                );
                let listed_after_save = PROVIDER_LIST
                    .state()
                    .read()
                    .iter()
                    .any(|provider| provider.id == "new-responses");
                tx.send((
                    paste_result,
                    pasted,
                    save_result,
                    blocked,
                    handle_unchanged,
                    disk_unchanged,
                    listed_after_block,
                    notice,
                    corrected,
                    corrected_result,
                    finished,
                    listed_after_save,
                ))
                .expect("login commit multiline provider paste blocks save until corrected: fixture channel must deliver its expected message");
            });

            let (
                paste_result,
                pasted,
                save_result,
                blocked,
                handle_unchanged,
                disk_unchanged,
                listed_after_block,
                notice,
                corrected,
                corrected_result,
                finished,
                listed_after_save,
            ) = rx.try_recv().expect("login commit multiline provider paste blocks save until corrected: fixture channel must deliver its expected message");
            let single_line = crate::i18n::tr("login-error-single-line");

            assert_eq!(paste_result, ratatui_kit::prelude::EventResult::Consumed);
            assert_eq!(
                pasted.0, "https://api.openai.com/v1/responses7",
                "清洗后的单行内容进入草稿"
            );
            assert_eq!(pasted.1, Some((LoginField::BaseUrl, single_line.clone())));
            assert_eq!(save_result, ratatui_kit::prelude::EventResult::Consumed);
            assert_eq!(
                blocked,
                (
                    LoginPanelMode::Edit,
                    LoginField::BaseUrl,
                    Some(single_line.clone()),
                    Some("https://api.openai.com/v1/responses7".to_string()),
                ),
                "错误未修正时必须保持编辑视图并聚焦出错字段"
            );
            assert!(handle_unchanged, "拒绝保存不得改动内存配置");
            assert!(disk_unchanged, "拒绝保存不得写盘");
            assert!(!listed_after_block, "拒绝保存不得发布 Provider 投影");
            assert_eq!(
                notice,
                Some(single_line.clone()),
                "拒绝保存必须显示既有字段错误"
            );

            assert_eq!(
                corrected,
                ("https://api.openai.com/v1/responses".to_string(), false)
            );
            assert_eq!(
                corrected_result,
                ratatui_kit::prelude::EventResult::Consumed
            );
            assert_eq!(
                finished,
                (
                    LoginPanelMode::Browse,
                    true,
                    super::initial_config().config.profiles,
                ),
                "修正后保存成功并退出编辑，且不改动任何档位"
            );
            assert!(listed_after_save, "成功保存后发布 Provider 投影");

            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

fn isolated(name: &str, scenario: impl Future<Output = ()>) {
    let module = module_path!()
        .split_once("::")
        .expect("the test module has a crate prefix")
        .1;
    super::run_isolated(name, module, scenario);
}
