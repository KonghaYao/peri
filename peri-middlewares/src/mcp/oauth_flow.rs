use std::{collections::HashMap, sync::Arc};

use rmcp::transport::auth::{AuthError, OAuthState};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{info, warn};

use super::{
    auth_store::{FileCredentialStore, PerServerCredentialStore},
    callback_server::{CallbackError, OAuthCallbackServer},
    config::OAuthConfig,
};

/// OAuth 回调结果（从 TUI 传回后台 OAuth 流程）
pub struct OAuthCallbackResult {
    /// 授权码
    pub code: String,
    /// CSRF state 参数
    pub state: String,
}

/// OAuth 流程编排错误
#[derive(Debug, Error)]
pub enum OAuthFlowError {
    #[error("OAuth 回调服务器错误: {0}")]
    CallbackError(#[from] CallbackError),
    #[error("OAuth 授权错误: {0}")]
    AuthError(#[from] AuthError),
    #[error("OAuth 授权被用户取消")]
    Cancelled,
    #[error("OAuth 回调等待超时")]
    CallbackTimeout,
}

/// OAuth 流程事件（由后台产生，需转发到 TUI 层）
pub enum OAuthFlowEvent {
    /// 需要用户浏览器授权
    AuthorizationNeeded {
        server_name: String,
        authorization_url: String,
        /// 回调通道：TUI 收集用户输入后通过此通道传回授权码
        callback_tx: oneshot::Sender<OAuthCallbackResult>,
    },
    /// OAuth 授权完成
    AuthorizationCompleted { server_name: String },
    /// OAuth 授权失败
    AuthorizationFailed { server_name: String, error: String },
    /// 从凭证存储恢复成功（快速路径：磁盘已有有效凭证，跳过浏览器授权）。
    ///
    /// 恢复 ≠ 用户本次完成授权——连接阶段仍会验证 token 有效性，失效时由
    /// 调用方清除凭证并重新走完整授权。TUI 收到此事件用于反馈「已使用已
    /// 保存凭证连接」并同步面板池状态。
    AuthorizationRestored { server_name: String },
}

/// OAuth 流程编排器
///
/// 为每个需要 OAuth 的 MCP 服务器管理独立的 OAuthState 状态机。
/// 通过回调函数将事件转发给调用方（client.rs），由调用方决定如何通知 TUI。
pub struct OAuthFlowManager {
    /// 共享的 Token 文件存储
    token_store: Arc<FileCredentialStore>,
    /// 按 server_name 管理的 OAuth 状态机
    states: HashMap<String, OAuthState>,
    /// 事件回调（由 client.rs 在创建时注入；Arc 存储便于跨任务共享）
    event_callback: Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>,
}

impl OAuthFlowManager {
    /// 创建 OAuth 流程管理器
    ///
    /// `token_store`: 共享的 Token 文件存储实例
    /// `event_callback`: 事件回调函数，用于将 OAuth 事件转发给 TUI
    pub fn new<F>(token_store: Arc<FileCredentialStore>, event_callback: F) -> Self
    where
        F: Fn(OAuthFlowEvent) + Send + Sync + 'static,
    {
        Self {
            token_store,
            states: HashMap::new(),
            event_callback: Arc::new(event_callback),
        }
    }

    /// 创建 OAuth 流程管理器（Arc 回调版本：跨任务共享的 `Arc<dyn Fn>` 回调）。
    pub fn new_with_arc(
        token_store: Arc<FileCredentialStore>,
        event_callback: Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>,
    ) -> Self {
        Self {
            token_store,
            states: HashMap::new(),
            event_callback,
        }
    }

    /// 对指定服务器执行完整 OAuth 授权流程
    pub async fn run_oauth_flow(
        &mut self,
        server_name: &str,
        server_url: &str,
        oauth_config: &OAuthConfig,
    ) -> Result<(), OAuthFlowError> {
        info!(server = %server_name, "开始 OAuth 授权流程");

        // 1. 创建或复用 OAuthState
        let state = if let Some(existing) = self.states.remove(server_name) {
            existing
        } else {
            let credential_store =
                PerServerCredentialStore::new(self.token_store.clone(), server_name.to_string());
            let mut mgr_state = OAuthState::new(server_url, None).await?;
            if let OAuthState::Unauthorized(ref mut manager) = mgr_state {
                manager.set_credential_store(credential_store);
            }
            mgr_state
        };

        let mut state = state;

        // 2. 尝试从存储恢复已有凭证（快速路径）
        if let OAuthState::Unauthorized(manager) = &mut state {
            let has_creds = manager.initialize_from_store().await?;
            if has_creds {
                info!(server = %server_name, "从存储恢复已有凭证，跳过浏览器授权");
                self.states.insert(server_name.to_string(), state);
                // 注意：不 emit AuthorizationCompleted——恢复凭证 ≠ 用户完成
                // 授权；token 可能已过期/被 revoke，有效性由连接阶段验证，
                // 失效时调用方清除凭证并重新走完整授权（弹 popup）。
                // emit AuthorizationRestored：通知 TUI 走的是快速路径（凭据
                // 已存在），供其反馈「已使用已保存凭证连接」并同步面板池。
                (self.event_callback)(OAuthFlowEvent::AuthorizationRestored {
                    server_name: server_name.to_string(),
                });
                return Ok(());
            }
        }
        if let OAuthState::Authorized(_) = &state {
            info!(server = %server_name, "已处于授权状态，跳过浏览器授权");
            self.states.insert(server_name.to_string(), state);
            return Ok(());
        }

        // 3. 绑定回调服务器
        let (callback_server, redirect_uri) = OAuthCallbackServer::bind().await?;

        // 4. 启动授权（DCR + PKCE + metadata 发现）
        // rmcp 3.x: start_authorization 参数收敛为 AuthorizationRequest 结构
        let scopes: Vec<String> = oauth_config.scopes.clone().unwrap_or_default();

        state
            .start_authorization(
                rmcp::transport::auth::AuthorizationRequest::new(redirect_uri)
                    .with_scopes(scopes)
                    .with_client_name("peri-mcp-client"),
            )
            .await?;

        // 5. 获取授权 URL
        let authorization_url = state.get_authorization_url().await?;

        // 6. 创建 oneshot 通道，通知 TUI 等待用户交互
        let (callback_tx, callback_rx) = oneshot::channel::<OAuthCallbackResult>();

        self.emit_event(OAuthFlowEvent::AuthorizationNeeded {
            server_name: server_name.to_string(),
            authorization_url: authorization_url.clone(),
            callback_tx,
        });

        // 7. 并发等待回调（本地服务器 + TUI 手动粘贴），取先到达的
        let callback_result = tokio::select! {
            result = callback_server.wait_for_code() => {
                match result {
                    Ok((code, state)) => Ok(OAuthCallbackResult { code, state }),
                    Err(CallbackError::Timeout) => Err(OAuthFlowError::CallbackTimeout),
                    Err(e) => Err(OAuthFlowError::CallbackError(e)),
                }
            }
            result = callback_rx => {
                match result {
                    Ok(mut result) => {
                        // 手动粘贴路径：TUI 无法预知 PKCE state（rmcp 用它作
                        // state_store 索引），从授权 URL 解析兜底，避免
                        // "Authorization state not found"。
                        if result.state.is_empty() {
                            result.state = extract_state_from_url(&authorization_url);
                        }
                        Ok(result)
                    }
                    Err(_) => Err(OAuthFlowError::Cancelled),
                }
            }
        };

        let callback_data = match callback_result {
            Ok(data) => data,
            Err(e) => {
                self.emit_event(OAuthFlowEvent::AuthorizationFailed {
                    server_name: server_name.to_string(),
                    error: e.to_string(),
                });
                return Err(e);
            }
        };

        // 8. 处理回调，完成授权
        state
            .handle_callback(&callback_data.code, &callback_data.state)
            .await?;

        // 9. 保存状态到 states map
        self.states.insert(server_name.to_string(), state);

        // 10. 通知 TUI 授权完成
        self.emit_event(OAuthFlowEvent::AuthorizationCompleted {
            server_name: server_name.to_string(),
        });

        info!(server = %server_name, "OAuth 授权流程完成");
        Ok(())
    }

    /// 获取指定服务器的 AuthorizationManager（用于构建 AuthClient 传输层）
    ///
    /// 同时接受 Authorized（刚完成授权）和 Unauthorized（从存储恢复凭证）两种状态，
    /// 因为 Unauthorized 的 manager 可能已通过 `initialize_from_store` 加载了有效凭证。
    pub fn get_authorization_manager(
        &mut self,
        server_name: &str,
    ) -> Option<rmcp::transport::auth::AuthorizationManager> {
        let state = self.states.remove(server_name)?;
        match state {
            OAuthState::Authorized(manager) | OAuthState::Unauthorized(manager) => Some(manager),
            _ => {
                warn!(
                    server = %server_name,
                    "OAuth 状态不是 Authorized/Unauthorized，无法提取 AuthorizationManager"
                );
                None
            }
        }
    }

    /// 判断指定服务器是否已完成 OAuth 授权
    pub fn is_authorized(&self, server_name: &str) -> bool {
        matches!(
            self.states.get(server_name),
            Some(OAuthState::Authorized(_)) | Some(OAuthState::AuthorizedHttpClient(_))
        )
    }

    /// 获取共享的 Token 存储引用
    pub fn token_store(&self) -> &Arc<FileCredentialStore> {
        &self.token_store
    }

    /// 发送事件给调用方
    fn emit_event(&self, event: OAuthFlowEvent) {
        (self.event_callback)(event);
    }
}

/// 从授权 URL 中提取 `state` 查询参数（RFC 6749 §4.1.1）。
///
/// 手动粘贴授权码路径下 TUI 无法预知 PKCE state（rmcp 用 state 作
/// state_store 索引），授权 URL 由 rmcp 生成时必带 `state=`，此处解析兜底。
fn extract_state_from_url(url: &str) -> String {
    let Some((_, query)) = url.split_once('?') else {
        return String::new();
    };
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == "state" {
                return value.to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
#[path = "oauth_flow_test.rs"]
mod tests;
