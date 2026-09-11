//! 请求绑定一次连接，pending guard 负责正常、超时及调用方取消时的释放。
use super::*;
use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest};

impl LspClient {
    pub(super) fn ready_connection(&self) -> Result<Arc<RegisteredConnection>, LspError> {
        let connection = self.connection.read();
        if connection.state == ServerState::Running {
            if let Some(registered) = &connection.registered {
                return Ok(registered.clone());
            }
        }
        Err(LspError::NotReady {
            server: self.name.clone(),
        })
    }

    pub(super) fn ready_dispatcher(&self) -> Result<Arc<MessageDispatcher>, LspError> {
        Ok(self.ready_connection()?.dispatcher.clone())
    }

    /// 发送请求并等待响应；超时覆盖排队、写入和响应等待。
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_ms: u64,
    ) -> Result<Value, LspError> {
        let dispatcher = self.ready_dispatcher()?;
        self.request_on(&dispatcher, method, params, timeout_ms)
            .await
    }

    pub(super) async fn request_on(
        &self,
        dispatcher: &MessageDispatcher,
        method: &str,
        params: Option<Value>,
        timeout_ms: u64,
    ) -> Result<Value, LspError> {
        let id = {
            let mut id = self.next_id.lock();
            *id += 1;
            *id
        };
        let request = JsonRpcRequest::new(id, method, params);
        let (_registration, receiver) = dispatcher.register_owned_request(id);
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
            dispatcher.send_request(&request).await?;
            receiver.await.map_err(|_| LspError::RequestFailed {
                method: method.to_string(),
                reason: "请求被取消".into(),
            })?
        })
        .await
        .map_err(|_| LspError::RequestTimeout {
            method: method.into(),
            timeout_ms,
        })?
    }

    /// 仅向已完成握手的当前连接发送通知。
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), LspError> {
        self.ready_dispatcher()?
            .send_notification(&JsonRpcNotification::new(method, params))
            .await
    }
}
