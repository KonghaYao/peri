//! ACP Registry 认证握手与首次配置的进程级回归。
//!
//! 每个用例都以隔离的 `HOME` 启动真实 `peri acp`，经 stdin/stdout 发送
//! newline-delimited JSON-RPC，验证：
//!
//! - 干净环境（无 provider）的 `initialize` 成功并按客户端能力广告 terminal auth；
//! - 同一连接的 `session/new` 返回 `-32000` + `data.reason="auth_required"`；
//! - 未声明认证能力时 `authMethods` 为空；
//! - 已配置 provider 的宿主 `session/new` 成功，不要求先配置；
//! - stdin EOF 后进程干净退出 0。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

struct AcpProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_log: PathBuf,
    next_id: i64,
}

impl AcpProcess {
    fn spawn(home: &Path) -> Self {
        let stderr_log = home.join("acp.stderr.log");
        let stderr = std::fs::File::create(&stderr_log)
            .expect("acp process: the isolated stderr log must be writable");
        let mut child = Command::new(env!("CARGO_BIN_EXE_peri"))
            .env_clear()
            .env("HOME", home)
            .env("TMPDIR", home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(home)
            .arg("acp")
            .arg("--cwd")
            .arg(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .expect("peri acp must start with an isolated HOME");
        let stdin = child.stdin.take().expect("peri acp stdin is piped");
        let stdout = child.stdout.take().expect("peri acp stdout is piped");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_log,
            next_id: 1,
        }
    }

    fn stderr(&self) -> String {
        read_log(&self.stderr_log)
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let stderr_log = self.stderr_log.clone();
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("request must be written to peri acp stdin");
        self.stdin.flush().await.expect("request must be flushed");

        let next = match tokio::time::timeout(STARTUP_TIMEOUT, self.lines.next_line()).await {
            Ok(Ok(Some(next))) => next,
            Ok(Ok(None)) => panic!(
                "peri acp closed stdout before answering {method}; stderr:\n{}",
                read_log(&stderr_log)
            ),
            Ok(Err(error)) => panic!("peri acp stdout read failed for {method}: {error}"),
            Err(_) => panic!("{method} timed out; stderr:\n{}", read_log(&stderr_log)),
        };
        let value: Value =
            serde_json::from_str(&next).expect("peri acp stdout must be a JSON-RPC line");
        assert_eq!(value["id"], id, "RequestId must round-trip: {value}");
        value
    }

    async fn initialize(&mut self, extra: Value) -> Value {
        let mut params = json!({ "protocolVersion": 1 });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            params.extend(extra.clone());
        }
        self.request("initialize", params).await
    }

