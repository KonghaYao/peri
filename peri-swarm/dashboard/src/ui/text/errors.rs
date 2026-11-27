//! Config and API error wording shown to the user.
//!
//! The underlying error types keep their `Display` implementations for logs
//! and internal diagnostics; every string a form, banner or page shows is
//! produced here so a future locale layer has one place to translate.

use crate::api::{ApiError, ApiErrorCause};
use crate::config::{BaseUrlError, ConfigError, InvalidConfig};

use super::{api_error_detail, config_save_failed};

/// Configuration failure detail, without a page-specific prefix.
#[must_use]
pub fn config_error_detail(error: &ConfigError) -> String {
    match error {
        ConfigError::HomeDirUnavailable => "the home directory is not available".to_owned(),
        ConfigError::InvalidBaseUrl(inner) => base_url_error(inner),
        ConfigError::EmptyApiKey => "API key must not be empty".to_owned(),
        ConfigError::CreateDir { path, source } => format!(
            "could not prepare the config directory {}: {source}",
            path.display()
        ),
        ConfigError::Read { path, source } => {
            format!(
                "could not read the config file {}: {source}",
                path.display()
            )
        }
        ConfigError::Corrupt { path } => {
            format!("the config file {} is not valid TOML", path.display())
        }
        ConfigError::Invalid { path, reason } => format!(
            "the config file {} is invalid: {}",
            path.display(),
            invalid_config(reason)
        ),
        ConfigError::Write { path, source } => format!(
            "could not write the config file {}: {source}",
            path.display()
        ),
        ConfigError::Delete { path, source } => format!(
            "could not delete the config file {}: {source}",
            path.display()
        ),
    }
}

/// Complete message shown when saving credentials fails.
#[must_use]
pub fn config_error(error: &ConfigError) -> String {
    config_save_failed(&config_error_detail(error))
}

/// Base URL validation message shown next to the field.
#[must_use]
pub fn base_url_error(error: &BaseUrlError) -> String {
    match error {
        BaseUrlError::Empty => "Base URL must not be empty.".to_owned(),
        BaseUrlError::Invalid => {
            "Base URL must be an absolute http or https URL with a host.".to_owned()
        }
        BaseUrlError::UnsupportedScheme => "Base URL must use http or https.".to_owned(),
        BaseUrlError::UserInfo => "Base URL must not contain credentials.".to_owned(),
        BaseUrlError::QueryOrFragment => {
            "Base URL must not contain a query or fragment.".to_owned()
        }
        BaseUrlError::UnsupportedPath => "Base URL path must be empty or /v1.".to_owned(),
    }
}

/// Reason a saved config file was rejected, in user-facing wording.
fn invalid_config(reason: &InvalidConfig) -> String {
    match reason {
        InvalidConfig::BaseUrl(inner) => base_url_error(inner),
        InvalidConfig::ApiKey => "API key must not be empty".to_owned(),
    }
}

/// Complete API failure description, including the gateway code and status.
#[must_use]
pub fn api_error(error: &ApiError) -> String {
    match error.cause() {
        Some(cause) => api_failure_message(cause, error.message()),
        None => api_error_detail(error.code(), error.status(), error.message()),
    }
}

/// Wording for the transport and decoding steps the API client records.
fn api_failure_message(cause: ApiErrorCause, detail: &str) -> String {
    match cause {
        ApiErrorCause::ClientBuild => format!("Cannot reach the gateway: {detail}"),
        ApiErrorCause::Request => format!("Gateway request failed: {detail}"),
        ApiErrorCause::ResponseRead => format!("Gateway response could not be read: {detail}"),
        ApiErrorCause::ResponseDecode => {
            format!("Gateway response could not be decoded: {detail}")
        }
    }
}

/// Message used when a non-2xx body carries no JSON error envelope.
#[must_use]
pub fn api_default_message(status: u16) -> &'static str {
    match status {
        400 => "the gateway rejected the request",
        401 => "authentication failed",
        404 => "the requested session was not found",
        409 => "the request conflicts with the current session state",
        422 => "the gateway rejected the payload as invalid",
        502 => "the gateway could not reach its host",
        300..=399 => "the gateway returned a redirect",
        500..=599 => "the gateway failed to process the request",
        _ => "the gateway returned an unexpected status",
    }
}

/// Summary line for a session's creation and last activity.
#[must_use]
pub fn created_last_active(created: &str, last_active: &str) -> String {
    format!("created {created} · last active {last_active}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigError;

    #[test]
    fn api_error_carries_the_gateway_code_and_status() {
        let error = ApiError::from_status(
            409,
            br#"{"error":{"code":"conflict","message":"logical id already used"}}"#,
        );
        assert_eq!(
            api_error(&error),
            "logical id already used (conflict, HTTP 409)"
        );
        assert_eq!(
            api_error(&ApiError::local("model must not be empty")),
            "model must not be empty"
        );
    }

    #[test]
    fn transport_and_decode_failures_are_worded_here() {
        assert_eq!(
            api_failure_message(ApiErrorCause::ClientBuild, "builder error"),
            "Cannot reach the gateway: builder error"
        );
        assert_eq!(
            api_failure_message(ApiErrorCause::Request, "connection refused"),
            "Gateway request failed: connection refused"
        );
        assert_eq!(
            api_failure_message(ApiErrorCause::ResponseRead, "connection reset"),
            "Gateway response could not be read: connection reset"
        );
        assert_eq!(
            api_failure_message(ApiErrorCause::ResponseDecode, "expected value"),
            "Gateway response could not be decoded: expected value"
        );
    }

    #[test]
    fn config_error_detail_is_user_facing_and_save_error_is_prefixed() {
        let error = ConfigError::EmptyApiKey;
        assert_eq!(config_error_detail(&error), "API key must not be empty");
        assert_eq!(
            config_error(&error),
            "Could not save credentials: API key must not be empty"
        );
    }

    #[test]
    fn base_url_errors_are_complete_sentences() {
        assert_eq!(
            base_url_error(&BaseUrlError::UnsupportedScheme),
            "Base URL must use http or https."
        );
        assert_eq!(
            base_url_error(&BaseUrlError::UnsupportedPath),
            "Base URL path must be empty or /v1."
        );
    }

    #[test]
    fn created_last_active_is_formatted_in_one_place() {
        assert_eq!(
            created_last_active("just now", "2s ago"),
            "created just now · last active 2s ago"
        );
    }

    #[test]
    fn default_messages_cover_every_status_class() {
        assert_eq!(
            api_default_message(409),
            "the request conflicts with the current session state"
        );
        assert_eq!(
            api_default_message(502),
            "the gateway could not reach its host"
        );
        assert_eq!(api_default_message(302), "the gateway returned a redirect");
    }
}
