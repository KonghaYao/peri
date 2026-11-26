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
    #[cfg(not(target_os = "emscripten"))]
    UnsupportedConnectParameter,
    #[cfg(target_os = "emscripten")]
    InvalidHttpEndpoint,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "session store locator is empty",
            Self::NotARemoteUrl => "session store locator is not a remote URL",
            Self::UnsupportedScheme => {
                "remote session store requires a postgres:// or postgresql:// URL"
            }
            #[cfg(not(target_os = "emscripten"))]
            Self::UnsupportedConnectParameter => "unsupported PostgreSQL connection parameter",
            Self::UnparsableUrl => "invalid PostgreSQL session store URL",
            #[cfg(target_os = "emscripten")]
            Self::InvalidHttpEndpoint => "PostgreSQL HTTP endpoint requires HTTPS or loopback HTTP",
        })
    }
}

impl std::error::Error for EndpointError {}

#[derive(Clone)]
pub(crate) struct RemoteEndpoint {
    url: Url,
    #[cfg(target_os = "emscripten")]
    http_endpoint: Option<Url>,
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
        #[cfg(not(target_os = "emscripten"))]
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
        Ok(Self {
            url,
            #[cfg(target_os = "emscripten")]
            http_endpoint: None,
        })
    }

    /// The caller must never include this value in diagnostics.
    pub(crate) fn sdk_url(&self) -> &str {
        self.url.as_str()
    }

    #[cfg(target_os = "emscripten")]
    pub(crate) fn with_http_endpoint(
        mut self,
        value: Option<String>,
    ) -> Result<Self, EndpointError> {
        if let Some(value) = value {
            let url = Url::parse(&value).map_err(|_| EndpointError::InvalidHttpEndpoint)?;
            let loopback = match url.host() {
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                Some(url::Host::Domain(name)) => name == "localhost",
                None => false,
            };
            if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
                || !url.has_host()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || url.query().is_some()
            {
                return Err(EndpointError::InvalidHttpEndpoint);
            }
            self.http_endpoint = Some(url);
        }
        Ok(self)
    }

    #[cfg(target_os = "emscripten")]
    pub(crate) fn http_endpoint(&self) -> Option<&str> {
        self.http_endpoint.as_ref().map(Url::as_str)
    }
}

impl fmt::Debug for RemoteEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteEndpoint(PostgreSQL)")
    }
}
