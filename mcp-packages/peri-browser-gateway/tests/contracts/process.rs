use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::Context;
use serde_json::json;

use crate::support::fake_engine::Engine;
use crate::support::gateway::{initialize, request, rpc, LEGACY};

struct RunningGateway(Child);

impl Drop for RunningGateway {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[tokio::test]
async fn trace_logs_keep_http_and_cdp_inputs_private_and_sigterm_reaps_listener(
) -> anyhow::Result<()> {
    const SECRET: &str = "fixture-test-only-monitor-secret";
    const HEADER: &str = "fixture-test-only-header";
    const PAYLOAD: &str = "fixture-test-only-payload";
    const CREDENTIAL: &str = "fixture-test-only-credential";
    let home = tempfile::tempdir()?;
    let mut engine = Engine::ephemeral().await?;
    let mut child = RunningGateway(
        Command::new(env!("CARGO_BIN_EXE_peri-browser"))
            .args(["--bind", "127.0.0.1:0", "--cdp-url", &engine.url()])
            .env_clear()
            .env("HOME", home.path())
            .env(
                "PERI_BROWSER_PROVISION_LOCK",
                home.path().join("provision.lock"),
            )
            .env("PERI_BROWSER_GATEWAY_SECRET", SECRET)
            .env("RUST_LOG", "trace")
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let (start, listening) = tokio::sync::oneshot::channel();
    let logs = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut reader = BufReader::new(stdout);
        let mut logs = String::new();
        while reader.read_line(&mut logs)? != 0 {
            if let Some((_, addr)) = logs.lines().last().unwrap_or_default().split_once("bind=") {
                let _ = start.send(addr.to_owned());
                break;
            }
        }
        reader.read_to_string(&mut logs)?;
        Ok(logs)
    });
    let stderr = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut stderr = stderr;
        let mut logs = String::new();
        stderr.read_to_string(&mut logs)?;
        Ok(logs)
    });
    let addr: std::net::SocketAddr = tokio::time::timeout(Duration::from_secs(5), listening)
        .await
        .context("等待子进程监听日志")??
        .parse()
        .context("解析实际监听地址")?;
    assert!(addr.ip().is_loopback());
    let base = format!("http://{addr}");
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let post = |body: serde_json::Value| {
        http.post(format!("{base}/mcp"))
            .header("accept", "application/json, text/event-stream")
            .header("authorization", HEADER)
            .json(&body)
    };
    let response = post(initialize(LEGACY)).send().await?.error_for_status()?;
    let id = response.headers()["mcp-session-id"].to_str()?.to_owned();
    assert_eq!(rpc(response).await?["result"]["protocolVersion"], LEGACY);
    let attached = |body| {
        post(body)
            .header("mcp-session-id", &id)
            .header("mcp-protocol-version", LEGACY)
    };
    let call = |name, arguments| {
        attached(request(
            "tools/call",
            json!({"name":name,"arguments":arguments}),
        ))
    };
    let bad_url = format!(
        "http://user:{CREDENTIAL}@{}/secret?arg={PAYLOAD}",
        engine.addr
    );
    let rejected = rpc(call("browser_connect", json!({"url":bad_url}))
        .send()
        .await?)
    .await?;
    assert_eq!(
        rejected["result"]["structuredContent"]["code"],
        "invalid_argument"
    );
    assert!(!rejected.to_string().contains(CREDENTIAL));
    let connected = rpc(call("browser_connect", json!({"url":engine.url()}))
        .send()
        .await?)
    .await?;
    assert_eq!(connected["result"]["structuredContent"]["connected"], true);
    let url = format!("https://fixture.test/fixture-committed-error?arg={PAYLOAD}");
    let navigated = rpc(call("navigate_page", json!({"url":url})).send().await?).await?;
    assert_eq!(navigated["result"]["structuredContent"]["url"], url);

    let index = http
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let (_, after) = index.split_once("href=\"/s/").expect("公开监控索引");
    let token = after
        .split('/')
        .next()
        .filter(|token| !token.is_empty())
        .unwrap();
    let path = format!("/s/{token}/");
    assert_eq!(
        http.get(format!("{base}{path}")).send().await?.status(),
        200
    );
    assert_eq!(
        http.get(format!("{base}{path}ws")).send().await?.status(),
        400
    );

    let mut sse = http
        .get(format!("{base}/mcp"))
        .header("accept", "text/event-stream")
        .header("mcp-session-id", &id)
        .header("mcp-protocol-version", LEGACY)
        .send()
        .await?
        .error_for_status()?;
    // 只向当前测试创建且仍被 Child 持有的子进程发送 SIGTERM。
    assert!(Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.0.id().to_string())
        .status()?
        .success());
    while sse.chunk().await?.is_some() {}
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok::<_, std::io::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(status.success());
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    let logs = logs.await?? + &stderr.await??;
    assert!(logs.contains("websocket upgrade required"));
    assert!(logs.contains("CDP 导航已提交"));
    assert!(logs.contains("关闭浏览器会话准入"));
    for needle in [SECRET, HEADER, PAYLOAD, CREDENTIAL, token, &id] {
        assert!(!logs.contains(needle), "日志包含不应记录的输入");
    }
    engine.shutdown().await
}
