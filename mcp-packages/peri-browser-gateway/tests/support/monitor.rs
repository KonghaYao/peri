use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{protocol::CloseFrame, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::fake_engine::FRAME;
use super::gateway::Gateway;

const WAIT: Duration = Duration::from_secs(5);

pub struct Monitor {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    latest_state: Option<Value>,
}

impl Monitor {
    pub async fn open(gateway: &Gateway, domain: &str) -> anyhow::Result<Self> {
        let path = gateway.monitor_path(domain).await?;
        let mut request = format!("{}{}ws", gateway.base.replacen("http://", "ws://", 1), path)
            .into_client_request()?;
        request
            .headers_mut()
            .insert("origin", gateway.base.parse()?);
        let (socket, _) =
            tokio::time::timeout(WAIT, tokio_tungstenite::connect_async(request)).await??;
        let mut monitor = Self {
            socket,
            latest_state: None,
        };
        let hello = monitor.until(|value| value["t"] == "hello").await?;
        assert_eq!(hello["session"], domain);
        Ok(monitor)
    }

    pub async fn send(&mut self, id: u64, state: &Value, mut action: Value) -> anyhow::Result<()> {
        action["id"] = json!(id);
        action["view"] = state["view"].clone();
        tokio::time::timeout(
            WAIT,
            self.socket.send(Message::Text(action.to_string().into())),
        )
        .await??;
        Ok(())
    }

    pub async fn command(
        &mut self,
        id: u64,
        state: &Value,
        action: Value,
    ) -> anyhow::Result<Value> {
        self.send(id, state, action).await?;
        self.result(id).await
    }

    pub async fn result(&mut self, id: u64) -> anyhow::Result<Value> {
        self.until(|value| value["t"] == "result" && value["id"] == id)
            .await
    }

    pub async fn state(&mut self, ready: impl Fn(&Value) -> bool) -> anyhow::Result<Value> {
        if let Some(state) = self.latest_state.as_ref().filter(|state| ready(state)) {
            return Ok(state.clone());
        }
        self.until(|value| value["t"] == "state" && ready(value))
            .await
    }

    pub async fn frame(&mut self, page: &str, epoch: &Value) -> anyhow::Result<Value> {
        let meta = self
            .until(|value| {
                value["t"] == "meta" && value["pageId"] == page && value["viewEpoch"] == *epoch
            })
            .await?;
        let Some(Message::Binary(jpeg)) = tokio::time::timeout(WAIT, self.socket.next())
            .await?
            .transpose()?
        else {
            anyhow::bail!("Meta 的下一条消息不是 JPEG");
        };
        assert_eq!(jpeg.as_ref(), FRAME);
        Ok(meta)
    }

    pub async fn close_frame(&mut self) -> anyhow::Result<CloseFrame> {
        tokio::time::timeout(WAIT, async {
            while let Some(frame) = self.socket.next().await {
                if let Message::Close(Some(frame)) = frame? {
                    return Ok(frame);
                }
            }
            anyhow::bail!("监控连接未发送 Close");
        })
        .await?
    }

    async fn until(&mut self, ready: impl Fn(&Value) -> bool) -> anyhow::Result<Value> {
        tokio::time::timeout(WAIT, async {
            while let Some(frame) = self.socket.next().await {
                match frame? {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(text.as_str())?;
                        if value["t"] == "state" {
                            self.latest_state = Some(value.clone());
                        }
                        if ready(&value) {
                            return Ok(value);
                        }
                    }
                    Message::Close(_) => anyhow::bail!("等待监控消息时连接关闭"),
                    _ => {}
                }
            }
            anyhow::bail!("等待监控消息时遇到 EOF");
        })
        .await?
    }
}
