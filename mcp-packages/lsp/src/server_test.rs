//! LSP MCP handler 的 crate 内行为证据。
//!
//! 覆盖口径：
//! - **门控谓词**：工具面可见性 == 生效 LSP 配置是否非空（`LspServerPool::has_servers()`），
//!   **与 language server 进程是否 ready 无关**：配置存在但一个进程都没起来时，
//!   `tools/list` 仍恰好一条 `LSP`（并以 spawn 计数文件证明「确实没起来」）；空配置时
//!   `list_tools` 成功且为空表，同时 `get_info()` 正常——**不得**用空表反推实例未就绪。
//! - **复用既有工具**：schema / description / 十个 operation 与 `LspTool::definition()`
//!   逐字相等（期望值由既有 `definition()` 生成，本文件不硬编码第二份 schema）；成功路径
//!   的文本与直接调用同一 `LspTool` 的文本逐字相等（handler 不另写 formatter）。
//! - **IF-D14 映射**：`tools/call` 的三种形态共用 `peri_mcp_common` 实现——
//!   未知工具名 → `Err(invalid_params)`；`Ok(text)` → success；`Err(_)` → **固定脱敏**
//!   error 文本（`LspToolError` 原文 / 文件路径 / 扩展名都不入模型面文本）。
//! - **快照**：工具面在构造时取一次；构造后 `pool.add_server(...)` **不改变** handler 的
//!   工具面（反向亦然：空 pool 构造不被倒灌）——配置热更新是显式非目标。
//! - **真实线路**：crate 用 rmcp 原生 Auto lifecycle client 和生产 handler 通过 duplex
//!   wire 通信，覆盖 `tools/list` 与 `tools/call`。宿主 runtime supervisor、取消收敛与实例
//!   生命周期由 `peri-middlewares` 的 host integration tests 覆盖。
//!
//! perl fake LSP server 与 tool tests 使用相同协议形态；此测试驱动不依赖 host runtime。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::config::{LspConfigFile, LspServerConfig};
use crate::pool::LspServerPool;
use async_trait::async_trait;
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode},
    service::{ClientLifecycleMode, Peer, QuitReason, RoleClient, RunningService},
    ServerHandler, ServiceError,
};
use serde_json::{json, Value};

use peri_mcp_common::{invoke_tool_call, list_tools_of};

use super::LspMcpServer;
use crate::tool::LspTool;

/// perl 编写的极简 LSP 服务器（与 `lsp/tool_test.rs` 同源，按本文件需要重抄）：
/// - 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行 "spawned"
/// - didOpen 通知的完整 JSON body 追加到 `$PERI_LSP_TEST_DIDOPEN`
/// - 对任何带 id 的请求回 `{"result":null}`（满足 initialize 握手与查询请求）
const FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"method"\s*:\s*"textDocument\/didOpen"/) {
        open my $f, '>>', $ENV{PERI_LSP_TEST_DIDOPEN} or next;
        print $f "$b\n";
        close $f;
    }
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 无路由扩展名的路径里的可辨认标记：`LspToolError::NoServerForExtension` 会把它（连同
/// 扩展名）嵌进错误原文，IF-D14 映射后**不得**出现在模型面文本里（§9 规则 7）。
const LEAK_MARKER: &str = "secret-marker";
/// 同上：无路由扩展名的文件名标记。
const LEAK_FILE: &str = "private.zzz";
/// `LspToolError::NoServerForExtension` 的原文片段：失败文本里**不得**出现。
const LSP_ERROR_TEXT: &str = "无 LSP 服务器可处理文件";

/// duplex 双向缓冲：单帧 JSON-RPC 消息远小于此容量。
const DUPLEX_BUF: usize = 8 * 1024;
/// client 侧握手上界。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

// ─── 夹具：以 perl fake server 为后端的 pool ───────────────────────────────────

/// 一个 fake-LSP pool 夹具：pool + 两个观测文件 + 目录存活期。
///
/// `dir` **必须**与 pool 同生命周期：配置文件与 spawn 观测文件都在里面。
struct LspFixture {
    pool: Arc<LspServerPool>,
    /// perl 每次 spawn 追加一行；文件不存在 == 从未 spawn。
    spawn_count: PathBuf,
    /// didOpen 通知记录（同一 client 的幂等缓存可被观察）。
    didopen: PathBuf,
    dir: tempfile::TempDir,
}

