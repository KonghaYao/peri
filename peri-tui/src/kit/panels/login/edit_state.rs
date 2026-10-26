//! Login 编辑草稿：typed 选择、四档 profile 基线与候选构造。
//!
//! 本模块不读全局句柄、不写磁盘：所有输入从进入表单时捕获的 [`PeriConfig`] 基线
//! 派生，候选构造与校验都是纯函数，便于在单元测试中覆盖四档隔离、回退归属和
//! 类型切换边界。落盘与发布由 `config_store` 负责。

use crate::config::ConfigurationRevision;
use crate::i18n;
use crate::kit::panels::shared::context_window::parse_context_window;

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

/// 单档草稿。四档各持一份，切换 selector 不丢输入。
#[derive(Debug, Clone)]
pub(super) struct ProfileDraft {
    pub(super) alias: &'static str,
    /// 进入表单时该档的有效归属 provider id（含空引用回退首 provider）。
    pub(super) baseline_owner: String,
    /// 进入表单时该档是否有效绑定到正在编辑的 provider。
    pub(super) bound: bool,
    /// 是否通过“空 provider → 首 provider”回退绑定（显示用，不改写引用）。
    pub(super) fallback_bound: bool,
    /// 新建 provider 时该档是否被显式勾选绑定。
    pub(super) bind: bool,
    /// 新建 provider 且基线没有任何 provider 时，空引用会回退到新 provider。
    pub(super) will_fallback: bool,
    pub(super) baseline_model: Option<String>,
    pub(super) baseline_window: Option<u32>,
    /// 进入表单时该档在基线 provider 映射下的实际模型（用于映射变化重确认）。
    pub(super) baseline_effective_model: String,
    pub(super) model: String,
    pub(super) model_dirty: bool,
    pub(super) window: String,
    pub(super) window_dirty: bool,
}

impl ProfileDraft {
    /// 本次保存是否会把该档归属到正在编辑/新建的 provider。
    pub(super) fn will_bind(&self, is_new: bool) -> bool {
        if is_new {
            self.bind || self.will_fallback
        } else {
            self.bound || self.bind
        }
    }

    /// 绑定状态是否只读：既有有效绑定、首 provider 回退绑定不可在 Login 取消。
    pub(super) fn bind_read_only(&self, is_new: bool) -> bool {
        if is_new {
            self.will_fallback
        } else {
            self.bound || self.will_fallback
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
    /// 进入表单时的已保存类型（未知类型为 None）。
    pub(super) baseline_kind: Option<ProviderKind>,
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
    pub(super) profiles: Vec<ProfileDraft>,
    pub(super) selected_profile: usize,
    pub(super) active_alias: String,
    pub(super) baseline: PeriConfig,
    pub(super) expected_revision: Option<ConfigurationRevision>,
    pub(super) inline_error: Option<LoginError>,
    /// 显式类型更改子视图的候选类型（None 表示未打开）。
    pub(super) type_change_candidate: Option<ProviderKind>,
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

/// provider 映射下的实际模型（fable 空值回退 opus，与 core 一致）。
fn mapping_model(models: &ProviderModels, alias: &str) -> String {
    models
        .get_model(alias)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("")
        .to_string()
}

fn profile_for_alias<'a>(cfg: &'a PeriConfig, alias: &str) -> &'a ProfileConfig {
    cfg.config
        .profiles
        .get(alias)
        .expect("Profiles always contains the four fixed aliases")
}

/// 表单的“当前激活档位”：空 active_alias 与 Model 面板一致退守 opus，但不改配置。
fn form_active_alias(cfg: &PeriConfig) -> String {
    if cfg.config.active_alias.is_empty() {
        Profiles::ALL[1].to_string()
    } else {
        cfg.config.active_alias.clone()
    }
}

impl LoginEditState {
    pub(super) fn is_new(&self) -> bool {
        self.original_provider_id.is_empty()
    }

