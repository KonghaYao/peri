use std::path::PathBuf;

use crate::chatgpt_auth::AuthTarget;
use crate::kit::atoms::{CONFIG_SOURCE_HANDLE, PERI_CONFIG_HANDLE};
use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::LoginPanelMode;
use super::auth::{AuthController, HostConfigAuthScope, notify_key, notify_text};
use super::auth_policy;
use super::config_store::{refresh_provider_list, save_login_edit};
use super::edit_state::{LoginEditState, LoginError, LoginErrorKind, LoginField};

/// 编辑视图产生的异步认证意图；面板在释放状态借用后执行。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum LoginEditIntent {
    #[default]
    None,
    QueryStatus,
    Login,
    LogoutConfirm,
}

/// 冻结当前草稿的宿主认证作用域（配置源身份 + store/account）。
pub(super) fn auth_scope(es: &LoginEditState) -> HostConfigAuthScope {
    HostConfigAuthScope::new(
        host_source_identity().unwrap_or_default(),
        AuthTarget::new(
            PathBuf::from(es.plan.store.trim()),
            es.plan.account.trim().to_owned(),
        ),
    )
}

/// 当前宿主配置源身份：进入表单后固定，不随会话 cwd 变化。
pub(super) fn host_source_identity() -> Option<String> {
    let source = crate::kit::atoms::CONFIG_SOURCE_HANDLE.get()?;
    Some(
        source
            .workspace_path()
            .unwrap_or_else(|| source.global_path())
            .display()
            .to_string(),
    )
}

/// 配置文件同目录下的默认凭据目录（绝对路径；不创建、不探测）。
pub(super) fn default_credential_store() -> Option<PathBuf> {
    let source = crate::kit::atoms::CONFIG_SOURCE_HANDLE.get()?;
    let settings = source
        .workspace_path()
        .unwrap_or_else(|| source.global_path());
    crate::chatgpt_auth::resolve_credential_store(settings, None).ok()
}

// ── Edit 模式辅助函数 ─────────────────────────────────────────────────────────

/// 从权威配置重读最新快照并刷新 Provider 投影。
///
/// 返回是否成功。失败（无配置源、解析/权限失败）时保留现有内存句柄，调用方
/// 仍可尝试用缓存的 revision 编辑；保存会暴露真实 authority 错误。
pub(super) fn load_latest_configuration() -> bool {
    if CONFIG_SOURCE_HANDLE.get().is_none() {
        return false;
    }
    match crate::kit::config_service::reload_configuration() {
        Ok(cfg) => {
            refresh_provider_list(&cfg);
            true
        }
        Err(error) => {
            tracing::warn!(error = %error, "LoginPanel: 重新载入配置失败，继续使用已发布快照");
            false
        }
    }
}

/// 从当前已发布配置进入既有 provider 编辑；不重新读取 authority。
fn enter_login_edit_mode_current(
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
    provider_id: &str,
) -> bool {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return false;
    };
    let revision = crate::config::configuration_revision().ok();
    let cfg = handle.read();
    let Some(state) = LoginEditState::edit_provider(&cfg, provider_id, revision) else {
        return false;
    };
    // 编辑既有 provider 默认聚焦 Provider ID：进入表单即可改名，类型行仍需显式确认。
    let cursor = state.field_text(LoginField::ProviderId).chars().count();
    *edit_state = Some(state);
    *edit_focus = LoginField::ProviderId;
    *edit_cursor = cursor;
    true
}

/// 从权威配置读取最新 revision 后进入编辑视图。
///
/// revision 冲突后按提示重新打开 /login 时，这里先 `reload`，确保 expected
/// revision 与 accepted snapshot 都来自最新配置，而不是进程启动时的缓存。
pub(super) fn enter_login_edit_mode(
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
    provider_id: &str,
) -> bool {
    load_latest_configuration();
    enter_login_edit_mode_current(edit_state, edit_focus, edit_cursor, provider_id)
}

fn enter_login_new_mode_current(
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
) -> bool {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return false;
    };
    let revision = crate::config::configuration_revision().ok();
    let cfg = handle.read();
    *edit_state = Some(LoginEditState::new_provider(&cfg, revision));
    *edit_focus = LoginField::ProviderType;
    *edit_cursor = 0;
    true
}

