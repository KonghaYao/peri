//! PostgreSQL connection URLs are secrets. Only the driver receives the full DSN.

use std::fmt;

use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteEngine {
    Postgres,
}

impl RemoteEngine {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "postgres" | "postgresql" => Some(Self::Postgres),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EndpointError {
    Empty,
    NotARemoteUrl,
    UnsupportedScheme,
    UnparsableUrl,
    UnsupportedConnectParameter,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "session store locator is empty",
            Self::NotARemoteUrl => "session store locator is not a remote URL",
            Self::UnsupportedScheme => {
                "remote session store requires a postgres:// or postgresql:// URL"
            }
            Self::UnsupportedConnectParameter => "unsupported PostgreSQL connection parameter",
            Self::UnparsableUrl => "invalid PostgreSQL session store URL",
        })
    }
}

impl std::error::Error for EndpointError {}

#[derive(Clone)]
pub(crate) struct RemoteEndpoint {
    url: Url,
}

impl RemoteEndpoint {
    pub(crate) fn parse(raw: &str) -> Result<Self, EndpointError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(EndpointError::Empty);
        }
        if !raw.contains("://") {
            return Err(EndpointError::NotARemoteUrl);
        }
        let url = Url::parse(raw).map_err(|_| EndpointError::UnparsableUrl)?;
        if RemoteEngine::parse(url.scheme()).is_none() {
            return Err(EndpointError::UnsupportedScheme);
        }
        if url.host().is_none() || url.fragment().is_some() {
            return Err(EndpointError::UnparsableUrl);
        }
        // SQLx logs both the name and the value of unknown connection options.
        // Fail before handing a possibly secret value to its URL parser.
        for (key, _) in url.query_pairs() {
            if !matches!(
                key.as_ref(),
                "sslmode"
                    | "ssl-mode"
                    | "sslrootcert"
                    | "ssl-root-cert"
                    | "ssl-ca"
                    | "sslcert"
                    | "ssl-cert"
                    | "sslkey"
                    | "ssl-key"
                    | "statement-cache-capacity"
                    | "host"
                    | "hostaddr"
                    | "port"
                    | "dbname"
                    | "user"
                    | "password"
                    | "application_name"
                    | "options"
            ) && !(key.starts_with("options[") && key.ends_with(']'))
            {
                return Err(EndpointError::UnsupportedConnectParameter);
            }
        }
        Ok(Self { url })
    }

    /// The caller must never include this value in diagnostics.
    pub(crate) fn sdk_url(&self) -> &str {
        self.url.as_str()
    }
}

impl fmt::Debug for RemoteEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteEndpoint(PostgreSQL)")
    }
}
