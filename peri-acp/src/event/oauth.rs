//! Capability-gated MCP OAuth wire boundary.
//!
//! The authorization URL is deliberately held by a non-`Debug`, non-`Serialize`
//! value and is converted to JSON only at the transport call site. This keeps it
//! out of generic event persistence and diagnostic surfaces.

use serde_json::{json, Value};
use url::{Host, Url};

pub const OAUTH_SCHEMA_VERSION: u32 = 1;
pub const OAUTH_IDENTIFIER_MAX_BYTES: usize = 128;
pub const OAUTH_AUTHORIZATION_URL_MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthWireStatus {
    AuthorizationNeeded,
    Completed,
    Failed,
    Cancelled,
    Restored,
}

impl OAuthWireStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthorizationNeeded => "authorization_needed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Restored => "restored",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFailureClass {
    CallbackUnavailable,
    CallbackTimeout,
    ProviderRejected,
    ConnectionFailed,
    Internal,
}

impl OAuthFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallbackUnavailable => "callback_unavailable",
            Self::CallbackTimeout => "callback_timeout",
            Self::ProviderRejected => "provider_rejected",
            Self::ConnectionFailed => "connection_failed",
            Self::Internal => "internal",
        }
    }
}

pub struct ValidatedAuthorizationUrl(String);

impl ValidatedAuthorizationUrl {
    pub fn parse(raw: String) -> Result<Self, OAuthWireError> {
        if raw.is_empty() || raw.len() > OAUTH_AUTHORIZATION_URL_MAX_BYTES {
            return Err(OAuthWireError::InvalidAuthorizationUrl);
        }
        let parsed = Url::parse(&raw).map_err(|_| OAuthWireError::InvalidAuthorizationUrl)?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(OAuthWireError::InvalidAuthorizationUrl);
        }
        let allowed = match parsed.scheme() {
            "https" => parsed.host().is_some(),
            "http" => match parsed.host() {
                Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                Some(Host::Ipv4(address)) => address.is_loopback(),
                Some(Host::Ipv6(address)) => address.is_loopback(),
                None => false,
            },
            _ => false,
        };
        if !allowed {
            return Err(OAuthWireError::InvalidAuthorizationUrl);
        }
        Ok(Self(raw))
    }

    fn into_inner(self) -> String {
        self.0
    }
}

pub struct OAuthWireNotification {
    flow_id: String,
    server_name: String,
    status: OAuthWireStatus,
    authorization_url: Option<ValidatedAuthorizationUrl>,
    failure_class: Option<OAuthFailureClass>,
}

/// Host assembly → transport consumer event. This internal carrier may contain
/// a URL or raw local error, so it intentionally implements neither `Debug` nor
/// serde traits.
pub enum HostOAuthEvent {
    AuthorizationNeeded {
        flow_id: String,
        server_name: String,
        authorization_url: String,
    },
    Completed {
        flow_id: String,
        server_name: String,
    },
    Failed {
        flow_id: String,
        server_name: String,
        failure_class: OAuthFailureClass,
        legacy_error: String,
    },
    Cancelled {
        flow_id: String,
        server_name: String,
    },
    Restored {
        flow_id: String,
        server_name: String,
    },
}

impl OAuthWireNotification {
    fn new(
        flow_id: String,
        server_name: String,
        status: OAuthWireStatus,
    ) -> Result<Self, OAuthWireError> {
        validate_identifier(&flow_id)?;
        validate_server_name(&server_name)?;
        Ok(Self {
            flow_id,
            server_name,
            status,
            authorization_url: None,
            failure_class: None,
        })
    }

    pub fn terminal(
        flow_id: String,
        server_name: String,
        status: OAuthWireStatus,
    ) -> Result<Self, OAuthWireError> {
        if matches!(
            status,
            OAuthWireStatus::AuthorizationNeeded | OAuthWireStatus::Failed
        ) {
            return Err(OAuthWireError::InvalidStatusShape);
        }
        Self::new(flow_id, server_name, status)
    }

