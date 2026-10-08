//! 统一 activation 的线路证据（W2）：真实 rmcp 客户端 + 原始 JSON-RPC server
//! （仅 client feature，无 rmcp server 面）。
//!
//! 覆盖验收：激活才读正文（`resources/read` 计数）、digest/frontmatter 失败
//! 不注入、stale 经 `skills/get` 刷新一次后重读、legacy 无绑定条目退化为
//! 发现期已校验正文、断连/取消/非 MCP 来源的显式错误。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::{
    mcp_skills::{HandleToken, McpSkillRegistry},
    skills::{SkillMetadata, SkillResource, SkillSource},
};
use rmcp::service::RunningService;
use rmcp::RoleClient;
use sha2::{Digest, Sha256};

use super::{activate, ActivationError};
use crate::mcp::client::{ClientStatus, McpClientHandle, OAuthStatus};

const SERVER: &str = "srv";
const URI: &str = "skill://srv/alpha/SKILL.md";

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    format!("sha256:{:x}", digest)
}

fn body(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\n# Body\n")
}

fn fm_json(name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "description": description })
}

/// `skills/get` 返回的条目快照（camelCase 与 wire 一致）。
fn entry_json(digest: &str, fm: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "uri": URI,
        "frontmatter": fm,
        "resources": [ { "uri": URI, "digest": digest } ],
    })
}

/// 发现面产出的绑定条目（与 `build_metadata` 同形：origin + resources + frontmatter）。
fn bound_meta(digest: &str, fm: serde_json::Value) -> SkillMetadata {
    SkillMetadata {
        name: "mcp__srv__alpha".to_string(),
        aliases: Vec::new(),
        description: "Alpha skill".to_string(),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(peri_acp_types::skills::SkillOrigin::Mcp {
            server: SERVER.to_string(),
            uri: URI.to_string(),
        }),
        content: None,
        resources: vec![SkillResource {
            uri: URI.to_string(),
            digest: digest.to_string(),
        }],
        frontmatter: fm.as_object().cloned(),
    }
}

/// 原始 JSON-RPC responder：`resources/read` 恒回 `body`；`skills/get` 回当前
/// `entry` 快照；两者各自计数。
/// 原始 server 的观测计数器（读取 / skills/get / 方法序列）。
#[derive(Clone, Default)]
struct Counters {
    reads: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
    methods: Arc<parking_lot::Mutex<Vec<String>>>,
}

async fn raw_activation_server(
    io: tokio::io::DuplexStream,
    body: String,
    entry: Arc<parking_lot::Mutex<serde_json::Value>>,
    list_skills: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    templates: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    counters: Counters,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line.trim_end()) else {
            line.clear();
            continue;
        };
        line.clear();
        let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = parsed["method"].as_str().unwrap_or_default().to_string();
        let uri = parsed["params"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        counters.methods.lock().push(method.clone());
        let response = match method.as_str() {
            "skills/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "skills": list_skills.lock().clone() }
            }),
            "resources/templates/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "resourceTemplates": templates.lock().clone() }
            }),
            "resources/read" => {
                counters.reads.fetch_add(1, Ordering::SeqCst);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "contents": [ { "uri": uri, "mimeType": "text/plain", "text": body } ]
                    }
                })
            }
            "skills/get" => {
                counters.gets.fetch_add(1, Ordering::SeqCst);
                let skill = entry.lock().clone();
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "skill": skill }
                })
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" }
            }),
        };
        let mut w = writer.lock().await;
        w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .await
            .unwrap();
        w.write_all(b"\n").await.unwrap();
    }
}

struct Fixture {
    registry: Arc<McpSkillRegistry>,
    counters: Counters,
    entry: Arc<parking_lot::Mutex<serde_json::Value>>,
    list_skills: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    templates: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    _server: tokio::task::JoinHandle<()>,
    _client: RunningService<RoleClient, ()>,
}

