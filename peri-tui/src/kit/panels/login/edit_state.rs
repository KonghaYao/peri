//! Login 编辑草稿：typed 选择、provider 候选构造。
//!
//! 本模块不读全局句柄、不写磁盘：所有输入从进入表单时捕获的 [`PeriConfig`] 基线
//! 派生，候选构造与校验都是纯函数。Login 只编辑 provider；档位绑定、模型与
//! contextWindow 由 Model 面板负责，新建 provider 不写任何 profile。落盘与发布由
//! `config_store` 负责。

use crate::config::ConfigurationRevision;
use crate::i18n;

use super::edit_errors::{
    profile_model_missing, profile_provider_missing, profile_resolver_error,
    profile_window_missing, provider_error, remap_hidden_field,
};

pub(super) use super::edit_types::{LoginError, LoginErrorKind, LoginField};
pub(super) use super::plan::PlanDraft;
#[cfg(test)]
pub(super) use super::plan::PlanReference;
use peri_acp::provider::config::{
    PeriConfig, ProfileConfig, Profiles, ProviderAuthConfig, ProviderConfig, ProviderModels,
};
use std::path::PathBuf;

/// UI 侧 Provider 类型。与 schema 字符串只在边界转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenAiResponses,
}

impl ProviderKind {
    pub(super) fn from_raw(raw: &str) -> Option<Self> {
        match raw {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            "openai_responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai_responses",
        }
    }

    pub(super) fn label_key(self) -> &'static str {
        match self {
            Self::Anthropic => "setup-provider-anthropic",
            Self::OpenAi => "setup-provider-openai",
            Self::OpenAiResponses => "setup-provider-openai-responses",
        }
    }

    pub(super) fn next(self) -> Self {
        match self {
            Self::Anthropic => Self::OpenAi,
            Self::OpenAi => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::Anthropic,
        }
    }

    pub(super) fn previous(self) -> Self {
        match self {
            Self::Anthropic => Self::OpenAiResponses,
            Self::OpenAi => Self::Anthropic,
            Self::OpenAiResponses => Self::OpenAi,
        }
    }
}

/// 当前草稿的认证方式。仅 Responses provider 可选择；切换只改草稿，
/// 最终序列化只输出当前模式，API key 模式不残留 plan 引用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthChoice {
    ApiKey,
    ChatGptPlan,
}

impl AuthChoice {
    pub(super) fn toggle(self) -> Self {
        match self {
            Self::ApiKey => Self::ChatGptPlan,
            Self::ChatGptPlan => Self::ApiKey,
        }
    }
}

/// Login 编辑状态。
///
/// 不实现 `Debug`：草稿可能持有 API key，格式化整份结构会把秘密带进日志/断言。
#[derive(Clone)]
pub(super) struct LoginEditState {
    /// 进入编辑时的 provider id；空表示新建。
    pub(super) original_provider_id: String,
    /// 当前草稿类型（未知已保存类型为 None，保存会被拒绝）。
    pub(super) kind: Option<ProviderKind>,
    /// 未知类型原值（显示用；不能默认成 Anthropic）。
    pub(super) raw_type: String,
    pub(super) provider_id: String,
    pub(super) auth_choice: AuthChoice,
    pub(super) plan: PlanDraft,
    pub(super) api_key: String,
    pub(super) base_url: String,
    pub(super) models: ProviderModels,
    /// provider 的未知扩展字段 / 显示名，编辑时原样保留。
    pub(super) name: Option<String>,
    pub(super) extra: serde_json::Map<String, serde_json::Value>,
    pub(super) baseline: PeriConfig,
    pub(super) expected_revision: Option<ConfigurationRevision>,
    pub(super) inline_error: Option<LoginError>,
    /// 显式类型更改子视图的候选类型（None 表示未打开）。
    pub(super) type_change_candidate: Option<ProviderKind>,
    /// 凭据目录是否以可编辑恢复行展示（缺失/非法/保存错误时置位；一旦展示不再因
    /// 当前值变化而消失，避免焦点跳走）。
    pub(super) plan_store_editable: bool,
}

