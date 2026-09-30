use std::sync::Arc;

use peri_acp_types::oauth_credentials::{
    validate_credentials, validate_server_key, OAuthCredentialError, OAuthCredentialPort,
    OAuthCredentialRequest, OAuthCredentialResponse, OAuthCredentialValue, OAUTH_CREDENTIAL_METHOD,
};
use rmcp::{
    model::{CustomRequest, CustomResult, Implementation, ServerCapabilities, ServerInfo},
    service::{RequestContext, RoleServer},
    ErrorData, ServerHandler,
};

#[derive(Clone)]
pub struct OAuthCredentialMcpServer {
    store: Arc<dyn OAuthCredentialPort>,
}

impl OAuthCredentialMcpServer {
    pub fn new(store: Arc<dyn OAuthCredentialPort>) -> Self {
        Self { store }
    }

    async fn execute(&self, request: OAuthCredentialRequest) -> OAuthCredentialResponse {
        match request {
            OAuthCredentialRequest::Load { server_key } => {
                validate_server_key(&server_key)?;
                self.store
                    .load(&server_key)
                    .await
                    .map(OAuthCredentialValue::Credentials)
            }
            OAuthCredentialRequest::Save {
                server_key,
                credentials,
            } => {
                validate_server_key(&server_key)?;
                validate_credentials(&credentials)?;
                self.store.save(&server_key, &credentials).await?;
                Ok(OAuthCredentialValue::Saved)
            }
            OAuthCredentialRequest::Clear { server_key } => {
                validate_server_key(&server_key)?;
                self.store.clear(&server_key).await?;
                Ok(OAuthCredentialValue::Saved)
            }
            OAuthCredentialRequest::ClearAll => {
                self.store.clear_all().await?;
                Ok(OAuthCredentialValue::Saved)
            }
            OAuthCredentialRequest::List => self.store.list().await.map(OAuthCredentialValue::Keys),
        }
    }
}

impl ServerHandler for OAuthCredentialMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::default()).with_server_info(Implementation::new(
            "peri-credentials",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        if request.method != OAUTH_CREDENTIAL_METHOD {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                "Unknown credential method",
                None,
            ));
        }
        let request = request
            .params
            .and_then(|params| serde_json::from_value(params).ok())
            .ok_or_else(|| ErrorData::invalid_params("Invalid OAuth credential request", None))?;
        let result = self.execute(request).await;
        let value = serde_json::to_value(result)
            .map_err(|_| ErrorData::internal_error("OAuth credential response failed", None))?;
        Ok(CustomResult(value))
    }
}

pub(crate) fn storage_error(error: OAuthCredentialError) -> std::io::Error {
    let kind = match error {
        OAuthCredentialError::InvalidInput => std::io::ErrorKind::InvalidInput,
        OAuthCredentialError::InvalidData => std::io::ErrorKind::InvalidData,
        OAuthCredentialError::ReadOnly => std::io::ErrorKind::PermissionDenied,
        OAuthCredentialError::Unavailable => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, error)
}