/// Ctrl+N：先重读权威配置，再以最新快照为基线新建 provider（不绑定任何档位）。
pub(super) fn enter_login_new_mode(
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
) -> bool {
    load_latest_configuration();
    enter_login_new_mode_current(edit_state, edit_focus, edit_cursor)
}

/// 冲突后的显式重载：丢弃当前草稿，按最新权威快照重新基线化同一/provider 表单。
///
/// 成功时保持 Edit 视图（同 provider 重建；新建表单重建为空表单）；provider 已被
/// 外部删除时回退 Browse，避免对不存在的身份继续保存。
///
/// `pub(super)`：键盘 Ctrl+R 与未来的鼠标“重新载入”按钮必须复用同一动作，
/// 不在 hit-test 接线时复制一套重载逻辑。
pub(super) fn reload_login_edit(
    mode: &mut LoginPanelMode,
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
) -> bool {
    let Some(target) = edit_state.as_ref().map(|state| {
        if state.is_new() {
            None
        } else {
            Some(state.original_provider_id.clone())
        }
    }) else {
        return false;
    };
    if !load_latest_configuration() {
        let error = anyhow::anyhow!(crate::i18n::tr("login-error-authority"));
        crate::kit::config_service::notify_reload_failed(&error);
        return false;
    }
    let rebuilt = match target {
        Some(provider_id) => {
            enter_login_edit_mode_current(edit_state, edit_focus, edit_cursor, &provider_id)
        }
        None => enter_login_new_mode_current(edit_state, edit_focus, edit_cursor),
    };
    if rebuilt {
        *mode = LoginPanelMode::Edit;
        crate::kit::config_service::notify_reloaded();
        true
    } else {
        *mode = LoginPanelMode::Browse;
        *edit_state = None;
        false
    }
}

/// 编辑模式按键处理所需的面板状态引用。
///
/// 这些状态由键盘与鼠标入口共同借用并原地更新；按职责打包后，调用点不再依赖
/// 8 个位置参数的正确顺序。
pub(super) struct LoginEditContext<'a> {
    pub(super) mode: &'a mut LoginPanelMode,
    pub(super) edit_state: &'a mut Option<LoginEditState>,
    pub(super) edit_focus: &'a mut LoginField,
    pub(super) edit_cursor: &'a mut usize,
    pub(super) auth: &'a mut AuthController,
}

