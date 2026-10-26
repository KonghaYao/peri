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
        )
    }
}

/// 错误类别：只有真正的 revision/authority 冲突才提示“重新载入”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoginErrorKind {
    /// 字段校验/输入拒绝；重新载入不会改变结果。
    Field,
    /// plan 模式缺少可用身份；认证状态变化或自动采用后应清除。
    IdentityRequired,
    /// 凭据引用不可用；认证状态变化或显式采用后应清除。
    AuthReference,
    /// 保存时 expected revision 已过期，必须重读权威配置。
    Conflict,
}

/// 保存前的字段级错误（渲染为固定行，同时聚焦对应字段）。
#[derive(Debug, Clone)]
pub(super) struct LoginError {
    pub(super) field: LoginField,
    pub(super) message: String,
    pub(super) kind: LoginErrorKind,
}

impl LoginError {
    pub(super) fn field(field: LoginField, message: String) -> Self {
        Self {
            field,
            message,
            kind: LoginErrorKind::Field,
        }
    }

    /// `expected revision` 过期：唯一允许显示“重新载入”提示的类别。
    pub(super) fn conflict(message: String) -> Self {
        Self {
            field: LoginField::Confirm,
            message,
            kind: LoginErrorKind::Conflict,
        }
    }

    /// Plan 身份缺失；保存门据此做状态化文案与自动采用。
    pub(super) fn identity_required(field: LoginField, message: String) -> Self {
        Self {
            field,
            message,
            kind: LoginErrorKind::IdentityRequired,
        }
    }

    /// 凭据引用不可用；与身份缺失分开，便于保存门抑制重复通知。
    pub(super) fn auth_reference(field: LoginField, message: String) -> Self {
        Self {
            field,
            message,
            kind: LoginErrorKind::AuthReference,
        }
    }
}