impl LspFixture {
    fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// 已 spawn 的 language server 进程数（0 == 从未启动，配置仍非空）。
    fn spawns(&self) -> usize {
        std::fs::read_to_string(&self.spawn_count)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// fake server 收到的 didOpen 通知次数。
    fn didopens(&self) -> usize {
        std::fs::read_to_string(&self.didopen)
            .map(|s| s.matches("textDocument/didOpen").count())
            .unwrap_or(0)
    }
}

/// `.rs` → rust 路由的 perl fake server 配置（env 指向夹具临时目录）。
fn fake_server_config(dir: &Path) -> LspServerConfig {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        dir.join("spawn_count.txt").to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_DIDOPEN".to_string(),
        dir.join("didopen.txt").to_string_lossy().into_owned(),
    );
    LspServerConfig {
        name: "fake-lsp".to_string(),
        command: "perl".to_string(),
        args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
        env: Some(env),
        extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
        initialization_options: None,
        disabled: None,
        max_restarts: Some(3),
        startup_timeout: None,
        source: None,
    }
}

/// 生效配置非空的 pool（`LspServerPool::new` 惰性：此处**不**拉进程）。
fn fixture_with_fake_server() -> LspFixture {
    let dir = tempfile::tempdir().unwrap();
    let config = LspConfigFile {
        lsp_servers: HashMap::from([("fake-lsp".to_string(), fake_server_config(dir.path()))]),
    };
    let pool = Arc::new(LspServerPool::new(dir.path().to_str().unwrap(), config));
    LspFixture {
        pool,
        spawn_count: dir.path().join("spawn_count.txt"),
        didopen: dir.path().join("didopen.txt"),
        dir,
    }
}

/// 生效配置**为空**的 pool（夹具空配置形态：`lsp_servers: HashMap::new()`）。
///
/// 空配置下不会有任何进程被拉起来，因此 cwd 只用于构造 `root_uri`，取当前目录即可。
fn empty_config_pool() -> Arc<LspServerPool> {
    Arc::new(LspServerPool::new(
        ".",
        LspConfigFile {
            lsp_servers: HashMap::new(),
        },
    ))
}

// ─── 夹具：原生 rmcp duplex wire ──────────────────────────────────────────────

/// 一条已握手的 handler 链路：client service + server task。
struct Pair {
    service: RunningService<RoleClient, ()>,
    server_task: tokio::task::JoinHandle<Result<QuitReason, tokio::task::JoinError>>,
}

async fn connect<S: rmcp::ServerHandler>(server: S) -> Pair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let (read, write) = tokio::io::split(server_io);
    let server_task = tokio::spawn(async move {
        let running = rmcp::serve_server(server, (read, write))
            .await
            .expect("handler server 装配失败");
        running.waiting().await
    });
    let service = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        rmcp::service::serve_client_with_lifecycle(
            (),
            tokio::io::split(client_io),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
                legacy_version: None,
            },
        ),
    )
    .await
    .expect("client 侧握手超时")
    .expect("client 侧握手失败");
    Pair {
        service,
        server_task,
    }
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：释放 client（server 侧读到 EOF），并有界等待 server task。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        if tokio::time::timeout(CLOSE_TIMEOUT, &mut self.server_task)
            .await
            .is_err()
        {
            self.server_task.abort();
            let _ = self.server_task.await;
        }
    }
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块（失败文本不打印任何夹具常量之外的东西）。
fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// 断言响应是 `Complete` 并取出结果（IF-D14 的失败形态**不是** `Err`）。
fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

/// `documentSymbol` 请求（成功/失败路径共用同一参数形状）。
fn document_symbol(file_path: &str) -> Value {
    json!({ "operation": "documentSymbol", "file_path": file_path })
}

// ─── 门控：工具面 == 生效配置快照（与进程 ready 解耦） ────────────────────────