/// 编辑模式下的按键处理。
pub(super) fn handle_login_edit_keys(
    ctx: LoginEditContext<'_>,
    auth_busy: bool,
    default_store: Option<PathBuf>,
    key: &KeyEvent,
) -> LoginEditIntent {
    let LoginEditContext {
        mode,
        edit_state,
        edit_focus,
        edit_cursor,
        auth,
    } = ctx;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if key.code == KeyCode::Esc {
        // 注销等待中先停止远端等待，保留本地清理结果；本地清理任务继续由
        // owner 接收，不能因关闭表单丢掉结论。再次 Esc 才放弃视图。
        if matches!(
            auth.view(),
            super::auth::AuthViewState::LoggingOut | super::auth::AuthViewState::CancellingLogout
        ) && auth.cancel_logout_wait()
        {
            notify_key("login-auth-cancelling-logout", 2);
            return LoginEditIntent::None;
        }
        if auth.login_succeeded() {
            notify_key("login-auth-unsaved", 3);
        }
        *mode = LoginPanelMode::Browse;
        *edit_state = None;
        return LoginEditIntent::None;
    }
    if key.code == KeyCode::Char('s') && is_ctrl {
        save_and_close(mode, edit_state, edit_focus, edit_cursor, auth);
        return LoginEditIntent::None;
    }
    // 冲突提示指向的显式重载：丢弃当前草稿，按最新权威快照重新基线化。
    // 非冲突状态下也允许使用；认证在途时拒绝，避免替换认证目标仍绑定的草稿。
    if key.code == KeyCode::Char('r') && is_ctrl {
        if auth_busy {
            notify_key("login-auth-wait-before-edit", 3);
            return LoginEditIntent::None;
        }
        let reloaded = reload_login_edit(mode, edit_state, edit_focus, edit_cursor);
        // Ctrl+R 可能换到另一 store/account：必须同步认证作用域，不能继续显示
        // 旧目标的“已登录”，也不能自动采用旧身份。
        if reloaded
            && let Some(es) = edit_state.as_ref()
            && es.is_plan_mode()
        {
            auth.sync_scope(auth_scope(es));
            return LoginEditIntent::QueryStatus;
        }
        return LoginEditIntent::None;
    }

    let Some(es) = edit_state else {
        *mode = LoginPanelMode::Browse;
        return LoginEditIntent::None;
    };
    let mut save_requested = false;
    let mut intent = LoginEditIntent::None;

    // 文本字段优先：字符、退格、删除、光标移动、Ctrl+W。
    if handle_login_text_input(es, *edit_focus, edit_cursor, key, auth_busy) {
        if matches!(*edit_focus, LoginField::PlanStore | LoginField::PlanAccount) {
            auth.invalidate_target();
        }
        return intent;
    }

    // 选择字段与动作。
    match *edit_focus {
        LoginField::ProviderType => match key.code {
            KeyCode::Left if !is_ctrl => {
                es.cycle_type(false);
                es.inline_error = None;
                es.ensure_focus_visible(edit_focus);
            }
            KeyCode::Right | KeyCode::Char(' ') if !is_ctrl => {
                es.cycle_type(true);
                es.inline_error = None;
                es.ensure_focus_visible(edit_focus);
            }
            KeyCode::Enter => {
                if es.is_new() {
                    es.cycle_type(true);
                    es.inline_error = None;
                    es.ensure_focus_visible(edit_focus);
                } else {
                    es.begin_type_change();
                    *mode = LoginPanelMode::ConfirmTypeChange;
                }
            }
            _ => {}
        },
        LoginField::AuthMode => {
            if auth_busy {
                notify_key("login-auth-wait-before-edit", 3);
            } else if matches!(
                key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter
            ) && !is_ctrl
            {
                es.cycle_auth_mode();
                es.ensure_plan_defaults(default_store.clone());
                es.inline_error = None;
                es.ensure_focus_visible(edit_focus);
                auth.sync_scope(auth_scope(es));
                if es.is_plan_mode() {
                    intent = LoginEditIntent::QueryStatus;
                }
            }
        }
        LoginField::AuthActions => match key.code {
            KeyCode::Left if !is_ctrl => auth.cycle_action(false, &es.plan),
            KeyCode::Right if !is_ctrl => auth.cycle_action(true, &es.plan),
            KeyCode::Enter | KeyCode::Char(' ') if !is_ctrl => {
                match auth.selected_action(&es.plan) {
                    Some(super::auth::AuthAction::Login) => intent = LoginEditIntent::Login,
                    Some(super::auth::AuthAction::Status) => intent = LoginEditIntent::QueryStatus,
                    Some(super::auth::AuthAction::UseAccount) => {
                        let scope = auth_scope(es);
                        if auth.use_account(&mut es.plan, &scope) {
                            es.inline_error = None;
                            auth.select_action(super::auth::AuthAction::Status, &es.plan);
                            notify_key("login-auth-account-adopted", 2);
                        } else if auth.scope() != &scope {
                            notify_key("login-auth-identity-scope-changed", 3);
                        } else {
                            notify_key("login-auth-identity-incomplete", 3);
                        }
                    }
                    Some(super::auth::AuthAction::Logout) => {
                        intent = LoginEditIntent::LogoutConfirm
                    }
                    Some(super::auth::AuthAction::CancelLogout) => {
                        if auth.cancel_logout_wait() {
                            notify_key("login-auth-cancelling-logout", 2);
                        }
                    }
                    None => notify_key("login-auth-target-required", 3),
                }
            }
            _ => {}
        },
        LoginField::Confirm => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') if !is_ctrl => {
                save_requested = true;
            }
            _ => {}
        },
        _ => {}
    }

    // 导航：Up/Down/Tab/BackTab 只在可见字段间循环。
    let previous_focus = *edit_focus;
    let direction = match key.code {
        KeyCode::Up | KeyCode::BackTab => Some(false),
        KeyCode::Down | KeyCode::Tab => Some(true),
        _ => None,
    };
    if let Some(forward) = direction {
        move_focus(es, edit_focus, edit_cursor, forward);
        if matches!(
            previous_focus,
            LoginField::PlanStore | LoginField::PlanAccount
        ) && auth.needs_status_query()
            && es.is_plan_mode()
            && intent == LoginEditIntent::None
        {
            auth.sync_scope(auth_scope(es));
            intent = LoginEditIntent::QueryStatus;
        }
    }
    if save_requested {
        save_and_close(mode, edit_state, edit_focus, edit_cursor, auth);
    }
    intent
}

