//! Login 草稿的 provider/profile 错误映射。
//!
//! 与 `edit_state` 分离以保持单文件规模。隐藏字段（account/subject/clientId）的
//! 错误必须落到仍然可见、可操作的目标上；`visible_fields` 是唯一来源。

use crate::i18n;

use super::edit_types::{LoginError, LoginField};

/// Provider 校验错误 → 可见字段 + 用户可读文案。
///
/// `InvalidSubscriptionReference` 不再使用 core 的字段级措辞：用户看不到账号/身份
/// 引用，只给出可恢复的“重新登录”指引。
pub(super) fn provider_error(error: peri_config::provider::ProviderError) -> LoginError {
    use peri_config::provider::ProviderError;
    match error {
        ProviderError::InvalidSubscriptionReference => LoginError::auth_reference(
            LoginField::AuthActions,
            i18n::tr("login-error-credentials-incomplete"),
        ),
        other => {
            let field = provider_error_field(&other);
            LoginError::field(field, provider_error_message(other))
        }
    }
}

/// profile resolver 错误：Login 不再编辑档位，统一指向 Model 面板。
pub(super) fn profile_resolver_error(
    alias: &'static str,
    error: peri_config::provider::ProviderError,
) -> LoginError {
    use peri_config::provider::ProviderError;
    match error {
        ProviderError::MissingModelCapacity => profile_window_missing(alias),
        other => LoginError::field(
            LoginField::ProviderType,
            i18n::tr_args(
                "login-error-profile-resolver",
                &[
                    ("alias".to_string(), fluent_bundle::FluentValue::from(alias)),
                    (
                        "error".to_string(),
                        fluent_bundle::FluentValue::from(other.to_string().as_str()),
                    ),
                ],
            ),
        ),
    }
}

/// 档位没有可用模型：指向对应的 Login 默认模型字段，本轮即可修复。
pub(super) fn profile_model_missing(alias: &'static str) -> LoginError {
    LoginError::field(
        default_model_field(alias),
        i18n::tr_args(
            "login-error-profile-model-missing",
            &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
        ),
    )
}

/// 档位缺少上下文窗口：Login 不写 contextWindow，只指向 Model 面板。
pub(super) fn profile_window_missing(alias: &'static str) -> LoginError {
    LoginError::field(
        LoginField::ProviderType,
        i18n::tr_args(
            "login-error-profile-window-missing",
            &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
        ),
    )
}

/// 档位绑定不可解析（候选里找不到归属 Provider）。
pub(super) fn profile_provider_missing(alias: &'static str) -> LoginError {
    LoginError::field(
        LoginField::ProviderType,
        i18n::tr_args(
            "login-error-profile-provider-missing",
            &[("alias".to_string(), fluent_bundle::FluentValue::from(alias))],
        ),
    )
}

/// Login 的四个默认模型字段与档位 alias 一一对应。
fn default_model_field(alias: &str) -> LoginField {
    match alias {
        "fable" => LoginField::FableModel,
        "opus" => LoginField::OpusModel,
        "sonnet" => LoginField::SonnetModel,
        "haiku" => LoginField::HaikuModel,
        _ => LoginField::ProviderType,
    }
}

/// `PlanAccount` 错误重映射到动作组：账号行不再渲染，不能让焦点落到不可见字段。
pub(super) fn remap_hidden_field(mut error: LoginError) -> LoginError {
    if error.field == LoginField::PlanAccount {
        error.field = LoginField::AuthActions;
    }
    error
}

fn provider_error_field(error: &peri_config::provider::ProviderError) -> LoginField {
    use peri_config::provider::ProviderError;
    match error {
        ProviderError::InvalidEndpoint => LoginField::BaseUrl,
        ProviderError::InvalidSubscriptionReference => LoginField::AuthActions,
        ProviderError::InvalidSubscriptionEndpoint => LoginField::AuthActions,
        ProviderError::MissingApiKey => LoginField::ApiKey,
        ProviderError::UnknownType
        | ProviderError::UnexpectedAuth
        | ProviderError::MissingModelCapacity => LoginField::ProviderType,
    }
}

fn provider_error_message(error: peri_config::provider::ProviderError) -> String {
    i18n::tr_args(
        "login-error-provider",
        &[(
            "error".to_string(),
            fluent_bundle::FluentValue::from(error.to_string().as_str()),
        )],
    )
}