/// 凭据目录可用：非空且是绝对路径（与 `PlanDraft::apply` 同一判定）。
fn plan_store_is_valid(store: &str) -> bool {
    let store = store.trim();
    !store.is_empty() && std::path::Path::new(store).is_absolute()
}

/// 空引用回退首 provider 的有效归属。
fn effective_owner<'a>(cfg: &'a PeriConfig, profile: &ProfileConfig) -> Option<&'a ProviderConfig> {
    if profile.provider.is_empty() {
        cfg.config.providers.first()
    } else {
        cfg.config
            .providers
            .iter()
            .find(|provider| provider.id == profile.provider)
    }
}

impl LoginEditState {
    pub(super) fn is_new(&self) -> bool {
        self.original_provider_id.is_empty()
    }

    /// 新建 provider：只构造 provider 草稿，不绑定任何 profile。
    pub(super) fn new_provider(cfg: &PeriConfig, revision: Option<ConfigurationRevision>) -> Self {
        Self {
            original_provider_id: String::new(),
            kind: Some(ProviderKind::Anthropic),
            raw_type: ProviderKind::Anthropic.as_str().to_string(),
            provider_id: String::new(),
            auth_choice: AuthChoice::ApiKey,
            plan: PlanDraft::default(),
            api_key: String::new(),
            base_url: String::new(),
            models: ProviderModels::default(),
            name: None,
            extra: serde_json::Map::new(),
            baseline: cfg.clone(),
            expected_revision: revision,
            inline_error: None,
            type_change_candidate: None,
            plan_store_editable: false,
        }
    }

