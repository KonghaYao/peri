//! 身份与作用域回归：复用真实挂载，接缝仅截取请求，避免全局 ACP OnceLock 干扰。
use super::*;
use operation::{OperationState, PluginOperation, tests::mount_panel};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serial_test::serial;
use std::cell::RefCell;

thread_local! {
    static REQUESTS: RefCell<Option<Vec<PluginOperation>>> = const { RefCell::new(None) };
}

pub(super) fn intercept_dispatch(request: &PluginOperation) -> bool {
    REQUESTS.with(|slot| {
        if let Some(requests) = slot.borrow_mut().as_mut() {
            requests.push(request.clone());
            true
        } else {
            false
        }
    })
}

struct Guard {
    plugins: Vec<PluginSummary>,
    session: String,
    reset: u64,
}

impl Guard {
    fn new() -> Self {
        REQUESTS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
        Self {
            plugins: PLUGIN_LIST.state().read().clone(),
            session: ACTIVE_SESSION_ID.state().read().clone(),
            reset: BRIDGE_RESET_COUNTER.get(),
        }
    }

    fn requests() -> Vec<PluginOperation> {
        REQUESTS.with(|slot| slot.borrow().clone().unwrap())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        *PLUGIN_LIST.state().write() = self.plugins.clone();
        *ACTIVE_SESSION_ID.state().write() = self.session.clone();
        BRIDGE_RESET_COUNTER.set(self.reset);
        REQUESTS.with(|slot| *slot.borrow_mut() = None);
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn plugin(name: &str, marketplace: &str, scope: &str) -> PluginSummary {
    PluginSummary {
        name: name.into(),
        marketplace: marketplace.into(),
        install_scope: Some(scope.into()),
        toggle_supported: Some(true),
        ..Default::default()
    }
}

fn marketplace(name: &str, source: &str) -> MsEntry {
    MsEntry {
        name: name.into(),
        source: peri_middlewares::plugin::MarketplaceSource::Directory {
            path: source.into(),
        },
        source_label: source.into(),
        plugin_count: 0,
        installed_count: 0,
        status: MsStatus::Cached,
        last_updated: None,
        auto_update: false,
    }
}

#[test]
#[serial]
fn installed_detail_and_action_follow_identity_after_reorder() {
    let _guard = Guard::new();
    for scope in ["user", "project", "local"] {
        let wanted = plugin("compiler", "tools", scope);
        *PLUGIN_LIST.state().write() = vec![wanted.clone(), plugin("other", "tools", scope)];
        mount_panel(move |panel| {
            panel.event(key(KeyCode::Enter));
            let identity = panel.detail_plugin.read().clone().unwrap();
            *PLUGIN_LIST.state().write() = vec![
                plugin("compiler", "other-market", scope),
                plugin("compiler", "tools", "unknown"),
                wanted.clone(),
            ];
            assert_eq!(
                identity
                    .resolve(&PLUGIN_LIST.state().read(), &current_scope())
                    .unwrap()
                    .install_scope
                    .as_deref(),
                Some(scope)
            );
            panel.event(key(KeyCode::Enter));
            assert_eq!(
                Guard::requests().last(),
                Some(&PluginOperation::installed("enable", &wanted).unwrap())
            );
        });
    }
}

#[test]
#[serial]
fn installed_missing_identity_and_confirmation_never_target_replacement() {
    let _guard = Guard::new();
    for confirm in [false, true] {
        *PLUGIN_LIST.state().write() = vec![plugin("compiler", "tools", "project")];
        mount_panel(move |panel| {
            panel.event(key(KeyCode::Enter));
            if confirm {
                panel.event(key(KeyCode::Down));
                panel.event(key(KeyCode::Enter));
                assert_eq!(panel.confirm_action.read().as_deref(), Some("uninstall"));
            }
            *PLUGIN_LIST.state().write() = vec![plugin("replacement", "tools", "project")];
            panel.event(key(KeyCode::Enter));
            assert!(Guard::requests().is_empty());
            assert!(panel.detail_plugin.read().is_none());
            assert!(panel.confirm_action.read().is_none());
            assert!(panel.operation.read().error.is_some());
        });
    }
}

#[test]
#[serial]
fn marketplace_detail_and_confirmation_follow_source_not_position() {
    let _guard = Guard::new();
    for delete in [false, true] {
        mount_panel(move |panel| {
            let wanted = marketplace("tools", "github:org/tools");
            data::set_marketplaces_for_test(vec![
                wanted.clone(),
                marketplace("other", "github:org/other"),
            ]);
            *panel.active_tab.write() = PluginViewTab::Marketplaces;
            *panel.selected.write() = 1;
            panel.event(key(KeyCode::Enter));
            data::set_marketplaces_for_test(vec![
                marketplace("tools", "github:replacement/tools"),
                wanted,
            ]);
            if delete {
                panel.event(key(KeyCode::Down));
            }
            panel.event(key(KeyCode::Enter));
            if delete {
                assert_eq!(
                    panel.marketplace_confirmation.read().as_ref().unwrap().name,
                    "tools"
                );
                panel.event(key(KeyCode::Enter));
            }
            let expected = if delete {
                PluginOperation::marketplace_remove("tools".into())
            } else {
                PluginOperation::marketplace_refresh("tools".into())
            };
            assert_eq!(Guard::requests().last(), Some(&expected));
        });
    }
}

#[test]
#[serial]
fn marketplace_missing_source_rejects_detail_and_confirmation() {
    let _guard = Guard::new();
    for confirm in [false, true] {
        mount_panel(move |panel| {
            data::set_marketplaces_for_test(vec![marketplace("tools", "github:org/tools")]);
            *panel.active_tab.write() = PluginViewTab::Marketplaces;
            *panel.selected.write() = 1;
            panel.event(key(KeyCode::Enter));
            if confirm {
                panel.event(key(KeyCode::Down));
                panel.event(key(KeyCode::Enter));
            }
            data::set_marketplaces_for_test(vec![marketplace("tools", "github:replacement/tools")]);
            panel.event(key(KeyCode::Enter));
            assert!(Guard::requests().is_empty());
            assert!(panel.marketplace_detail.read().is_none());
            assert!(panel.marketplace_confirmation.read().is_none());
            assert!(panel.operation.read().error.is_some());
        });
    }
}

#[test]
#[serial]
fn stale_session_or_reset_event_cannot_cancel_new_loading() {
    let _guard = Guard::new();
    for reset in [false, true] {
        mount_panel(move |panel| {
            let old_scope = current_scope();
            if reset {
                BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get() + 1);
            } else {
                *ACTIVE_SESSION_ID.state().write() = "new-plugin-session".into();
            }
            let ticket = panel.operation.write().begin(
                PluginOperation::marketplace_refresh("tools".into()),
                current_scope(),
            );
            let attempts = data::REFRESH_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst);
            for code in [KeyCode::Char('x'), KeyCode::Enter, KeyCode::Esc] {
                let result = search_handler::handle_search_event(
                    key(code),
                    &old_scope,
                    None,
                    panel.active_tab,
                    panel.discover,
                    panel.detail_plugin,
                    panel.marketplace_detail,
                    panel.marketplace_detail_action,
                    panel.confirm_action,
                    panel.marketplace_confirmation,
                    panel.operation,
                    panel.add_marketplace_input,
                    panel.add_marketplace_active,
                );
                assert_eq!(result, EventResult::Consumed);
                let result = panel_handler::handle_panel_event(
                    key(code),
                    &old_scope,
                    None,
                    panel.selected,
                    panel.active_tab,
                    panel.discover,
                    panel.action_index,
                    panel.confirm_action,
                    panel.marketplace_confirmation,
                    panel.operation,
                    panel.detail_plugin,
                    panel.marketplace_detail,
                    panel.marketplace_detail_action,
                    panel.add_marketplace_input,
                    panel.add_marketplace_active,
                );
                assert_eq!(result, EventResult::Consumed);
                assert!(!ticket.is_cancelled());
                assert_eq!(
                    panel.operation.read().pending_action(),
                    Some("refresh_marketplace")
                );
            }
            assert_eq!(
                data::REFRESH_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst),
                attempts
            );
            assert!(panel.operation.write().complete(
                &ticket,
                crate::application::operations::OperationOutcome::Succeeded
            ));
            assert!(!panel.operation.read().is_pending());
        });
    }
}