impl Fixture {
    fn reads(&self) -> usize {
        self.counters.reads.load(Ordering::SeqCst)
    }

    fn gets(&self) -> usize {
        self.counters.gets.load(Ordering::SeqCst)
    }

    fn set_entry(&self, entry: serde_json::Value) {
        *self.entry.lock() = entry;
    }

    fn handle(&self) -> Arc<McpClientHandle> {
        self.registry
            .discovery_state(SERVER)
            .map(|state| match state {
                peri_acp_types::mcp_skills::ServerDiscoveryState::Started { handle }
                | peri_acp_types::mcp_skills::ServerDiscoveryState::Discovered { handle, .. } => {
                    handle
                }
            })
            .and_then(|token| token.downcast::<McpClientHandle>().ok())
            .expect("夹具 registry 必须持有真实 McpClientHandle")
    }
}

/// 装配：duplex 原始 server + 真客户端 peer + registry Discovered（handle =
/// 真实 `McpClientHandle`，激活经 `peer_of` downcast 解析当前代 peer）。
async fn fixture(
    body: String,
    entry: serde_json::Value,
    discovered: Vec<SkillMetadata>,
) -> Fixture {
    let counters = Counters::default();
    let entry = Arc::new(parking_lot::Mutex::new(entry));
    let list_skills = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let templates = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(raw_activation_server(
        server_io,
        body,
        Arc::clone(&entry),
        Arc::clone(&list_skills),
        Arc::clone(&templates),
        counters.clone(),
    ));
    let client = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = Arc::new(McpClientHandle {
        name: SERVER.to_string(),
        version: None,
        cache_version: None,
        peer: Some(client.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: true,
    });
    let registry = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = handle.clone();
    registry.mark_discovery_started(SERVER, token.clone());
    registry.mark_discovery_completed(SERVER, token, discovered);
    Fixture {
        registry,
        counters,
        entry,
        list_skills,
        templates,
        _server: server,
        _client: client,
    }
}

// ─── 成功路径：激活才读正文 ─────────────────────────────────────────────────

#[tokio::test]
async fn activation_reads_body_once_and_returns_verified_content() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    let fx = fixture(
        text.clone(),
        entry_json(&digest, fm_json("alpha", "Alpha skill")),
        vec![bound_meta(&digest, fm_json("alpha", "Alpha skill"))],
    )
    .await;
    assert_eq!(fx.reads(), 0, "装配（发现完成）不得读取正文");

    let content = activate(&fx.registry, &fx.registry.all_skills()[0], None)
        .await
        .expect("绑定一致必须激活成功");
    assert_eq!(content, text);
    assert_eq!(fx.reads(), 1, "激活恰好读一次");
    assert_eq!(fx.gets(), 0, "校验通过不触发 skills/get");
}

// ─── stale：digest 不一致 → skills/get 刷新一次 → 重读并校验 ────────────────

#[tokio::test]
async fn activation_refreshes_once_after_digest_mismatch() {
    let text = body("alpha", "Alpha skill");
    let good = sha256_hex(&text);
    let stale = sha256_hex("other content");
    let fx = fixture(
        text.clone(),
        // v1 条目 digest 与内容不符（stale）；skills/get 返回修正后的 v2。
        entry_json(&stale, fm_json("alpha", "Alpha skill")),
        vec![bound_meta(&stale, fm_json("alpha", "Alpha skill"))],
    )
    .await;
    fx.set_entry(entry_json(&good, fm_json("alpha", "Alpha skill")));

    let content = activate(&fx.registry, &fx.registry.all_skills()[0], None)
        .await
        .expect("刷新一次后必须成功");
    assert_eq!(content, text);
    assert_eq!(fx.gets(), 1, "digest stale 触发一次 skills/get");
    assert_eq!(
        fx.reads(),
        2,
        "首读（stale 命中）+ 恢复路径按刷新条目重读一次"
    );

    // 回写：registry 条目已换成刷新后的 digest（后续激活不再需要刷新）。
    let updated = &fx.registry.all_skills()[0];
    assert_eq!(
        updated.resources[0].digest, good,
        "刷新条目必须回写 registry"
    );
}