    /// 编辑已有 provider。返回 None 表示目标已不在配置里。
    pub(super) fn edit_provider(
        cfg: &PeriConfig,
        provider_id: &str,
        revision: Option<ConfigurationRevision>,
    ) -> Option<Self> {
        let provider = cfg
            .config
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)?
            .clone();
        let baseline_kind = ProviderKind::from_raw(&provider.provider_type);
        let (auth_choice, plan) = match &provider.auth {
            Some(ProviderAuthConfig::ChatGptPlan {
                store,
                account,
                subject,
                client_id,
            }) => (
                AuthChoice::ChatGptPlan,
                PlanDraft::from_reference(store, account, subject, client_id),
            ),
            Some(ProviderAuthConfig::ApiKey) | None => (AuthChoice::ApiKey, PlanDraft::default()),
        };
        let plan_store_editable =
            auth_choice == AuthChoice::ChatGptPlan && !plan_store_is_valid(&plan.store);
        Some(Self {
            original_provider_id: provider.id.clone(),
            kind: baseline_kind,
            raw_type: provider.provider_type.clone(),
            provider_id: provider.id.clone(),
            auth_choice,
            plan,
            api_key: provider.api_key.clone(),
            base_url: provider.base_url.clone(),
            models: provider.models.clone(),
            name: provider.name.clone(),
            extra: provider.extra.clone(),
            baseline: cfg.clone(),
            expected_revision: revision,
            inline_error: None,
            type_change_candidate: None,
            plan_store_editable,
        })
    }

    pub(super) fn is_responses(&self) -> bool {
        self.kind == Some(ProviderKind::OpenAiResponses)
    }

    /// Responses plan mode: API key fields are hidden and plan auth serialized.
    pub(super) fn is_plan_mode(&self) -> bool {
        self.is_responses() && self.auth_choice == AuthChoice::ChatGptPlan
    }

    /// 已完整验证的 plan 引用（显示与测试用；身份不完整时为 None）。
    #[cfg(test)]
    pub(super) fn plan_reference(&self) -> Option<PlanReference> {
        if self.is_plan_mode() {
            self.plan.reference()
        } else {
            None
        }
    }

    /// Responses 专属认证方式切换；草稿互不覆盖，切回可恢复另一模式输入。
    pub(super) fn cycle_auth_mode(&mut self) {
        if !self.is_responses() {
            return;
        }
        self.auth_choice = self.auth_choice.toggle();
        self.seed_default_models_from_saved();
    }

    /// 新建 Responses + ChatGPT plan Provider 时，用已保存的号池映射预填四档默认模型。
    ///
    /// 只填空字段，用户手动输入优先保留；不写 profile、不新增绑定。
    pub(super) fn seed_default_models_from_saved(&mut self) -> bool {
        if !self.is_new() || !self.is_plan_mode() {
            return false;
        }
        let Some(source) = saved_model_mapping(&self.baseline) else {
            return false;
        };
        let mut changed = false;
        for (target, candidate) in [
            (&mut self.models.fable, &source.fable),
            (&mut self.models.opus, &source.opus),
            (&mut self.models.sonnet, &source.sonnet),
            (&mut self.models.haiku, &source.haiku),
        ] {
            if target.trim().is_empty() && !candidate.trim().is_empty() {
                *target = candidate.clone();
                changed = true;
            }
        }
        changed
    }

    /// 新建 plan Provider 且没有任何可复用来源：展示一次性可操作提示。
    pub(super) fn model_defaults_hint_visible(&self) -> bool {
        self.is_new()
            && self.is_plan_mode()
            && models_are_empty(&self.models)
            && saved_model_mapping(&self.baseline).is_none()
    }

    /// 切换到 plan 模式时补齐宿主默认 store/account（不创建目录）。
    ///
    /// 补齐后仍不是绝对路径时强制展示可编辑恢复行；合法默认值不置位。
    pub(super) fn ensure_plan_defaults(&mut self, default_store: Option<PathBuf>) {
        self.plan.ensure_defaults(default_store);
        if self.is_plan_mode() && !plan_store_is_valid(&self.plan.store) {
            self.plan_store_editable = true;
        }
        self.seed_default_models_from_saved();
    }

    /// 恢复行是否参与导航/绘制/命中（仅 plan 模式且已置位）。
    pub(super) fn store_recovery_visible(&self) -> bool {
        self.is_plan_mode() && self.plan_store_editable
    }

    /// 保存错误指向凭据目录时强制打开恢复行。
    pub(super) fn open_store_recovery(&mut self) {
        if self.is_plan_mode() {
            self.plan_store_editable = true;
        }
    }

    /// 动态可见字段：导航、绘制、粘贴与错误定位共用。
    pub(super) fn visible_fields(&self) -> Vec<LoginField> {
        let mut fields = vec![LoginField::ProviderType, LoginField::ProviderId];
        if self.is_responses() {
            fields.push(LoginField::AuthMode);
        }
        if self.is_plan_mode() {
            fields.push(LoginField::AuthActions);
        } else {
            fields.push(LoginField::BaseUrl);
            fields.push(LoginField::ApiKey);
        }
        fields.extend([
            LoginField::FableModel,
            LoginField::OpusModel,
            LoginField::SonnetModel,
            LoginField::HaikuModel,
        ]);
        // 凭据目录的恢复行在视觉上属于“高级”区，位于 Confirm 之前。
        if self.store_recovery_visible() {
            fields.push(LoginField::PlanStore);
        }
        fields.push(LoginField::Confirm);
        fields
    }

    /// 焦点字段被隐藏（切换类型/认证方式后）时收敛到最近的可操作字段。
    ///
    /// 隐藏的账号/凭据目录不能留下不可见焦点：回到认证动作组；其余回退 Confirm。
    pub(super) fn ensure_focus_visible(&self, focus: &mut LoginField) {
        let fields = self.visible_fields();
        if fields.contains(focus) {
            return;
        }
        *focus = match *focus {
            LoginField::PlanAccount | LoginField::PlanStore
                if fields.contains(&LoginField::AuthActions) =>
            {
                LoginField::AuthActions
            }
            _ => fields
                .get(
                    fields
                        .iter()
                        .position(|field| field == &LoginField::Confirm)
                        .unwrap_or(0),
                )
                .copied()
                .unwrap_or(LoginField::Confirm),
        };
    }

    pub(super) fn field_text(&self, field: LoginField) -> String {
        match field {
            LoginField::ProviderId => self.provider_id.clone(),
            LoginField::PlanStore => self.plan.store.clone(),
            LoginField::PlanAccount => self.plan.account.clone(),
            LoginField::BaseUrl => self.base_url.clone(),
            LoginField::ApiKey => self.api_key.clone(),
            LoginField::FableModel => self.models.fable.clone(),
            LoginField::OpusModel => self.models.opus.clone(),
            LoginField::SonnetModel => self.models.sonnet.clone(),
            LoginField::HaikuModel => self.models.haiku.clone(),
            LoginField::ProviderType
            | LoginField::AuthMode
            | LoginField::AuthActions
            | LoginField::Confirm => String::new(),
        }
    }

    /// 可变文本字段访问；非文本字段返回 None（调用方不会写入）。
    pub(super) fn field_text_mut(&mut self, field: LoginField) -> Option<&mut String> {
        match field {
            LoginField::ProviderId => Some(&mut self.provider_id),
            LoginField::PlanStore => Some(&mut self.plan.store),
            LoginField::PlanAccount => Some(&mut self.plan.account),
            LoginField::BaseUrl => Some(&mut self.base_url),
            LoginField::ApiKey => Some(&mut self.api_key),
            LoginField::FableModel => Some(&mut self.models.fable),
            LoginField::OpusModel => Some(&mut self.models.opus),
            LoginField::SonnetModel => Some(&mut self.models.sonnet),
            LoginField::HaikuModel => Some(&mut self.models.haiku),
            LoginField::ProviderType
            | LoginField::AuthMode
            | LoginField::AuthActions
            | LoginField::Confirm => None,
        }
    }

    /// 新建 provider 的类型选择：三态环。
    pub(super) fn cycle_type(&mut self, forward: bool) {
        if self.is_new() {
            let current = self.kind.unwrap_or(ProviderKind::Anthropic);
            let next = if forward {
                current.next()
            } else {
                current.previous()
            };
            self.kind = Some(next);
            self.raw_type = next.as_str().to_string();
            self.seed_default_models_from_saved();
        }
    }

    /// 显式类型更改的候选初始化。
    pub(super) fn begin_type_change(&mut self) {
        if !self.is_new() {
            self.type_change_candidate = Some(self.kind.unwrap_or(ProviderKind::Anthropic));
        }
    }

    pub(super) fn cycle_type_change_candidate(&mut self, forward: bool) {
        if let Some(candidate) = self.type_change_candidate {
            self.type_change_candidate = Some(if forward {
                candidate.next()
            } else {
                candidate.previous()
            });
        }
    }

    pub(super) fn confirm_type_change(&mut self) {
        if let Some(candidate) = self.type_change_candidate.take() {
            self.kind = Some(candidate);
            self.raw_type = candidate.as_str().to_string();
            self.seed_default_models_from_saved();
        }
    }

    pub(super) fn cancel_type_change(&mut self) {
        self.type_change_candidate = None;
    }

    /// 有效归属为当前 provider 的档位（显式引用或首 provider 回退）。
    ///
    /// 仅用于编辑已有 provider 时的 resolver 校验和类型确认提示；新建 provider
    /// 不读取、不写回任何档位。
    pub(super) fn bound_aliases(&self) -> Vec<&'static str> {
        if self.is_new() {
            return Vec::new();
        }
        Profiles::ALL
            .iter()
            .copied()
            .filter(|alias| {
                self.baseline
                    .config
                    .profiles
                    .get(alias)
                    .and_then(|profile| effective_owner(&self.baseline, profile))
                    .is_some_and(|provider| provider.id == self.original_provider_id)
            })
            .collect()
    }

    /// 认证状态变化或显式采用后清理陈旧的认证类错误。
    ///
    /// 只清 `IdentityRequired`/`AuthReference`：普通字段错误与 revision 冲突
    /// 仍代表真实输入/权威问题，不能因状态刷新被掩盖。返回是否清除了身份错误。
    pub(super) fn clear_auth_error(&mut self) -> bool {
        let Some(error) = self.inline_error.as_ref() else {
            return false;
        };
        let cleared_identity = error.kind == LoginErrorKind::IdentityRequired;
        if matches!(
            error.kind,
            LoginErrorKind::IdentityRequired | LoginErrorKind::AuthReference
        ) {
            self.inline_error = None;
        }
        cleared_identity
    }

    /// 构造保存候选（纯函数，不落盘）。
    #[cfg(test)]
    pub(super) fn build_candidate(&self) -> Result<PeriConfig, LoginError> {
        self.build_candidate_with_warnings()
            .map(|(candidate, _)| candidate)
    }

    /// 构造保存候选，并返回“原本就不可解析、本次也未修好”的档位。
    ///
    /// 这些档位不阻断保存（Login 改不了 contextWindow/绑定），但保存成功后要
    /// 给一次非阻断提示，避免用户以为它们已经可用。
    pub(super) fn build_candidate_with_warnings(
        &self,
    ) -> Result<(PeriConfig, Vec<&'static str>), LoginError> {
        let provider_id = self.provider_id.trim();
        if provider_id.is_empty() {
            return Err(LoginError::field(
                LoginField::ProviderId,
                i18n::tr("login-error-id-empty"),
            ));
        }
        if self
            .baseline
            .config
            .providers
            .iter()
            .any(|provider| provider.id == provider_id && provider.id != self.original_provider_id)
        {
            return Err(LoginError::field(
                LoginField::ProviderId,
                i18n::tr("login-error-id-duplicate"),
            ));
        }
        let kind = self.kind.ok_or_else(|| {
            LoginError::field(
                LoginField::ProviderType,
                i18n::tr("login-error-unknown-type"),
            )
        })?;

        let mut candidate = self.baseline.clone();
        let mut provider = if self.is_new() {
            ProviderConfig {
                id: provider_id.to_string(),
                ..Default::default()
            }
        } else {
            let Some(existing) = candidate
                .config
                .providers
                .iter()
                .find(|provider| provider.id == self.original_provider_id)
                .cloned()
            else {
                return Err(LoginError::field(
                    LoginField::ProviderId,
                    i18n::tr("login-error-provider-missing"),
                ));
            };
            existing
        };
        provider.id = provider_id.to_string();
        provider.provider_type = kind.as_str().to_string();
        provider.models = ProviderModels {
            fable: self.models.fable.trim().to_string(),
            opus: self.models.opus.trim().to_string(),
            sonnet: self.models.sonnet.trim().to_string(),
            haiku: self.models.haiku.trim().to_string(),
        };
        provider.name = self.name.clone();
        provider.extra = self.extra.clone();

        let keep_plan = self.is_plan_mode();
        if keep_plan {
            self.plan.apply(&mut provider).map_err(remap_hidden_field)?;
        } else if kind == ProviderKind::OpenAiResponses {
            provider.api_key = self.api_key.trim().to_string();
            provider.base_url = self.base_url.trim().to_string();
            provider.auth = match (&provider.auth, self.is_new()) {
                (Some(ProviderAuthConfig::ApiKey), _) => Some(ProviderAuthConfig::ApiKey),
                (_, true) => Some(ProviderAuthConfig::ApiKey),
                _ => None,
            };
        } else {
            provider.auth = None;
            provider.api_key = self.api_key.trim().to_string();
            provider.base_url = self.base_url.trim().to_string();
        }

        let provider_index = if self.is_new() {
            candidate.config.providers.push(provider);
            candidate.config.providers.len() - 1
        } else {
            let Some(index) = candidate
                .config
                .providers
                .iter()
                .position(|provider| provider.id == self.original_provider_id)
            else {
                return Err(LoginError::field(
                    LoginField::ProviderId,
                    i18n::tr("login-error-provider-missing"),
                ));
            };
            candidate.config.providers[index] = provider;
            index
        };
        // 档位绑定与参数完全归 Model 面板：Login 只追加/更新 provider。
        // 改名覆盖所有显式引用；空引用继续走首 provider 回退，不写死。
        if !self.is_new() && self.original_provider_id != provider_id {
            for alias in Profiles::ALL {
                if let Some(profile) = candidate.config.profiles.get_mut(alias)
                    && profile.provider == self.original_provider_id
                {
                    profile.provider = provider_id.to_string();
                }
            }
        }

        // provider 级 Responses 校验与 resolver 共用同一实现（未绑定任何档位也能校验）。
        let mut unresolved = Vec::new();
        if self.is_responses() {
            let provider = &candidate.config.providers[provider_index];
            if let Err(error) = peri_config::provider::validate_responses_provider(provider) {
                return Err(provider_error(error));
            }
            // 新建 provider 不改任何档位，不因未绑定/缺 contextWindow 阻断保存。
            // 已有 provider 只阻断“本次新弄坏”的绑定：baseline 原本就不解析且
            // 本次仍未修好的档位由 Model 面板负责，保存后给出非阻断提示。
            if !self.is_new() {
                for alias in self.bound_aliases() {
                    let baseline_ok =
                        peri_config::provider::try_resolve_for_alias(&self.baseline, alias)
                            .is_ok_and(|resolved| resolved.is_some());
                    match peri_config::provider::try_resolve_for_alias(&candidate, alias) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            if baseline_ok {
                                return Err(profile_provider_missing(alias));
                            }
                            unresolved.push(alias);
                        }
                        Err(error) => {
                            if baseline_ok {
                                return Err(resolver_error_for_candidate(&candidate, alias, error));
                            }
                            unresolved.push(alias);
                        }
                    }
                }
            }
        }

        Ok((candidate, unresolved))
    }
}