#[test]
#[serial]
fn rejected_cache_refresh_does_not_publish() {
    let _guard = Guard::new();
    let _catalog = data::empty_catalog_for_test();
    let sentinel = marketplace("sentinel", "local-source");
    data::set_marketplaces_for_test(vec![sentinel]);
    // 第一次允许读盘，第二次模拟读盘期间作用域失效；不得发布旧结果。
    let calls = std::cell::Cell::new(0);
    data::refresh_marketplace_cache(&|| {
        calls.set(calls.get() + 1);
        calls.get() == 1
    });
    assert_eq!(calls.get(), 2);
    assert_eq!(data::get_marketplace_cache()[0].name, "sentinel");
    data::refresh_discover_cache(&|| false);
    assert!(data::get_discover_cache().is_empty());
}

#[test]
#[serial]
fn detail_render_keeps_opened_plugin_after_reorder() {
    let _guard = Guard::new();
    *PLUGIN_LIST.state().write() = vec![plugin("opened-identity", "tools", "user")];
    let text = operation::tests::mount_panel_frames(3, |panel| {
        panel.event(key(KeyCode::Enter));
        *PLUGIN_LIST.state().write() = vec![
            plugin("replacement-identity", "tools", "user"),
            plugin("opened-identity", "tools", "user"),
        ];
    });
    assert!(text.contains("opened-identity"));
    assert!(!text.contains("replacement-identity"));
}