/// 显式类型更改子视图：方向键只改候选，Enter 确认，Esc 保持原值。
pub(super) fn handle_login_type_change_keys(
    mode: &mut LoginPanelMode,
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
    key: &KeyEvent,
) {
    let Some(es) = edit_state else {
        *mode = LoginPanelMode::Browse;
        return;
    };
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Left if !is_ctrl => es.cycle_type_change_candidate(false),
        KeyCode::Right | KeyCode::Char(' ') if !is_ctrl => es.cycle_type_change_candidate(true),
        KeyCode::Enter => {
            es.confirm_type_change();
            es.inline_error = None;
            es.ensure_focus_visible(edit_focus);
            clamp_cursor(es, *edit_focus, edit_cursor);
            *mode = LoginPanelMode::Edit;
        }
        KeyCode::Esc => {
            es.cancel_type_change();
            *mode = LoginPanelMode::Edit;
        }
        _ => {}
    }
}

fn save_and_close(
    mode: &mut LoginPanelMode,
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
    auth: &mut AuthController,
) {
    if auth.blocks_config_save() {
        notify_key("login-auth-busy-save", 3);
        return;
    }
    if auth.outcome_unknown() {
        notify_key("login-auth-unknown-query", 3);
        return;
    }
    let Some(es) = edit_state else {
        *mode = LoginPanelMode::Browse;
        return;
    };
    if es.is_plan_mode() {
        // 1) 不同身份绝不静默覆盖：显式“使用此账号”是唯一出口。
        if auth_identity_conflict(es, auth) {
            let message = crate::i18n::tr("login-auth-identity-mismatch");
            es.inline_error = Some(LoginError::auth_reference(
                LoginField::AuthActions,
                message.clone(),
            ));
            *edit_focus = LoginField::AuthActions;
            auth.select_action(super::auth::AuthAction::UseAccount, &es.plan);
            notify_text(&message, 3);
            clamp_cursor(es, *edit_focus, edit_cursor);
            return;
        }
        // 2) 保存门最后机会采用：状态 effect 尚未落地时也能一次保存成功。
        if adopt_verified_identity(es, auth) {
            auth.select_action(super::auth::AuthAction::Status, &es.plan);
        }
        // 3) 认证事实已变化时，旧的身份/引用错误不得继续阻断保存。
        es.clear_auth_error();
    }
    if save_login_edit(es) {
        *mode = LoginPanelMode::Browse;
        *edit_state = None;
        return;
    }
    // 身份缺失可能是“状态刚成功、effect 尚未落地”：采用后重试一次。
    if is_identity_required(es) && adopt_verified_identity(es, auth) {
        es.clear_auth_error();
        auth.select_action(super::auth::AuthAction::Status, &es.plan);
        if save_login_edit(es) {
            *mode = LoginPanelMode::Browse;
            *edit_state = None;
            return;
        }
    }
    if is_identity_required(es) {
        apply_identity_block(es, auth, edit_focus, edit_cursor);
        return;
    }
    // 失败：保持草稿与焦点；字段错误定位到对应可见字段。
    let Some(error_field) = es.inline_error.as_ref().map(|error| error.field) else {
        return;
    };
    // 隐藏字段不能留下不可见焦点：凭据目录错误强制打开恢复行；其余落到认证动作组。
    let mut error_field = error_field;
    if !es.visible_fields().contains(&error_field) {
        if error_field == LoginField::PlanStore {
            es.open_store_recovery();
        }
        if !es.visible_fields().contains(&error_field) {
            error_field = if es.is_plan_mode() {
                LoginField::AuthActions
            } else {
                LoginField::Confirm
            };
        }
        if let Some(error) = es.inline_error.as_mut() {
            error.field = error_field;
        }
    }
    *edit_focus = error_field;
    clamp_cursor(es, *edit_focus, edit_cursor);
}