#[tokio::test]
async fn activation_refresh_failure_stays_rejected() {
    let text = body("alpha", "Alpha skill");
    let stale = sha256_hex("other content");
    let fx = fixture(
        text,
        // skills/get 仍返回同一 stale 条目 → 恢复失败，不写半成品。
        entry_json(&stale, fm_json("alpha", "Alpha skill")),
        vec![bound_meta(&stale, fm_json("alpha", "Alpha skill"))],
    )
    .await;

    let error = activate(&fx.registry, &fx.registry.all_skills()[0], None)
        .await
        .expect_err("恢复后仍不一致必须失败");
    assert_eq!(error, ActivationError::DigestMismatch);
    assert_eq!(fx.gets(), 1);
    assert_eq!(
        fx.registry.all_skills()[0].resources[0].digest,
        stale,
        "恢复失败不得改写条目"
    );
}

// ─── frontmatter 差异不是 stale：不重试、不注入 ─────────────────────────────

#[tokio::test]
async fn activation_frontmatter_mismatch_is_not_retried() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    // 条目快照 description 与正文不同（陈旧或篡改）。
    let fx = fixture(
        text,
        entry_json(&digest, fm_json("alpha", "Tampered")),
        vec![bound_meta(&digest, fm_json("alpha", "Tampered"))],
    )
    .await;

    let error = activate(&fx.registry, &fx.registry.all_skills()[0], None)
        .await
        .expect_err("frontmatter 不一致必须拒绝");
    assert_eq!(error, ActivationError::FrontmatterMismatch);
    assert_eq!(fx.reads(), 1, "正文已读一次");
    assert_eq!(fx.gets(), 0, "frontmatter 失败不是 stale，不发 skills/get");
}

// ─── legacy / 断连 / 取消 / 非 MCP 来源 ─────────────────────────────────────

#[tokio::test]
async fn activation_without_binding_uses_discovery_time_content() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    let fx = fixture(
        text.clone(),
        entry_json(&digest, fm_json("alpha", "Alpha skill")),
        Vec::new(),
    )
    .await;
    // legacy 条目：无 resources 绑定、正文在发现期已校验并缓存。
    let mut legacy = bound_meta(&digest, fm_json("alpha", "Alpha skill"));
    legacy.resources.clear();
    legacy.content = Some(text.clone());

    let content = activate(&fx.registry, &legacy, None)
        .await
        .expect("legacy 兜底必须可用");
    assert_eq!(content, text);
    assert_eq!(fx.reads(), 0, "legacy 兜底不产生激活期读取");

    // 无绑定且无缓存正文 → 显式缺口（不静默返回空）。
    legacy.content = None;
    assert_eq!(
        activate(&fx.registry, &legacy, None).await.unwrap_err(),
        ActivationError::MissingBinding
    );
}

#[tokio::test]
async fn activation_reports_unreachable_cancelled_and_non_mcp() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    let meta = bound_meta(&digest, fm_json("alpha", "Alpha skill"));
    let fx = fixture(
        text,
        entry_json(&digest, fm_json("alpha", "Alpha skill")),
        vec![meta.clone()],
    )
    .await;

    // 取消：立即返回 Cancelled，且不发起读取。
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        activate(&fx.registry, &meta, Some(&cancel))
            .await
            .unwrap_err(),
        ActivationError::Cancelled
    );
    assert_eq!(fx.reads(), 0);

    // 未连接（registry 无该 server）：Unreachable，不回落磁盘/缓存。
    let empty = McpSkillRegistry::new();
    assert_eq!(
        activate(&empty, &meta, None).await.unwrap_err(),
        ActivationError::Unreachable
    );

    // 非 MCP 来源（本地/内置）：明确拒绝，由调用方走各自读取路径。
    let mut local = meta.clone();
    local.source = SkillSource::Project;
    local.origin = None;
    assert_eq!(
        activate(&fx.registry, &local, None).await.unwrap_err(),
        ActivationError::NotMcpSourced
    );
}