#[test]
#[serial]
fn missing_detail_converges_in_effect_without_keyboard() {
    let _guard = Guard::new();
    *PLUGIN_LIST.state().write() = vec![plugin("opened-identity", "tools", "user")];
    let text = operation::tests::mount_panel_frames(4, |panel| {
        panel.event(key(KeyCode::Enter));
        *PLUGIN_LIST.state().write() = vec![plugin("replacement-identity", "tools", "user")];
    });
    assert!(text.contains("replacement-identity"));
    assert!(text.contains(&i18n::tr("panel-plugin-management-unsupported")));
    assert!(!text.contains("opened-identity"));
    assert!(Guard::requests().is_empty());
}

#[test]
#[serial]
fn marketplace_source_type_is_identity_not_display_label() {
    let scope = OperationScope::new("session", 0);
    let entry = marketplace("tools", "same-path");
    let key = MarketplaceIdentity::new(&entry, scope.clone());
    let mut replacement = entry;
    replacement.source = peri_middlewares::plugin::MarketplaceSource::File {
        path: "same-path".into(),
    };
    assert!(key.resolve(&[replacement], &scope).is_none());
}

#[test]
fn loading_has_one_completion_owner() {
    let mut state = OperationState::default();
    let scope = OperationScope::new("session", 1);
    let old = state.begin(
        PluginOperation::marketplace_refresh("old".into()),
        scope.clone(),
    );
    let new = state.begin(PluginOperation::marketplace_refresh("new".into()), scope);
    assert!(!state.complete(
        &old,
        crate::application::operations::OperationOutcome::Succeeded
    ));
    assert!(state.is_pending());
    assert!(state.complete(
        &new,
        crate::application::operations::OperationOutcome::Succeeded
    ));
    assert!(!state.is_pending());
}