fn is_identity_required(es: &LoginEditState) -> bool {
    es.inline_error
        .as_ref()
        .is_some_and(|error| error.kind == LoginErrorKind::IdentityRequired)
}

/// 当前作用域的已验证账号与草稿身份不同。
fn auth_identity_conflict(es: &LoginEditState, auth: &AuthController) -> bool {
    let scope = auth_scope(es);
    auth_policy::conflict_status(&es.plan, auth.view(), auth.scope(), &scope).is_some()
}

fn adopt_verified_identity(es: &mut LoginEditState, auth: &AuthController) -> bool {
    let scope = auth_scope(es);
    auth_policy::adopt_if_verified(&mut es.plan, auth.view(), auth.scope(), &scope)
}

/// 身份无法自动采用：按认证状态给出准确文案与推荐动作。
///
/// `Checking` 只提示等待，不写红字；其他状态写字段错误并选中推荐动作。
fn apply_identity_block(
    es: &mut LoginEditState,
    auth: &mut AuthController,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
) {
    let scope = auth_scope(es);
    let Some(block) = auth_policy::save_block(&es.plan, auth.view(), auth.scope(), &scope) else {
        return;
    };
    let message = block.message();
    *edit_focus = LoginField::AuthActions;
    if block.is_wait() {
        es.clear_auth_error();
    } else {
        es.inline_error = Some(LoginError::identity_required(
            LoginField::AuthActions,
            message.clone(),
        ));
    }
    auth.select_action(block.recommended_action(), &es.plan);
    notify_text(&message, 3);
    clamp_cursor(es, *edit_focus, edit_cursor);
}

fn move_focus(es: &LoginEditState, focus: &mut LoginField, cursor: &mut usize, forward: bool) {
    let fields = es.visible_fields();
    let index = fields.iter().position(|field| field == focus).unwrap_or(0);
    let next = if forward {
        (index + 1) % fields.len()
    } else {
        (index + fields.len() - 1) % fields.len()
    };
    *focus = fields[next];
    *cursor = if focus.is_text() {
        es.field_text(*focus).chars().count()
    } else {
        0
    };
}

fn clamp_cursor(es: &LoginEditState, focus: LoginField, cursor: &mut usize) {
    *cursor = if focus.is_text() {
        es.field_text(focus).chars().count()
    } else {
        0
    };
}

