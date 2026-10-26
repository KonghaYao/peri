//! Plan 认证与保存门的单一推导规则。
//!
//! `AuthController` 只回答“当前凭据目录的认证视图是什么”；`PlanDraft` 只回答
//! “将要落盘的引用是什么”。本模块把两者合成一个可测试的保存就绪状态，供状态
//! 渲染、动作可用性与保存门共同使用，避免再次出现“状态说已登录、保存说没登录”。
//!
//! 纯函数边界：不读全局句柄、不写盘、不产生副作用；作用域由调用方算好传入。

use fluent_bundle::FluentValue;

use crate::chatgpt_auth::AccountStatus;
use crate::i18n;

use super::plan::PlanDraft;
use crate::chatgpt_auth::{AuthAction, AuthFailure, AuthViewState, HostConfigAuthScope};

/// 本地记录中的 access token 新鲜度。
///
/// 只描述访问令牌：过期不代表登录失效，使用时由 `Credential` 请求自动刷新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenFreshness {
    Valid,
    Expiring,
    Expired,
    Unknown,
}

impl TokenFreshness {
    /// 即将过期的显示阈值（与 credentials 的 60 秒自动刷新阈值无关）。
    pub(super) const EXPIRING_WINDOW_SECS: u64 = 300;

    /// `expires_at == 0` 表示旧服务端/未知；时间比较由调用方注入，不读真实时钟。
    pub(super) fn classify(expires_at: u64, now: u64) -> Self {
        if expires_at == 0 {
            Self::Unknown
        } else if now >= expires_at {
            Self::Expired
        } else if expires_at - now <= Self::EXPIRING_WINDOW_SECS {
            Self::Expiring
        } else {
            Self::Valid
        }
    }

    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            Self::Valid => "login-auth-token-valid",
            Self::Expiring => "login-auth-token-expiring",
            Self::Expired => "login-auth-token-expired",
            Self::Unknown => "login-auth-token-unknown",
        }
    }
}

/// 保存门需要区分的身份阻断原因；每个原因对应一条准确文案与推荐动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdentityBlock {
    Checking,
    Unchecked,
    SignedOut,
    LoggedOut,
    Failed(AuthFailure),
    OutcomeUnknown,
    ScopeStale,
    NoPlanScope,
    Incomplete,
    /// 认证/注销进行中；保存门通常先由 `blocks_config_save` 拦截。
    Busy,
}

impl IdentityBlock {
    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            Self::Checking => "login-auth-identity-checking",
            Self::Unchecked => "login-auth-identity-unchecked",
            Self::SignedOut | Self::LoggedOut => "login-auth-identity-signed-out",
            Self::Failed(_) => "login-auth-identity-status-failed",
            Self::OutcomeUnknown => "login-auth-identity-outcome-unknown",
            Self::ScopeStale => "login-auth-identity-scope-changed",
            Self::NoPlanScope => "login-auth-no-plan-scope",
            Self::Incomplete => "login-auth-identity-incomplete",
            Self::Busy => "login-auth-busy-save",
        }
    }

    /// 状态查询中：保存只提示等待，不写红字。
    pub(super) fn is_wait(self) -> bool {
        matches!(self, Self::Checking)
    }

    /// 推荐动作；绝不返回注销。
    pub(super) fn recommended_action(self) -> AuthAction {
        match self {
            Self::SignedOut | Self::LoggedOut | Self::NoPlanScope | Self::Incomplete => {
                AuthAction::Login
            }
            Self::Checking
            | Self::Unchecked
            | Self::Failed(_)
            | Self::OutcomeUnknown
            | Self::ScopeStale
            | Self::Busy => AuthAction::Status,
        }
    }

    pub(super) fn message(self) -> String {
        match self {
            Self::Failed(failure) => i18n::tr_args(
                self.i18n_key(),
                &[(
                    "reason".to_string(),
                    FluentValue::from(i18n::tr(failure.i18n_key()).as_str()),
                )],
            ),
            _ => i18n::tr(self.i18n_key()),
        }
    }
}

