use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use peri_browser_gateway::apps::{AppDocument, AppDocuments};
use peri_browser_gateway::config::Config;
use peri_browser_gateway::view::ViewSessions;
use reqwest::{Client, RequestBuilder, Response};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub const LEGACY: &str = "2025-11-25";
const ACCEPT: &str = "application/json, text/event-stream";

pub struct Gateway {
    pub base: String,
    pub http: Client,
    pub stop: CancellationToken,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: AtomicU64,
    _temp: tempfile::TempDir,
}

impl Gateway {
    pub async fn start(suggested: &str, env: &[(&str, &str)]) -> anyhow::Result<Self> {
        Self::serve(TcpListener::bind("127.0.0.1:0").await?, suggested, env).await
    }

    pub async fn serve(
        listener: TcpListener,
        suggested: &str,
        env: &[(&str, &str)],
    ) -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let config = fixture_config(temp.path(), suggested, env)?;
        let base = format!("http://{}", listener.local_addr()?);
        let stop = CancellationToken::new();
        let views = ViewSessions::new(
            &config,
            std::sync::Arc::new(AppDocuments::new(AppDocument::placeholder())),
        );
        let server = tokio::spawn(peri_browser_gateway::serve(
            listener,
            config,
            views,
            stop.clone().cancelled_owned(),
        ));
        Ok(Self {
            base,
            stop,
            server,
            next_id: AtomicU64::new(2),
            _temp: temp,
            http: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()?,
        })
    }

    pub fn install_root(&self) -> PathBuf {
        self._temp.path().join(".kitesurf")
    }

    pub fn mcp(&self) -> String {
        format!("{}/mcp", self.base)
    }

    pub fn post(&self, id: Option<&str>, value: Value) -> RequestBuilder {
        let request = self
            .http
            .post(self.mcp())
            .header("accept", ACCEPT)
            .json(&value);
        match id {
            Some(id) => request
                .header("mcp-session-id", id)
                .header("mcp-protocol-version", LEGACY),
            None => request,
        }
    }

    pub fn events(&self, id: &str) -> RequestBuilder {
        self.http
            .get(self.mcp())
            .header("accept", "text/event-stream")
            .header("mcp-session-id", id)
            .header("mcp-protocol-version", LEGACY)
    }

    pub async fn init(&self) -> anyhow::Result<String> {
        let response = self
            .post(None, initialize(LEGACY))
            .send()
            .await?
            .error_for_status()?;
        let id = response
            .headers()
            .get("mcp-session-id")
            .expect("会话 header")
            .to_str()?
            .to_owned();
        let result = rpc(response)
            .await
            .map_err(|e| e.context("读取 initialize 回执"))?;
        assert_eq!(result["result"]["serverInfo"]["name"], "peri-browser-mcp");
        assert_eq!(result["result"]["protocolVersion"], LEGACY);
        let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(
            self.post(Some(&id), notification).send().await?.status(),
            202
        );
        Ok(id)
    }

    pub fn tool_request(&self, name: &str, arguments: Value) -> (u64, Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = request("tools/call", json!({"name": name, "arguments": arguments}));
        message["id"] = json!(id);
        (id, message)
    }

    pub async fn call(&self, id: &str, name: &str, arguments: Value) -> anyhow::Result<Value> {
        let (request_id, message) = self.tool_request(name, arguments);
        let response = self.post(Some(id), message).send().await?;
        let reply = rpc(response.error_for_status()?).await?;
        assert_eq!(reply["id"], request_id);
        anyhow::ensure!(
            reply.get("result").is_some(),
            "fixture 预期工具结果而非协议错误"
        );
        Ok(reply["result"].clone())
    }

    pub async fn success(&self, id: &str, name: &str, arguments: Value) -> anyhow::Result<Value> {
        let result = self.call(id, name, arguments).await?;
        assert_ne!(result["isError"], true, "{name}: {result}");
        Ok(result["structuredContent"].clone())
    }

    pub async fn error_code(
        &self,
        id: &str,
        name: &str,
        arguments: Value,
        code: &str,
    ) -> anyhow::Result<()> {
        let result = self.call(id, name, arguments).await?;
        assert_eq!(result["isError"], true, "{name}: {result}");
        assert_eq!(result["structuredContent"]["code"], code);
        Ok(())
    }

    pub async fn connect(&self, id: &str, url: &str) -> anyhow::Result<()> {
        let result = self
            .success(id, "browser_connect", json!({"url": url}))
            .await?;
        assert_eq!(result["connected"], true);
        Ok(())
    }

    pub async fn new_page(&self, id: &str) -> anyhow::Result<String> {
        let result = self.success(id, "new_page", json!({})).await?;
        let page = result["pageId"].as_str().expect("公开 pageId");
        assert!(page.starts_with("p_"));
        Ok(page.to_owned())
    }

    pub async fn domain(&self, id: &str) -> anyhow::Result<String> {
        Ok(
            self.success(id, "browser_status", json!({})).await?["sessionId"]
                .as_str()
                .expect("公开会话标识")
                .to_owned(),
        )
    }

    pub fn delete(&self, id: &str) -> RequestBuilder {
        self.http
            .delete(self.mcp())
            .header("mcp-session-id", id)
            .header("mcp-protocol-version", LEGACY)
    }

    pub async fn count(&self) -> anyhow::Result<u64> {
        let health: Value = self
            .http
            .get(format!("{}/healthz", self.base))
            .send()
            .await?
            .json()
            .await?;
        Ok(health["sessions"].as_u64().expect("会话计数"))
    }

    pub async fn monitor_path(&self, domain: &str) -> anyhow::Result<String> {
        let index = self.http.get(&self.base).send().await?.text().await?;
        for (path, item) in index
            .split("<li><a href=\"")
            .skip(1)
            .filter_map(|item| item.split_once("\">"))
        {
            if item
                .split("</li>")
                .next()
                .unwrap_or_default()
                .contains(domain)
            {
                return Ok(path.to_owned());
            }
        }
        anyhow::bail!("索引缺少当前会话")
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(18), &mut self.server).await??
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub fn fixture_config(
    root: &Path,
    suggested: &str,
    env: &[(&str, &str)],
) -> anyhow::Result<Config> {
    let lock = root.join("provision.lock");
    Ok(Config::parse_from(
        [
            "--bind".to_owned(),
            "127.0.0.1:0".to_owned(),
            "--cdp-url".to_owned(),
            suggested.to_owned(),
        ],
        |key| {
            env.iter()
                .find(|(name, _)| key == *name)
                .map(|(_, value)| (*value).to_owned())
                .or_else(|| match key {
                    "HOME" => root.to_str().map(str::to_owned),
                    "PERI_BROWSER_PROVISION_LOCK" => lock.to_str().map(str::to_owned),
                    "PERI_BROWSER_CONNECT_TIMEOUT_MS" => Some("400".to_owned()),
                    "PERI_BROWSER_RECLAIM_WAIT_MS" => Some("50".to_owned()),
                    _ => None,
                })
        },
    )?)
}

pub fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

pub fn initialize(version: &str) -> Value {
    request(
        "initialize",
        json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}),
    )
}

pub async fn rpc(response: Response) -> anyhow::Result<Value> {
    let text = response.text().await?;
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find(|data| !data.trim().is_empty());
    Ok(serde_json::from_str(payload.unwrap_or(&text))?)
}