    pub fn authorization_needed(
        flow_id: String,
        server_name: String,
        authorization_url: String,
    ) -> Result<Self, OAuthWireError> {
        let mut notification =
            Self::new(flow_id, server_name, OAuthWireStatus::AuthorizationNeeded)?;
        notification.authorization_url = Some(ValidatedAuthorizationUrl::parse(authorization_url)?);
        Ok(notification)
    }

    pub fn failed(
        flow_id: String,
        server_name: String,
        failure_class: OAuthFailureClass,
    ) -> Result<Self, OAuthWireError> {
        let mut notification = Self::new(flow_id, server_name, OAuthWireStatus::Failed)?;
        notification.failure_class = Some(failure_class);
        Ok(notification)
    }

    pub fn into_params(self) -> Value {
        let mut params = json!({
            "schemaVersion": OAUTH_SCHEMA_VERSION,
            "flowId": self.flow_id,
            "serverName": self.server_name,
            "status": self.status.as_str(),
        });
        if let Some(url) = self.authorization_url {
            params["authorizationUrl"] = Value::String(url.into_inner());
        }
        if let Some(class) = self.failure_class {
            params["failureClass"] = Value::String(class.as_str().to_string());
        }
        params
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OAuthWireError {
    #[error("invalid OAuth flow identifier")]
    InvalidFlowId,
    #[error("invalid MCP server name")]
    InvalidServerName,
    #[error("invalid OAuth authorization URL")]
    InvalidAuthorizationUrl,
    #[error("invalid OAuth notification status shape")]
    InvalidStatusShape,
}

pub fn validate_identifier(value: &str) -> Result<(), OAuthWireError> {
    if value.is_empty()
        || value.len() > OAUTH_IDENTIFIER_MAX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(OAuthWireError::InvalidFlowId);
    }
    Ok(())
}

pub fn validate_server_name(value: &str) -> Result<(), OAuthWireError> {
    if value.is_empty()
        || value.len() > OAUTH_IDENTIFIER_MAX_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(OAuthWireError::InvalidServerName);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authorization_url_policy_accepts_https_and_loopback_http() {
        for url in [
            "https://auth.example.test/authorize?state=safe",
            "http://localhost:43119/callback?state=safe",
            "http://127.0.0.1:43119/callback",
            "http://[::1]:43119/callback",
        ] {
            assert!(
                ValidatedAuthorizationUrl::parse(url.to_string()).is_ok(),
                "应允许安全授权 URL: {url}"
            );
        }
    }

    #[test]
    fn test_authorization_url_policy_rejects_unsafe_shapes() {
        for url in [
            "http://auth.example.test/authorize",
            "javascript:alert(1)",
            "data:text/plain,secret",
            "file:///tmp/token",
            "https://user:pass@auth.example.test/authorize",
            "https://auth.example.test/authorize#token=secret",
            "not a url",
        ] {
            assert_eq!(
                ValidatedAuthorizationUrl::parse(url.to_string()).err(),
                Some(OAuthWireError::InvalidAuthorizationUrl),
                "应拒绝不安全授权 URL: {url}"
            );
        }
    }

    #[test]
    fn test_notification_exposes_only_bounded_wire_fields() {
        let params = OAuthWireNotification::authorization_needed(
            "flow-1".into(),
            "docs".into(),
            "https://auth.example.test/authorize?state=opaque".into(),
        )
        .unwrap()
        .into_params();
        assert_eq!(params["schemaVersion"], 1);
        assert_eq!(params["flowId"], "flow-1");
        assert_eq!(params["serverName"], "docs");
        assert_eq!(params["status"], "authorization_needed");
        assert!(params.get("error").is_none());
        assert!(params.get("code").is_none());
        assert!(params.get("state").is_none());
    }

    #[test]
    fn test_notification_status_shape_cannot_omit_required_fields() {
        for status in [
            OAuthWireStatus::AuthorizationNeeded,
            OAuthWireStatus::Failed,
        ] {
            assert_eq!(
                OAuthWireNotification::terminal("flow-1".into(), "docs".into(), status).err(),
                Some(OAuthWireError::InvalidStatusShape)
            );
        }
    }
}