    /// 新建 provider：只默认勾选进入表单时的 active alias。
    pub(super) fn new_provider(cfg: &PeriConfig, revision: Option<ConfigurationRevision>) -> Self {
        let active_alias = form_active_alias(cfg);
        let providers_empty = cfg.config.providers.is_empty();
        let profiles = Profiles::ALL
            .iter()
            .map(|alias| {
                let profile = profile_for_alias(cfg, alias);
                let owner = effective_owner(cfg, profile)
                    .map(|provider| provider.id.clone())
                    .unwrap_or_default();
                ProfileDraft {
                    alias,
                    baseline_owner: owner,
                    bound: false,
                    fallback_bound: false,
                    bind: *alias == active_alias,
                    will_fallback: profile.provider.is_empty() && providers_empty,
                    baseline_model: profile.model.clone(),
                    baseline_window: profile.context_window,
                    baseline_effective_model: String::new(),
                    model: String::new(),
                    model_dirty: false,
                    window: String::new(),
                    window_dirty: false,
                }
            })
            .collect();
        let selected_profile = Profiles::ALL
            .iter()
            .position(|alias| *alias == active_alias)
            .unwrap_or(0);
        Self {
            original_provider_id: String::new(),
            baseline_kind: None,
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
            profiles,
            selected_profile,
            active_alias,
            baseline: cfg.clone(),
            expected_revision: revision,
            inline_error: None,
            type_change_candidate: None,
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
        let active_alias = form_active_alias(cfg);
        let profiles = Profiles::ALL
            .iter()
            .map(|alias| {
                let profile = profile_for_alias(cfg, alias);
                let owner = effective_owner(cfg, profile)
                    .map(|owner| owner.id.clone())
                    .unwrap_or_default();
                let bound = owner == provider.id;
                let fallback_bound = bound && profile.provider.is_empty();
                let baseline_effective_model = mapping_model(&provider.models, alias);
                let model = profile
                    .model
                    .as_ref()
                    .filter(|model| !model.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| baseline_effective_model.clone());
                let window = if bound {
                    profile
                        .context_window
                        .map(|window| window.to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                ProfileDraft {
                    alias,
                    baseline_owner: owner,
                    bound,
                    fallback_bound,
                    bind: bound,
                    will_fallback: false,
                    baseline_model: profile.model.clone(),
                    baseline_window: profile.context_window,
                    baseline_effective_model,
                    model,
                    model_dirty: false,
                    window,
                    window_dirty: false,
                }
            })
            .collect();
        let selected_profile = Profiles::ALL
            .iter()
            .position(|alias| *alias == active_alias)
            .unwrap_or(0);
        Some(Self {
            original_provider_id: provider.id.clone(),
            baseline_kind,
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
            profiles,
            selected_profile,
            active_alias,
            baseline: cfg.clone(),
            expected_revision: revision,
            inline_error: None,
            type_change_candidate: None,
        })
    }

    pub(super) fn selected(&self) -> &ProfileDraft {
        &self.profiles[self.selected_profile]
    }

    pub(super) fn selected_mut(&mut self) -> &mut ProfileDraft {
        &mut self.profiles[self.selected_profile]
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
    }

    /// 切换到 plan 模式时补齐宿主默认 store/account（不创建目录）。
    pub(super) fn ensure_plan_defaults(&mut self, default_store: Option<PathBuf>) {
        self.plan.ensure_defaults(default_store);
    }

    /// 动态可见字段：导航、绘制、粘贴与错误定位共用。
    pub(super) fn visible_fields(&self) -> Vec<LoginField> {
        let mut fields = vec![LoginField::ProviderType, LoginField::ProviderId];
        if self.is_responses() {
            fields.push(LoginField::AuthMode);
        }
        if self.is_plan_mode() {
            fields.push(LoginField::PlanStore);
            fields.push(LoginField::PlanAccount);
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
        if self.is_responses() {
            fields.push(LoginField::ProfileSelector);
            fields.push(LoginField::ProfileBind);
            if self.selected().will_bind(self.is_new()) {
                fields.push(LoginField::ProfileModel);
                fields.push(LoginField::ProfileWindow);
            }
        }
        fields.push(LoginField::Confirm);
        fields
    }

    /// 焦点字段被隐藏（切换类型/绑定后）时收敛到最近的可操作字段。
    pub(super) fn ensure_focus_visible(&self, focus: &mut LoginField) {
        let fields = self.visible_fields();
        if fields.contains(focus) {
            return;
        }
        *focus = fields
            .get(
                fields
                    .iter()
                    .position(|field| field == &LoginField::Confirm)
                    .unwrap_or(0),
            )
            .copied()
            .unwrap_or(LoginField::Confirm);
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
            LoginField::ProfileModel => self.selected().model.clone(),
            LoginField::ProfileWindow => self.selected().window.clone(),
            LoginField::ProviderType
            | LoginField::AuthMode
            | LoginField::AuthActions
            | LoginField::ProfileSelector
            | LoginField::ProfileBind
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
            LoginField::ProfileModel => Some(&mut self.selected_mut().model),
            LoginField::ProfileWindow => Some(&mut self.selected_mut().window),
            LoginField::ProviderType
            | LoginField::AuthMode
            | LoginField::AuthActions
            | LoginField::ProfileSelector
            | LoginField::ProfileBind
            | LoginField::Confirm => None,
        }
    }

    /// 标记字段发生过实际编辑（光标移动不算）。
    pub(super) fn mark_field_edited(&mut self, field: LoginField) {
        match field {
            LoginField::ProfileModel => self.selected_mut().model_dirty = true,
            LoginField::ProfileWindow => self.selected_mut().window_dirty = true,
            _ => {}
        }
    }

    /// provider 默认映射字段（Fable/Opus/Sonnet/Haiku）是否属于当前字段。
    pub(super) fn is_mapping_field(field: LoginField) -> bool {
        matches!(
            field,
            LoginField::FableModel
                | LoginField::OpusModel
                | LoginField::SonnetModel
                | LoginField::HaikuModel
        )
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
            self.sync_profile_models();
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
            self.sync_profile_models();
        }
    }

    pub(super) fn cancel_type_change(&mut self) {
        self.type_change_candidate = None;
    }

    pub(super) fn cycle_profile(&mut self, forward: bool) {
        let len = self.profiles.len();
        self.selected_profile = if forward {
            (self.selected_profile + 1) % len
        } else {
            (self.selected_profile + len - 1) % len
        };
    }

    /// 绑定复选项：既有绑定与首 provider 回退只读。
    pub(super) fn toggle_bind(&mut self) {
        let is_new = self.is_new();
        if self.selected().bind_read_only(is_new) {
            return;
        }
        let alias = self.selected().alias;
        let bind = !self.selected().bind;
        let mapped = mapping_model(&self.models, alias);
        let profile = self.selected_mut();
        profile.bind = bind;
        if bind && !profile.model_dirty {
            profile.model = mapped;
        }
    }

    /// provider 默认映射变化后同步“未手动编辑且无显式 override”的档位草稿。
    pub(super) fn sync_profile_models(&mut self) {
        for profile in &mut self.profiles {
            if profile.model_dirty {
                continue;
            }
            profile.model = if profile.bound {
                profile
                    .baseline_model
                    .as_ref()
                    .filter(|model| !model.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| mapping_model(&self.models, profile.alias))
            } else {
                mapping_model(&self.models, profile.alias)
            };
        }
    }

    /// 类型更改影响到的档位（有效归属为当前 provider 的档位）。
    pub(super) fn affected_aliases(&self) -> Vec<&'static str> {
        self.profiles
            .iter()
            .filter(|profile| profile.will_bind(self.is_new()))
            .map(|profile| profile.alias)
            .collect()
    }

    /// 渲染用：该档是否因 provider 映射变化需要重新确认容量。
    pub(super) fn profile_needs_reconfirm(&self, profile: &ProfileDraft) -> bool {
        if profile.model_dirty
            && !profile.window_dirty
            && self.baseline_kind == Some(ProviderKind::OpenAiResponses)
        {
            return true;
        }
        if !profile.bound
            || profile.model_dirty
            || profile.window_dirty
            || self.baseline_kind != Some(ProviderKind::OpenAiResponses)
            || profile
                .baseline_model
                .as_ref()
                .is_some_and(|baseline| !baseline.trim().is_empty())
        {
            return false;
        }
        mapping_model(&self.models, profile.alias) != profile.baseline_effective_model
    }

    /// 渲染用：本次将更新的档位摘要。
    pub(super) fn profiles_to_update(&self) -> String {
        if !self.is_responses() {
            return i18n::tr("login-profile-none");
        }
        let aliases = self.affected_aliases();
        if aliases.is_empty() {
            i18n::tr("login-profile-none")
        } else {
            aliases.join(", ")
        }
    }

    fn owned_profiles(&self) -> Vec<&ProfileDraft> {
        self.profiles
            .iter()
            .filter(|profile| profile.will_bind(self.is_new()))
            .collect()
    }

    fn type_changed_to_responses(&self) -> bool {
        self.is_responses() && self.baseline_kind != Some(ProviderKind::OpenAiResponses)
    }

    /// 构造保存候选（纯函数，不落盘）。
    pub(super) fn build_candidate(&self) -> Result<PeriConfig, LoginError> {
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
            self.plan.apply(&mut provider)?;
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
        let candidate_mappings = candidate.config.providers[provider_index].models.clone();

        if self.is_responses() {
            for profile_draft in self.owned_profiles() {
                let alias = profile_draft.alias;
                let model = self.resolved_model(profile_draft, &candidate_mappings);
                if model.is_empty() {
                    return Err(LoginError::profile(
                        LoginField::ProfileModel,
                        alias,
                        i18n::tr_args(
                            "login-error-model-required",
                            &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
                        ),
                    ));
                }
                let window = self.resolved_window(profile_draft).ok_or_else(|| {
                    LoginError::profile(
                        LoginField::ProfileWindow,
                        alias,
                        i18n::tr_args(
                            "login-error-context-window",
                            &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
                        ),
                    )
                })?;
                if profile_draft.model_dirty && !profile_draft.window_dirty {
                    return Err(LoginError::profile(
                        LoginField::ProfileWindow,
                        alias,
                        i18n::tr("login-profile-reconfirm"),
                    ));
                }
                if self.needs_window_reconfirmation(profile_draft, &model) {
                    return Err(LoginError::profile(
                        LoginField::ProfileWindow,
                        alias,
                        i18n::tr("login-profile-reconfirm"),
                    ));
                }

                let baseline_profile = profile_for_alias(&self.baseline, alias);
                let mut profile = baseline_profile.clone();
                let write_provider = self.write_provider(profile_draft);
                if write_provider {
                    profile.provider = provider_id.to_string();
                }
                if profile_draft.model_dirty || write_provider {
                    profile.model = Some(model);
                }
                profile.context_window = Some(window);
                if let Some(target) = candidate.config.profiles.get_mut(alias) {
                    *target = profile;
                }
            }
        }

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
        if self.is_responses() {
            let provider = &candidate.config.providers[provider_index];
            if let Err(error) = peri_config::provider::validate_responses_provider(provider) {
                return Err(self.provider_error(error));
            }
            // profile 侧继续走 core resolver，保证 UI 与运行端接受同一组配置。
            for alias in self.affected_aliases() {
                match peri_config::provider::try_resolve_for_alias(&candidate, alias) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Err(LoginError::profile(
                            LoginField::ProfileModel,
                            alias,
                            i18n::tr_args(
                                "login-error-model-required",
                                &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
                            ),
                        ));
                    }
                    Err(error) => return Err(self.profile_resolver_error(alias, error)),
                }
            }
        }