/// 文本输入（与 setup_wizard 的 handle_text_input 模式一致）。
fn handle_login_text_input(
    state: &mut LoginEditState,
    field: LoginField,
    edit_cursor: &mut usize,
    key: &KeyEvent,
    auth_busy: bool,
) -> bool {
    if !field.is_text() {
        return false;
    }
    if auth_busy && matches!(field, LoginField::PlanStore | LoginField::PlanAccount) {
        notify_key("login-auth-wait-before-edit", 3);
        return true;
    }

    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // 先取长度与快照，再在可变借用内编辑，避免跨字段借用冲突。
    let mut value = state.field_text(field);
    let chars: Vec<char> = value.chars().collect();

    let mut edited = false;
    match key.code {
        Char(ch) if !is_ctrl => {
            if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
                state.inline_error = Some(super::edit_state::LoginError {
                    field,
                    message: crate::i18n::tr("login-error-single-line"),
                    kind: LoginErrorKind::Field,
                });
                return true;
            }
            let pos = (*edit_cursor).min(chars.len());
            let prefix: String = chars[..pos].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            value = format!("{}{}{}", prefix, ch, suffix);
            *edit_cursor = pos + 1;
            edited = true;
        }
        Backspace if !is_ctrl => {
            if *edit_cursor > 0 && *edit_cursor <= chars.len() {
                let prefix: String = chars[..*edit_cursor - 1].iter().collect();
                let suffix: String = chars[*edit_cursor..].iter().collect();
                value = format!("{}{}", prefix, suffix);
                *edit_cursor -= 1;
                edited = true;
            } else if *edit_cursor > chars.len() && !chars.is_empty() {
                value = chars[..chars.len() - 1].iter().collect();
                *edit_cursor = chars.len() - 1;
                edited = true;
            }
        }
        Delete => {
            if *edit_cursor < chars.len() {
                let prefix: String = chars[..*edit_cursor].iter().collect();
                let suffix: String = chars[*edit_cursor + 1..].iter().collect();
                value = format!("{}{}", prefix, suffix);
                edited = true;
            }
        }
        Left if !is_ctrl => {
            if *edit_cursor > 0 {
                *edit_cursor -= 1;
            }
            return true;
        }
        Right if !is_ctrl => {
            let max_pos = chars.len();
            if *edit_cursor < max_pos {
                *edit_cursor += 1;
            }
            return true;
        }
        Home if !is_ctrl => {
            *edit_cursor = 0;
            return true;
        }
        End if !is_ctrl => {
            *edit_cursor = chars.len();
            return true;
        }
        Char('w') if is_ctrl => {
            let pos = (*edit_cursor).min(chars.len());
            if pos == 0 {
                return true;
            }
            let mut end = pos;
            while end > 0 && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            while end > 0 && !chars[end - 1].is_whitespace() {
                end -= 1;
            }
            let prefix: String = chars[..end].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            *edit_cursor = end;
            value = format!("{}{}", prefix, suffix);
            edited = true;
        }
        _ => return false,
    }

    if edited {
        if let Some(target) = state.field_text_mut(field) {
            *target = value;
        }
        if matches!(field, LoginField::PlanStore | LoginField::PlanAccount) {
            // A changed store/account cannot inherit the previous verified identity.
            state.plan.clear_identity();
        }
        state.inline_error = None;
    }
    true
}

/// 粘贴到当前编辑文本字段的光标位置。
///
/// 单行字段过滤换行与控制字符并留下可见错误；选择/按钮字段不接收粘贴内容。
pub(super) fn handle_login_paste(
    edit_cursor: &mut usize,
    state: &mut LoginEditState,
    field: LoginField,
    paste_text: &str,
) {
    if !field.is_text() {
        return;
    }
    const MAX_PASTE_CHARS: usize = 10_000;
    let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
    let (sanitized, removed): (String, usize) =
        normalized
            .chars()
            .fold((String::new(), 0usize), |(mut text, removed), ch| {
                if ch == '\n' || ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
                    (text, removed + 1)
                } else {
                    text.push(ch);
                    (text, removed)
                }
            });
    let truncated: String = sanitized.chars().take(MAX_PASTE_CHARS).collect();
    if sanitized.chars().count() > truncated.chars().count() {
        tracing::warn!("login panel: paste truncated to {MAX_PASTE_CHARS} chars (CJK-safe)");
    }
    if removed > 0 {
        state.inline_error = Some(super::edit_state::LoginError {
            field,
            message: crate::i18n::tr("login-error-single-line"),
            kind: LoginErrorKind::Field,
        });
    } else {
        // 与按键编辑一致：本次粘贴未过滤任何字符，说明输入已回到单行状态。
        state.inline_error = None;
    }

    let mut value = state.field_text(field);
    let chars: Vec<char> = value.chars().collect();
    let pos = (*edit_cursor).min(chars.len());
    let prefix: String = chars[..pos].iter().collect();
    let suffix: String = chars[pos..].iter().collect();
    let paste_len = truncated.chars().count();
    value = format!("{}{}{}", prefix, truncated, suffix);
    if let Some(target) = state.field_text_mut(field) {
        *target = value;
    }
    if matches!(field, LoginField::PlanStore | LoginField::PlanAccount) {
        state.plan.clear_identity();
    }
    *edit_cursor = pos + paste_len;
}