    async fn exit(mut self) {
        let stderr_log = self.stderr_log.clone();
        drop(self.stdin);
        let waited = tokio::time::timeout(Duration::from_secs(30), self.child.wait()).await;
        let status = match waited {
            Ok(status) => status.expect("peri acp must be waitable"),
            Err(_) => panic!(
                "peri acp did not exit after EOF; stderr:\n{}",
                read_log(&stderr_log)
            ),
        };
        assert!(
            status.success(),
            "peri acp must exit 0 after EOF; status={status}; stderr:\n{}",
            read_log(&stderr_log)
        );
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn setup_with_settings_stdin_is_a_clap_argument_error() {
    let home = isolated_home();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_peri"))
        .env_clear()
        .env("HOME", home.path())
        .env("TMPDIR", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(home.path())
        .args(["acp", "--settings-stdin", "--setup"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("peri acp --settings-stdin --setup must run");
    assert_eq!(
        output.status.code(),
        Some(2),
        "argument conflict must exit 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--setup cannot be combined with --settings-stdin"),
        "stderr must explain the conflict"
    );
    assert!(
        output.stdout.is_empty(),
        "argument validation must not write JSON-RPC frames"
    );
}

fn isolated_home() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("isolated HOME must be creatable")
}

fn write_configured_settings(home: &Path) {
    std::fs::create_dir_all(home.join(".peri")).expect("~/.peri must be creatable");
    std::fs::write(
        home.join(".peri/settings.json"),
        json!({
            "config": {
                "active_alias": "sonnet",
                "providers": [{
                    "id": "fixture",
                    "type": "anthropic",
                    "apiKey": "test-only",
                    "baseUrl": "http://127.0.0.1:9",
                    "models": {"sonnet": "fixture-model"}
                }]
            }
        })
        .to_string(),
    )
    .expect("isolated settings must be writable");
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_home_advertises_terminal_auth_and_rejects_session_new() {
    let home = isolated_home();
    let mut process = AcpProcess::spawn(home.path());

    let initialize = process
        .initialize(json!({
            "clientCapabilities": { "auth": { "terminal": true } }
        }))
        .await;
    assert!(
        initialize.get("error").is_none(),
        "initialize: {initialize}"
    );
    assert_eq!(initialize["result"]["protocolVersion"], 1, "{initialize}");
    let methods = initialize["result"]["authMethods"]
        .as_array()
        .unwrap_or_else(|| panic!("authMethods must be an array: {initialize}"));
    assert_eq!(
        methods.len(),
        1,
        "exactly one terminal auth method: {initialize}"
    );
    assert_eq!(methods[0]["type"], "terminal", "{initialize}");
    assert_eq!(methods[0]["id"], "peri-setup", "{initialize}");
    assert_eq!(
        methods[0]["args"],
        json!(["--setup"]),
        "setup args: {initialize}"
    );
    assert_eq!(
        methods[0]["env"]["PERI_ACP_SETUP"], "1",
        "setup env: {initialize}"
    );

    let session = process
        .request(
            "session/new",
            json!({ "cwd": home.path(), "mcpServers": [] }),
        )
        .await;
    assert_eq!(session["error"]["code"], -32000, "session/new: {session}");
    assert!(
        session["error"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("AUTH_REQUIRED: ")),
        "session/new message: {session}"
    );
    assert_eq!(
        session["error"]["data"]["reason"], "auth_required",
        "session/new data: {session}"
    );
    assert_eq!(
        session["error"]["data"]["setupCommand"], "peri --setup",
        "session/new setup command: {session}"
    );

    process.exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_home_without_auth_marker_keeps_auth_methods_empty() {
    let home = isolated_home();
    let mut process = AcpProcess::spawn(home.path());

    let initialize = process.initialize(json!({})).await;
    assert_eq!(
        initialize["result"]["authMethods"],
        json!([]),
        "unmarked client must not get auth methods: {initialize}"
    );

    process.exit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_home_creates_a_session_without_setup() {
    let home = isolated_home();

    // 未配置的首次连接只能拿到 AUTH_REQUIRED；配置写入后重连才进入正常宿主。
    {
        let mut first = AcpProcess::spawn(home.path());
        first.initialize(json!({})).await;
        let rejected = first
            .request(
                "session/new",
                json!({ "cwd": home.path(), "mcpServers": [] }),
            )
            .await;
        assert_eq!(
            rejected["error"]["data"]["reason"], "auth_required",
            "first connection must require setup: {rejected}"
        );
        first.exit().await;
    }

    write_configured_settings(home.path());
    let mut process = AcpProcess::spawn(home.path());

    let initialize = process.initialize(json!({})).await;
    assert!(
        initialize.get("error").is_none(),
        "initialize: {initialize}"
    );
    assert_eq!(
        initialize["result"]["authMethods"],
        json!([]),
        "configured host must not advertise auth: {initialize}"
    );

    let session = process
        .request(
            "session/new",
            json!({ "cwd": home.path(), "mcpServers": [] }),
        )
        .await;
    assert!(
        session.get("error").is_none(),
        "configured session/new must succeed after reconnect: {session}; stderr:\n{}",
        process.stderr()
    );
    assert!(
        session["result"]["sessionId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "session/new must return a sessionId: {session}"
    );

    process.exit().await;
}