/// 草稿身份相对已验证状态的唯一结论。
pub(super) enum IdentityReadiness<'a> {
    /// 草稿身份完整，或已与状态一致：无需认证干预。
    Ready,
    /// 当前作用域的已登录状态可静默写入空/兼容草稿身份。
    Adoptable(&'a AccountStatus),
    /// 草稿已有不同身份：只能显式“使用此账号”，不得自动覆盖。
    Conflict(&'a AccountStatus),
    /// 需要用户动作或等待。
    NeedsAction(IdentityBlock),
}

pub(super) fn readiness<'a>(
    plan: &PlanDraft,
    view: &'a AuthViewState,
    status_scope: &HostConfigAuthScope,
    current_scope: &HostConfigAuthScope,
) -> IdentityReadiness<'a> {
    if let AuthViewState::SignedIn(status) = view {
        if status_scope != current_scope {
            return if plan.needs_identity() {
                IdentityReadiness::NeedsAction(IdentityBlock::ScopeStale)
            } else {
                IdentityReadiness::Ready
            };
        }
        if plan.identity_mismatch(status) {
            return IdentityReadiness::Conflict(status);
        }
        if !plan.needs_identity() {
            return IdentityReadiness::Ready;
        }
        if !status.plan_enabled {
            return IdentityReadiness::NeedsAction(IdentityBlock::NoPlanScope);
        }
        if !status_has_identity(status) {
            return IdentityReadiness::NeedsAction(IdentityBlock::Incomplete);
        }
        return IdentityReadiness::Adoptable(status);
    }
    // 已有完整引用时，未登录/查询失败不阻断无关编辑；运行时由 Core 报告失败。
    if !plan.needs_identity() {
        return IdentityReadiness::Ready;
    }
    IdentityReadiness::NeedsAction(block_for(view))
}

/// 当前作用域内已登录但身份不同：保存前必须显式采用。
pub(super) fn conflict_status<'a>(
    plan: &PlanDraft,
    view: &'a AuthViewState,
    status_scope: &HostConfigAuthScope,
    current_scope: &HostConfigAuthScope,
) -> Option<&'a AccountStatus> {
    match readiness(plan, view, status_scope, current_scope) {
        IdentityReadiness::Conflict(status) => Some(status),
        _ => None,
    }
}

/// 空/兼容身份 + 当前作用域 + Plan 已授予时静默采用；否则保持原样。
pub(super) fn adopt_if_verified(
    plan: &mut PlanDraft,
    view: &AuthViewState,
    status_scope: &HostConfigAuthScope,
    current_scope: &HostConfigAuthScope,
) -> bool {
    match readiness(plan, view, status_scope, current_scope) {
        IdentityReadiness::Adoptable(status) => plan.adopt_verified(status),
        _ => false,
    }
}

/// 保存门在无法自动采用时返回的状态化阻断原因。
pub(super) fn save_block(
    plan: &PlanDraft,
    view: &AuthViewState,
    status_scope: &HostConfigAuthScope,
    current_scope: &HostConfigAuthScope,
) -> Option<IdentityBlock> {
    match readiness(plan, view, status_scope, current_scope) {
        IdentityReadiness::NeedsAction(block) => Some(block),
        // 调用方应先 adopt；到这里说明状态可写但采用未完成，仍需一次显式动作。
        IdentityReadiness::Adoptable(_) => Some(IdentityBlock::Incomplete),
        IdentityReadiness::Ready | IdentityReadiness::Conflict(_) => None,
    }
}

fn status_has_identity(status: &AccountStatus) -> bool {
    !status.subject.trim().is_empty() && !status.client_id.trim().is_empty()
}

fn block_for(view: &AuthViewState) -> IdentityBlock {
    match view {
        AuthViewState::Unchecked => IdentityBlock::Unchecked,
        AuthViewState::Checking => IdentityBlock::Checking,
        AuthViewState::SignedOut => IdentityBlock::SignedOut,
        AuthViewState::LoggedOut(_) => IdentityBlock::LoggedOut,
        AuthViewState::Failed(failure) => IdentityBlock::Failed(*failure),
        AuthViewState::OutcomeUnknown(_) => IdentityBlock::OutcomeUnknown,
        AuthViewState::StartingLogin
        | AuthViewState::WaitingCallback { .. }
        | AuthViewState::Cancelling
        | AuthViewState::LoggingOut
        | AuthViewState::CancellingLogout => IdentityBlock::Busy,
        // 上面的 `SignedIn` 分支已处理；保持穷尽性。
        AuthViewState::SignedIn(_) => IdentityBlock::Incomplete,
    }
}

#[cfg(test)]
#[path = "auth_policy_test.rs"]
mod tests;