// ─── 边界可发现性（W2）：空列表 / 模板兜底（legacy）────────────────────────

/// 空 `skills/list` 是合法结果：发现完成（空条目）、零读取、不报错。
#[tokio::test]
async fn empty_skills_list_completes_without_reads() {
    let fx = fixture(String::new(), serde_json::Value::Null, Vec::new()).await;
    // skills/list 返回空（list_skills 缺省为空）
    let handle = fx.handle();
    let token: HandleToken = handle.clone();
    crate::mcp::skill_discovery::run_discovery(
        Arc::clone(&fx.registry),
        None,
        handle,
        token,
        tokio_util::sync::CancellationToken::new(),
    )
    .await;
    assert!(fx.registry.all_skills().is_empty(), "空列表→空条目（合法）");
    assert!(fx.list_skills.lock().is_empty(), "夹具前置：list 载荷为空");
    assert_eq!(fx.reads(), 0, "空列表不得触发正文读取");
    assert!(fx
        .counters
        .methods
        .lock()
        .iter()
        .any(|m| m == "skills/list"));
}

/// legacy 且无 enrolled 候选、仅经 `resources/templates/list` 暴露 skill 形态
/// 模板：发现完成（空条目，模板不可展开）+ 探测请求发出（可发现性信号）。
#[tokio::test]
async fn legacy_templates_only_server_probes_without_error() {
    let fx = fixture(String::new(), serde_json::Value::Null, Vec::new()).await;
    fx.templates.lock().push(serde_json::json!({
        "uriTemplate": "skill://{name}/SKILL.md",
        "name": "skill-entry",
    }));
    // legacy 句柄：skills_capable=false + 无 enrolled 资源
    let legacy = Arc::new(McpClientHandle {
        name: SERVER.to_string(),
        version: None,
        cache_version: None,
        peer: Some(fx._client.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    });
    let token: HandleToken = legacy.clone();
    fx.registry.mark_discovery_started(SERVER, token.clone());
    crate::mcp::skill_discovery::run_discovery(
        Arc::clone(&fx.registry),
        None,
        legacy,
        token,
        tokio_util::sync::CancellationToken::new(),
    )
    .await;

    assert!(
        fx.registry.all_skills().is_empty(),
        "模板不可展开 → 不注册候选（空结果是合法发现）"
    );
    assert!(
        fx.counters
            .methods
            .lock()
            .iter()
            .any(|m| m == "resources/templates/list"),
        "候选空时必须探测一次模板面：{:?}",
        fx.counters.methods.lock()
    );
    assert_eq!(fx.reads(), 0, "模板探测不得读取正文");
}

// ─── 消费面接线：SkillTool 的 MCP 分支经统一 activation ─────────────────────

#[tokio::test]
async fn skill_tool_activates_mcp_skill_through_registry() {
    use peri_agent::tools::{BaseTool, ToolContext};

    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    let fx = fixture(
        text.clone(),
        entry_json(&digest, fm_json("alpha", "Alpha skill")),
        vec![bound_meta(&digest, fm_json("alpha", "Alpha skill"))],
    )
    .await;

    let tool = crate::skills::tools::SkillTool::new(Some(Arc::clone(&fx.registry)));
    let output = tool
        .invoke(
            serde_json::json!({ "skill_name": "mcp__srv__alpha" }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .expect("SkillTool 的 MCP 分支必须经 activation 成功");
    assert!(output.contains("# Body"), "应含激活正文：{output}");
    assert!(
        output.contains("This skill is served by MCP server \"srv\""),
        "MCP 来源标注必须保留（与 content::load 同源）：{output}"
    );
    assert_eq!(fx.reads(), 1, "SkillTool 正文恰来自一次 activation 读取");
}

// ─── 裁决 B（W2 验收）：真实 workspace provider 经生产 client 链路的端到端 ──
//
// 真实 `WorkspaceMcpServer` + W1 `with_resources`（临时 roots，生产装配不动）
// → 生产 transport/client 握手（`spawn_builtin_transport_with_handler` +
// `serve_client_auto`）→ 真实发现（`skills/list`）→ 统一 activation → SkillTool
// 注入文本。生产切换随 W4（不启用宿主投递）。
mod real_workspace_provider {
    use std::path::PathBuf;
    use std::sync::Arc;

    use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
    use peri_agent::tools::{BaseTool, ToolContext};
    use peri_mcp_workspace::{
        ResourceRoot, ResourceScope, WorkspaceMcpServer, WorkspaceResourcesInput,
    };

    use crate::mcp::builtin::runtime::{BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT};
    use crate::mcp::client::{ClientStatus, McpClientHandle, McpServiceWrapper, OAuthStatus};
    use crate::mcp::McpCapabilityProfile;
    use crate::skills::tools::SkillTool;

    const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(5);

    /// 真实 provider + 生产链路夹具：临时技能根 + `with_resources` + 生产
    /// transport/client + 真实发现写 registry + 共享同一 registry 的 SkillTool。
    struct RealProvider {
        registry: Arc<McpSkillRegistry>,
        tool: SkillTool,
        entry: PathBuf,
        _root: tempfile::TempDir,
        supervisor: BuiltinInstanceSupervisor,
        service: McpServiceWrapper,
    }

    impl RealProvider {
        async fn wire(body: &str) -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let dir = root.path().join("alpha");
            std::fs::create_dir_all(&dir).expect("建技能目录");
            let entry = dir.join("SKILL.md");
            std::fs::write(
                &entry,
                format!("---\nname: alpha\ndescription: Alpha skill\n---\n\n{body}\n"),
            )
            .expect("写 SKILL.md");

            let handler = WorkspaceMcpServer::new(root.path().to_string_lossy().to_string(), None)
                .with_resources(
                    WorkspaceResourcesInput::new()
                        .with_skill_root(ResourceRoot::new(root.path(), ResourceScope::Project))
                        .with_disable_bundled(true),
                );
            let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler(
                "workspace",
                handler,
            );
            let (io, supervisor) = transport.into_parts();
            let service = crate::mcp::client::serve_client_auto(
                io,
                &McpCapabilityProfile::disabled(),
                HANDSHAKE,
            )
            .await
            .expect("生产 client 握手不得超时")
            .expect("生产 client 握手不得失败");

            let handle = Arc::new(McpClientHandle {
                name: "workspace".to_string(),
                version: None,
                cache_version: None,
                peer: Some(service.peer().clone()),
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Connected,
                oauth_status: OAuthStatus::default(),
                source: None,
                url: None,
                skills_capable: true,
            });
            let registry = Arc::new(McpSkillRegistry::new());
            let token: HandleToken = handle.clone();
            registry.mark_discovery_started("workspace", token.clone());
            crate::mcp::skill_discovery::run_discovery(
                Arc::clone(&registry),
                None,
                handle,
                token,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;

            let tool = SkillTool::new(Some(Arc::clone(&registry)));
            Self {
                registry,
                tool,
                entry,
                _root: root,
                supervisor,
                service,
            }
        }

        async fn invoke(&self, name: &str) -> Result<String, String> {
            self.tool
                .invoke(
                    serde_json::json!({ "skill_name": name }),
                    ToolContext::new(&[], "/tmp"),
                )
                .await
                .map_err(|error| error.to_string())
        }

        async fn shutdown(mut self) {
            let _ = self
                .service
                .close_with_timeout(std::time::Duration::from_millis(500))
                .await;
            self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        }
    }

    /// 裁决 B（W2 验收）：真实 provider 的发现 → activation → 注入文本，
    /// 含 digest 漂移经 `skills/get` 一次刷新重试、内容消失拒绝注入。
    #[tokio::test]
    async fn real_workspace_provider_discovery_activation_and_injection() {
        let fixture = RealProvider::wire("# Alpha v1").await;

        // 1) 发现只发布 metadata：真实 skills/list 已跑过，正文未随条目携带。
        let skills = fixture.registry.all_skills();
        assert_eq!(skills.len(), 1, "真实 provider 应发布一个技能");
        assert_eq!(skills[0].name, "mcp__workspace__alpha");
        assert!(skills[0].content.is_none(), "发现期不得携带正文");
        assert!(skills[0].frontmatter.is_some(), "必须携带 frontmatter 快照");
        assert!(
            skills[0]
                .resources
                .iter()
                .any(|r| r.uri.ends_with("/SKILL.md")),
            "manifest 必须含 SKILL.md 自身条目"
        );

        // 2) 激活 → 注入文本（含来源标注）。
        let out = fixture
            .invoke("mcp__workspace__alpha")
            .await
            .expect("真实 provider 激活应成功");
        assert!(out.contains("# Alpha v1"), "应注入 v1 正文：{out}");
        assert!(
            out.contains("This skill is served by MCP server \"workspace\""),
            "注入文本必须带来源标注：{out}"
        );

        // 3) digest 漂移：内容被替换 → activation 经 skills/get 刷新一次 →
        //    按新条目重读校验 → 注入新正文（真实 provider 每请求重扫，刷新条目
        //    的 digest 与新内容一致）。
        std::fs::write(
            &fixture.entry,
            "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha v2\n",
        )
        .expect("替换内容");
        let out = fixture
            .invoke("mcp__workspace__alpha")
            .await
            .expect("漂移后经一次刷新应成功");
        assert!(out.contains("# Alpha v2"), "应注入刷新后的 v2 正文：{out}");

        // 4) 内容消失：读取失败 → 显式拒绝（不注入、不回落旧内容）。
        std::fs::remove_file(&fixture.entry).expect("删除 SKILL.md");
        let error = fixture
            .invoke("mcp__workspace__alpha")
            .await
            .expect_err("内容不可得必须显式失败");
        assert!(error.contains("cannot activate"), "实际: {error}");

        fixture.shutdown().await;
    }
}

/// X6（W2b 核实并锁定）：本地/系统**受信来源免逐技能批准，但不免完整性校验**
/// ——受信 origin 的条目同样要过 digest + frontmatter 全量比对，失败一律不注入。
#[tokio::test]
async fn trusted_origin_still_verifies_content_binding() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    let fx = fixture(
        text,
        entry_json(&digest, fm_json("alpha", "Tampered")),
        Vec::new(),
    )
    .await;
    // 同一真实 peer 注册到受信 server 名 "workspace"（host 绑定的实例身份，
    // 不是资源文本自称），条目 origin 指向它。
    let handle = fx.handle();
    fx.registry
        .mark_discovery_started("workspace", handle.clone());
    let mut meta = bound_meta(&digest, fm_json("alpha", "Tampered"));
    if let Some(peri_acp_types::skills::SkillOrigin::Mcp { server, .. }) = &mut meta.origin {
        *server = "workspace".to_string();
    }
    fx.registry
        .mark_discovery_completed("workspace", handle, vec![meta.clone()]);

    let error = activate(&fx.registry, &meta, None)
        .await
        .expect_err("受信来源也必须通过 frontmatter 全量校验");
    assert_eq!(error, ActivationError::FrontmatterMismatch);
    assert_eq!(
        fx.gets(),
        0,
        "frontmatter 失败不是 stale，不触发 skills/get"
    );
}