#[tokio::test]
async fn list_tools_follows_configured_server_set() {
    // ① 有配置但**未启动** language server：工具面照常可见（可见性 ≠ 进程 ready）。
    let fixture = fixture_with_fake_server();
    let server = LspMcpServer::new(Arc::clone(&fixture.pool));
    let listed = list_tools_of(server.tools());
    assert_eq!(
        listed.tools.len(),
        1,
        "生效配置非空 ⇒ 恰好一个工具（注册表单工具）"
    );
    assert_eq!(listed.tools[0].name.as_ref(), "LSP", "原始名逐字为 LSP");
    assert_eq!(fixture.spawns(), 0, "构造 handler 不得拉起 language server");

    let pair = connect(server).await;
    let peer = pair.peer();
    let wire = peer
        .list_all_tools()
        .await
        .expect("tools/list 必须成功（覆写 discover 会让它在会话层被拒）");
    let names: Vec<&str> = wire.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec!["LSP"],
        "「有配置、零进程」时线路上必须恰好一条 LSP——工具面可见性与 language server 是否 ready 无关"
    );
    assert_eq!(
        fixture.spawns(),
        0,
        "只做 tools/list 就拉起 language server == 门控谓词被进程就绪态污染"
    );
    pair.shutdown().await;

    // ② 空配置：`list_tools` 成功且为空表；`get_info()` 仍正常——空表**不**等于实例未就绪。
    let pool = empty_config_pool();
    let empty = LspMcpServer::new(Arc::clone(&pool));
    let info = empty.get_info();
    assert_eq!(
        info.server_info.name, "peri-lsp-mcp",
        "空配置实例的 get_info 必须正常（不得用空表反推未就绪）"
    );
    assert!(info.capabilities.tools.is_some());
    assert!(
        list_tools_of(empty.tools()).tools.is_empty(),
        "生效配置为空 ⇒ 空表，但仍是可用实例"
    );

    let empty_pair = connect(LspMcpServer::new(pool)).await;
    let empty_wire = empty_pair
        .peer()
        .list_all_tools()
        .await
        .expect("空配置的 tools/list 必须成功返回空表，而不是报错");
    assert!(
        empty_wire.is_empty(),
        "空配置 ⇒ 空表；实际：{:?}",
        empty_wire.iter().map(|tool| &tool.name).collect::<Vec<_>>()
    );
    empty_pair.shutdown().await;
}

// ─── 复用既有 LspTool + IF-D14 结果映射 ───────────────────────────────────────

