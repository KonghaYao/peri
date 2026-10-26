//! Login 动态字段身份与字段级错误。
//!
//! 与 `edit_state` 分离以保持单文件规模；导航、绘制、粘贴与错误定位共用
//! [`LoginField`]，不维护第二套字段枚举。

/// 动态字段身份。`visible_fields`、导航、绘制与错误定位共用同一枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoginField {
    ProviderType,
    ProviderId,
    AuthMode,
    PlanStore,
    PlanAccount,
    AuthActions,
    BaseUrl,
    ApiKey,
    FableModel,
    OpusModel,
    SonnetModel,
    HaikuModel,
    ProfileSelector,
    ProfileBind,
    ProfileModel,
    ProfileWindow,
    Confirm,
}

impl LoginField {
    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            Self::ProviderType => "login-field-type",
            Self::ProviderId => "login-field-name",
            Self::AuthMode => "login-field-auth-mode",
            Self::PlanStore => "login-field-store",
            Self::PlanAccount => "login-field-account",
            Self::AuthActions => "login-auth-actions",
            Self::BaseUrl => "login-field-base-url",
            Self::ApiKey => "login-field-api-key",
            Self::FableModel => "login-field-fable-model",
            Self::OpusModel => "login-field-opus-model",
            Self::SonnetModel => "login-field-sonnet-model",
            Self::HaikuModel => "login-field-haiku-model",
            Self::ProfileSelector => "login-profile-selector",
            Self::ProfileBind => "login-profile-bind",
            Self::ProfileModel => "login-profile-model",
            Self::ProfileWindow => "login-profile-context-window",
            Self::Confirm => "login-confirm",
        }
    }

    pub(super) fn is_text(self) -> bool {
        matches!(
            self,
            Self::ProviderId
                | Self::PlanStore
                | Self::PlanAccount
                | Self::BaseUrl
                | Self::ApiKey
                | Self::FableModel
                | Self::OpusModel
                | Self::SonnetModel
                | Self::HaikuModel
                | Self::ProfileModel
                | Self::ProfileWindow
        )
    }
}

/// 错误类别：只有真正的 revision/authority 冲突才提示“重新载入”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoginErrorKind {
    /// 字段校验/输入拒绝；重新载入不会改变结果。
    Field,
    /// 保存时 expected revision 已过期，必须重读权威配置。
    Conflict,
}

/// 保存前的字段级错误（渲染为固定行，同时聚焦对应字段）。
#[derive(Debug, Clone)]
pub(super) struct LoginError {
    pub(super) field: LoginField,
    pub(super) message: String,
    pub(super) alias: Option<&'static str>,
    pub(super) kind: LoginErrorKind,
}

impl LoginError {
    pub(super) fn field(field: LoginField, message: String) -> Self {
        Self {
            field,
            message,
            alias: None,
            kind: LoginErrorKind::Field,
        }
    }

    pub(super) fn profile(field: LoginField, alias: &'static str, message: String) -> Self {
        Self {
            field,
            message,
            alias: Some(alias),
            kind: LoginErrorKind::Field,
        }
    }

    /// `expected revision` 过期：唯一允许显示“重新载入”提示的类别。
    pub(super) fn conflict(message: String) -> Self {
        Self {
            field: LoginField::Confirm,
            message,
            alias: None,
            kind: LoginErrorKind::Conflict,
        }
    }
}