        Ok(candidate)
    }

    fn resolved_model(
        &self,
        profile: &ProfileDraft,
        candidate_mappings: &ProviderModels,
    ) -> String {
        if profile.model_dirty {
            return profile.model.trim().to_string();
        }
        if profile.bound
            && let Some(model) = profile
                .baseline_model
                .as_ref()
                .map(|model| model.trim())
                .filter(|model| !model.is_empty())
        {
            return model.to_string();
        }
        mapping_model(candidate_mappings, profile.alias)
    }

    fn resolved_window(&self, profile: &ProfileDraft) -> Option<u32> {
        if profile.window_dirty {
            return parse_context_window(&profile.window);
        }
        if profile.bound
            && self.baseline_kind == Some(ProviderKind::OpenAiResponses)
            && !self.type_changed_to_responses()
        {
            return profile.baseline_window.filter(|window| *window > 0);
        }
        None
    }

    /// 本次保存是否把某档显式写到此 provider。
    fn write_provider(&self, profile: &ProfileDraft) -> bool {
        if self.is_new() {
            // 首 provider 回退不写显式引用：新建 provider 默认只显式绑定 active alias。
            return profile.bind;
        }
        if profile.bind && !profile.bound {
            return true;
        }
        // 已有 provider 转为 Responses：把本次需补齐的档位归属显式化。
        profile.bound && self.type_changed_to_responses()
    }