#[tokio::test]
async fn call_tool_reuses_lsp_tool_and_shared_result_mapping() {
    let fixture = fixture_with_fake_server();
    let src = fixture.dir().join("main.rs");
    std::fs::write(&src, "fn main() {}\n").unwrap();
    let src_path = src.to_string_lossy().to_string();

    // ① 声明面：与既有 `LspTool::definition()` 逐字相等（期望值由既有工具生成）。
    let definition = LspTool::new(Arc::clone(&fixture.pool)).definition();
    let listed = list_tools_of(LspMcpServer::new(Arc::clone(&fixture.pool)).tools());
    assert_eq!(listed.tools.len(), 1, "LSP 是单工具实例");
    let mapped = &listed.tools[0];
    assert_eq!(mapped.name.as_ref(), definition.name.as_str());
    assert_eq!(
        mapped.description.as_deref(),
        Some(definition.description.as_str()),
        "description 必须逐字映射"
    );
    assert_eq!(
        Value::Object(mapped.input_schema.as_ref().clone()),
        definition.parameters,
        "input_schema 必须逐字等于既有工具的 parameters()"
    );
    let operations: Vec<&str> = definition.parameters["properties"]["operation"]["enum"]
        .as_array()
        .expect("operation.enum 必须是数组")
        .iter()
        .map(|value| value.as_str().expect("operation 名必须是字符串"))
        .collect();
    assert_eq!(operations.len(), 10, "既有 LspTool 声明十个 operation");
    for operation in &operations {
        assert!(
            definition.description.contains(operation),
            "description 必须逐个列出 operation：缺 {operation}"
        );
    }

    let pair = connect(LspMcpServer::new(Arc::clone(&fixture.pool))).await;
    let peer = pair.peer();

    // ② 成功路径：真实临时 `.rs` 文件（fake server 回 `result: null`）→ IF-D14 success 形态。
    let success = complete(
        peer.call_tool_once(call("LSP", document_symbol(&src_path)))
            .await
            .expect("成功形态必须是协议成功"),
    );
    assert_eq!(success.is_error, Some(false));
    let wire_text = first_text(&success).expect("成功结果必须有文本块");
    assert!(!wire_text.is_empty(), "成功文本不得为空");

    // 与直接调用**同一** pool 上的既有 `LspTool` 的文本逐字相等：handler 只做路由，
    // 不另写 formatter；两次调用打在同一 client 上（didOpen 缓存共享 ⇒ 仍是一次 didOpen）。
    let direct_text = LspTool::new(Arc::clone(&fixture.pool))
        .invoke(document_symbol(&src_path), ToolContext::new(&[], ""))
        .await
        .expect("既有 LspTool 对 fake server 的 documentSymbol 必须成功");
    assert_eq!(
        wire_text, direct_text,
        "handler 的成功文本必须就是既有 LspTool 的返回文本"
    );
    assert_eq!(fixture.spawns(), 1, "同一 pool ⇒ 同一 language server 进程");
    assert_eq!(
        fixture.didopens(),
        1,
        "同一 client 的 didOpen 幂等缓存必须共享（两次调用同一 pool）"
    );

    // ③ 失败路径：无路由扩展名 ⇒ IF-D14 的**固定脱敏** error 文本。
    let zzz_dir = fixture.dir().join(LEAK_MARKER);
    std::fs::create_dir_all(&zzz_dir).unwrap();
    let zzz = zzz_dir.join(LEAK_FILE);
    std::fs::write(&zzz, "no server routes this extension\n").unwrap();

    // 参考文本：**同一** IF-D14 实现（`invoke_tool_call`）对**同名**替身工具的失败形态。
    // 规则文本只由工具名决定，所以这就是 `LSP` 的固定脱敏文本——本文件因此不硬编码规则字面量。
    let stub: Vec<Arc<dyn BaseTool>> = vec![Arc::new(FailingStubTool)];
    let reference = complete(
        invoke_tool_call(&stub, "", &call("LSP", json!({})))
            .await
            .expect("工具级失败必须是 Ok(Complete(error 结果))，不是 Err(internal_error)"),
    );
    assert_eq!(reference.is_error, Some(true));
    let expected = first_text(&reference).expect("错误结果必须有文本块");
    assert!(
        !expected.contains(LSP_ERROR_TEXT) && !expected.contains(LEAK_MARKER),
        "固定文本本身不得含替身工具的原文：{expected}"
    );

    let failure = complete(
        peer.call_tool_once(call("LSP", document_symbol(&zzz.to_string_lossy())))
            .await
            .expect("业务失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(failure.is_error, Some(true));
    let failure_text = first_text(&failure).expect("错误结果必须有文本块");
    assert_eq!(
        failure_text, expected,
        "失败文本必须等于 IF-D14 的固定脱敏文本"
    );
    assert!(
        failure_text.contains("LSP"),
        "规则文本只带工具名：{failure_text}"
    );
    assert!(
        !failure_text.contains(LSP_ERROR_TEXT),
        "不得泄漏 LspToolError 原文：{failure_text}"
    );
    assert!(
        !failure_text.contains(LEAK_MARKER) && !failure_text.contains(LEAK_FILE),
        "不得泄漏未注册路径 / 文件名（§9 规则 7）：{failure_text}"
    );
    assert!(
        !failure_text.contains("扩展名"),
        "不得回传扩展名诊断原文：{failure_text}"
    );

    // ④ 未知工具名 ⇒ 协议错误（IF-D14 第一条分支），且经真实 wire。
    let probe = LspMcpServer::new(Arc::clone(&fixture.pool));
    let missing = invoke_tool_call(probe.tools(), "", &call("LSP_pause", json!({})))
        .await
        .expect_err("未知工具名必须是协议错误（invalid_params）");
    assert_eq!(missing.code.0, ErrorCode::INVALID_PARAMS.0);
    assert!(
        missing.message.contains("unknown tool: LSP_pause"),
        "错误文本应含工具名；实际：{}",
        missing.message
    );
    let wire_error = peer
        .call_tool_once(call("LSP_pause", json!({})))
        .await
        .expect_err("线路上未知工具名必须回 -32602");
    match wire_error {
        ServiceError::McpError(data) => {
            assert_eq!(data.code.0, ErrorCode::INVALID_PARAMS.0);
            assert!(data.message.contains("unknown tool: LSP_pause"));
        }
        other => panic!("期望 McpError(invalid_params)，实际：{other:?}"),
    }

    pair.shutdown().await;
    // 夹具收尾：有界关停真的被拉起过的 language server 进程。
    fixture.pool.shutdown().await;
}

/// 失败替身工具：名字取 `LSP`，因此 `invoke_tool_call` 为它生成的固定文本与真实
/// `LspTool` 失败的**逐字相同**（规则文本只由工具名决定）；错误原文里埋入真实的
/// `LspToolError::NoServerForExtension` 形状串，用于证明映射后原文不泄漏。
struct FailingStubTool;

#[async_trait]
impl BaseTool for FailingStubTool {
    fn name(&self) -> &str {
        "LSP"
    }

    fn description(&self) -> &str {
        "crate 内替身工具（只为生成 IF-D14 的固定文本，不碰 LSP 链路）"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("{LSP_ERROR_TEXT}: /tmp/{LEAK_MARKER}/{LEAK_FILE} (扩展名: zzz)").into())
    }
}

// ─── 快照：构造时取一次，运行期配置合并不倒灌 ─────────────────────────────────

#[tokio::test]
async fn handler_snapshots_tools_after_config_merge() {
    // ① 生效配置非空时构造 ⇒ 已有工具；此后 `add_server` 不改变这一份工具面。
    let fixture = fixture_with_fake_server();
    let server = LspMcpServer::new(Arc::clone(&fixture.pool));
    assert!(
        Arc::ptr_eq(server.pool(), &fixture.pool),
        "同一 pool，不是复制"
    );
    assert_eq!(server.tools().len(), 1, "构造快照：非空配置 ⇒ 一个工具");
    let before: Arc<dyn BaseTool> = Arc::clone(&server.tools()[0]);

    fixture
        .pool
        .add_server(LspServerConfig {
            name: "late-lsp".to_string(),
            command: "perl".to_string(),
            args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
            env: None,
            extension_to_language: HashMap::from([(".late".to_string(), "late".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: Some(1),
            startup_timeout: None,
            source: None,
        })
        .await;
    assert!(
        fixture.pool.has_servers(),
        "配置合并后 pool 侧确实有两个服务器"
    );
    assert_eq!(
        server.tools().len(),
        1,
        "handler 的工具面是构造快照：add_server 不得改变它"
    );
    assert!(
        Arc::ptr_eq(&before, &server.tools()[0]),
        "同一份工具实例：add_server 不得触发重建"
    );
    assert_eq!(
        list_tools_of(server.tools()).tools.len(),
        1,
        "list_tools 也不重算：声明面恒为构造快照"
    );

    // ② 反向：空 pool 构造（空表）后 `add_server` ⇒ handler 仍是空表（快照不倒灌）。
    let late_dir = tempfile::tempdir().unwrap();
    let empty_pool = empty_config_pool();
    let empty_server = LspMcpServer::new(Arc::clone(&empty_pool));
    assert!(
        Arc::ptr_eq(empty_server.pool(), &empty_pool),
        "同一 pool，不是复制"
    );
    assert!(empty_server.tools().is_empty(), "空配置构造 ⇒ 空表");

    empty_pool
        .add_server(fake_server_config(late_dir.path()))
        .await;
    assert!(empty_pool.has_servers(), "pool 侧已非空");
    assert!(
        empty_server.tools().is_empty(),
        "快照不倒灌：构造后才出现的配置不改变已构造实例的工具面"
    );
    assert!(
        list_tools_of(empty_server.tools()).tools.is_empty(),
        "list_tools 不得重算 has_servers()"
    );
}

// ─── get_info：只声明 tools ───────────────────────────────────────────────────

#[test]
fn server_info_declares_tools_only() {
    let configured = fixture_with_fake_server();
    for pool in [Arc::clone(&configured.pool), empty_config_pool()] {
        let info = LspMcpServer::new(pool).get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "必须声明 tools 能力，否则 tools/list 不可达"
        );
        assert!(
            info.capabilities.resources.is_none(),
            "不声明 resources（subscribe / subscriptions 随之不存在）"
        );
        assert!(info.capabilities.prompts.is_none(), "不声明 prompts");
        assert!(info.capabilities.logging.is_none(), "不声明 logging");
        assert!(
            info.capabilities.completions.is_none(),
            "不声明 completions"
        );
        assert!(
            info.capabilities.experimental.is_none(),
            "不声明 experimental 能力"
        );
        assert!(
            info.capabilities.extensions.is_none(),
            "不声明 MCP 扩展能力"
        );

        let implementation = &info.server_info;
        assert_eq!(implementation.name, "peri-lsp-mcp");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
    }
}