/// 把 resolver 失败精确分类到“模型缺失（Login 可修）”与“窗口缺失（Model 面板）”。
fn resolver_error_for_candidate(
    candidate: &PeriConfig,
    alias: &'static str,
    error: peri_config::provider::ProviderError,
) -> LoginError {
    use peri_config::provider::ProviderError;
    match error {
        ProviderError::MissingModelCapacity => {
            if alias_has_model(candidate, alias) {
                profile_window_missing(alias)
            } else {
                profile_model_missing(alias)
            }
        }
        other => profile_resolver_error(alias, other),
    }
}

fn alias_has_model(cfg: &PeriConfig, alias: &str) -> bool {
    let Some(profile) = cfg.config.profiles.get(alias) else {
        return false;
    };
    if profile
        .model
        .as_deref()
        .is_some_and(|model| !model.trim().is_empty())
    {
        return true;
    }
    effective_owner(cfg, profile)
        .and_then(|provider| provider.models.get_model(alias))
        .is_some_and(|model| !model.trim().is_empty())
}

/// 新建 plan Provider 的默认模型来源：先同类 plan Provider，再其他 Responses
/// Provider；每类选非空档位最多者（同分取配置顺序靠前者）。没有可靠来源返回
/// `None`，绝不凭空生成模型 ID。
fn saved_model_mapping(cfg: &PeriConfig) -> Option<&ProviderModels> {
    let providers = cfg.config.providers.iter();
    best_model_mapping(providers.clone().filter(|provider| {
        provider.provider_type == "openai_responses"
            && matches!(provider.auth, Some(ProviderAuthConfig::ChatGptPlan { .. }))
    }))
    .or_else(|| {
        best_model_mapping(
            providers.filter(|provider| provider.provider_type == "openai_responses"),
        )
    })
}

fn best_model_mapping<'a>(
    providers: impl Iterator<Item = &'a ProviderConfig>,
) -> Option<&'a ProviderModels> {
    providers
        .map(|provider| &provider.models)
        .filter(|models| !models_are_empty(models))
        .fold(
            None,
            |best: Option<&'a ProviderModels>, models| match best {
                Some(current) if model_count(current) >= model_count(models) => Some(current),
                _ => Some(models),
            },
        )
}

fn models_are_empty(models: &ProviderModels) -> bool {
    models.fable.trim().is_empty()
        && models.opus.trim().is_empty()
        && models.sonnet.trim().is_empty()
        && models.haiku.trim().is_empty()
}

fn model_count(models: &ProviderModels) -> usize {
    [&models.fable, &models.opus, &models.sonnet, &models.haiku]
        .iter()
        .filter(|model| !model.trim().is_empty())
        .count()
}

#[cfg(test)]
#[path = "edit_state_test.rs"]
mod tests;