    /// 依赖默认映射的档位在映射模型变化后必须重新确认容量。
    fn needs_window_reconfirmation(&self, profile: &ProfileDraft, model: &str) -> bool {
        if !profile.bound
            || profile.model_dirty
            || profile.window_dirty
            || self.baseline_kind != Some(ProviderKind::OpenAiResponses)
            || profile
                .baseline_model
                .as_ref()
                .is_some_and(|baseline| !baseline.trim().is_empty())
        {
            return false;
        }
        model != profile.baseline_effective_model
    }

    fn provider_error(&self, error: peri_config::provider::ProviderError) -> LoginError {
        use peri_config::provider::ProviderError;
        let field = match error {
            ProviderError::InvalidEndpoint => LoginField::BaseUrl,
            ProviderError::InvalidSubscriptionReference => LoginField::PlanStore,
            ProviderError::InvalidSubscriptionEndpoint => LoginField::AuthActions,
            ProviderError::MissingApiKey => LoginField::ApiKey,
            ProviderError::UnknownType
            | ProviderError::UnexpectedAuth
            | ProviderError::MissingModelCapacity => LoginField::ProviderType,
        };
        LoginError::field(
            field,
            i18n::tr_args(
                "login-error-provider",
                &[(
                    "error".to_string(),
                    fluent_bundle::FluentValue::from(error.to_string().as_str()),
                )],
            ),
        )
    }

    fn profile_resolver_error(
        &self,
        alias: &'static str,
        error: peri_config::provider::ProviderError,
    ) -> LoginError {
        use peri_config::provider::ProviderError;
        match error {
            ProviderError::MissingModelCapacity => LoginError::profile(
                LoginField::ProfileWindow,
                alias,
                i18n::tr_args(
                    "login-error-context-window",
                    &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
                ),
            ),
            other => self.provider_error(other),
        }
    }
}

/// ASCII 十进制正整数，trim 后不接受符号、后缀、内部空格、Unicode 数字或换行。
#[cfg(test)]
pub(super) fn parse_window(raw: &str) -> Result<u32, ()> {
    parse_context_window(raw).ok_or(())
}

#[cfg(test)]
#[path = "edit_state_test.rs"]
mod tests;
