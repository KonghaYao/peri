use super::Digest as _;
use super::{
    ActivationError, Arc, AtomicUsize, ClientStatus, HandleToken, McpClientHandle,
    McpSkillRegistry, OAuthStatus, Ordering, RoleClient, RunningService, Sha256, SkillMetadata,
    SkillResource, SkillSource,
};

pub(super) const SERVER: &str = "srv";

pub(super) const URI: &str = "skill://srv/alpha/SKILL.md";

pub(super) fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sha256:{hex}")
}

pub(super) fn body(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\n# Body\n")
}

pub(super) fn fm_json(name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "description": description })
}

/// `skills/get` 返回的条目快照（camelCase 与 wire 一致）。
pub(super) fn entry_json(digest: &str, fm: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "uri": URI,
        "frontmatter": fm,
        "resources": [ { "uri": URI, "digest": digest } ],
    })
}

/// 发现面产出的绑定条目（与 `build_metadata` 同形：origin + resources + frontmatter）。
pub(super) fn bound_meta(digest: &str, fm: serde_json::Value) -> SkillMetadata {
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
pub(super) struct Counters {
    pub(super) reads: Arc<AtomicUsize>,
    pub(super) gets: Arc<AtomicUsize>,
    pub(super) methods: Arc<parking_lot::Mutex<Vec<String>>>,
}

pub(super) async fn raw_activation_server(
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

pub(super) struct Fixture {
    pub(super) registry: Arc<McpSkillRegistry>,
    pub(super) counters: Counters,
    pub(super) entry: Arc<parking_lot::Mutex<serde_json::Value>>,
    pub(super) list_skills: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    pub(super) templates: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    pub(super) _server: tokio::task::JoinHandle<()>,
    pub(super) _client: RunningService<RoleClient, ()>,
}

impl Fixture {
    pub(super) fn reads(&self) -> usize {
        self.counters.reads.load(Ordering::SeqCst)
    }

    pub(super) fn gets(&self) -> usize {
        self.counters.gets.load(Ordering::SeqCst)
    }

    pub(super) fn set_entry(&self, entry: serde_json::Value) {
        *self.entry.lock() = entry;
    }

    pub(super) fn handle(&self) -> Arc<McpClientHandle> {
        self.registry
            .discovery_state(SERVER)
            .map(|state| match state {
                peri_acp_types::mcp_skills::ServerDiscoveryState::Started { handle }
                | peri_acp_types::mcp_skills::ServerDiscoveryState::Failed { handle }
                | peri_acp_types::mcp_skills::ServerDiscoveryState::Discovered { handle, .. } => {
                    handle
                }
            })
            .and_then(|token| token.downcast::<McpClientHandle>().ok())
            .expect("夹具 registry 必须持有真实 McpClientHandle")
    }
}

/// 装配：duplex 原始 server + 真客户端 peer + registry Discovered（handle =
/// 真实 `McpClientHandle`，激活经 `connection_of` 解析当前代连接）。
pub(super) async fn fixture(
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
        call_gate: Default::default(),
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
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
pub(super) async fn activation_reads_body_once_and_returns_verified_content() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    let fx = fixture(
        text.clone(),
        entry_json(&digest, fm_json("alpha", "Alpha skill")),
        vec![bound_meta(&digest, fm_json("alpha", "Alpha skill"))],
    )
    .await;
    assert_eq!(fx.reads(), 0, "装配（发现完成）不得读取正文");

    let content = super::super::activate(&fx.registry, &fx.registry.all_skills()[0], None)
        .await
        .expect("绑定一致必须激活成功");
    assert_eq!(content, text);
    assert_eq!(fx.reads(), 1, "激活恰好读一次");
    assert_eq!(fx.gets(), 0, "校验通过不触发 skills/get");
}

// ─── stale：digest 不一致 → skills/get 刷新一次 → 重读并校验 ────────────────

#[tokio::test]
pub(super) async fn activation_refreshes_once_after_digest_mismatch() {
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

    let content = super::super::activate(&fx.registry, &fx.registry.all_skills()[0], None)
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
pub(super) async fn activation_refresh_failure_stays_rejected() {
    let text = body("alpha", "Alpha skill");
    let stale = sha256_hex("other content");
    let fx = fixture(
        text,
        // skills/get 仍返回同一 stale 条目 → 恢复失败，不写半成品。
        entry_json(&stale, fm_json("alpha", "Alpha skill")),
        vec![bound_meta(&stale, fm_json("alpha", "Alpha skill"))],
    )
    .await;

    let error = super::super::activate(&fx.registry, &fx.registry.all_skills()[0], None)
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
pub(super) async fn activation_frontmatter_mismatch_is_not_retried() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    // 条目快照 description 与正文不同（陈旧或篡改）。
    let fx = fixture(
        text,
        entry_json(&digest, fm_json("alpha", "Tampered")),
        vec![bound_meta(&digest, fm_json("alpha", "Tampered"))],
    )
    .await;

    let error = super::super::activate(&fx.registry, &fx.registry.all_skills()[0], None)
        .await
        .expect_err("frontmatter 不一致必须拒绝");
    assert_eq!(error, ActivationError::FrontmatterMismatch);
    assert_eq!(fx.reads(), 1, "正文已读一次");
    assert_eq!(fx.gets(), 0, "frontmatter 失败不是 stale，不发 skills/get");
}

// ─── legacy / 断连 / 取消 / 非 MCP 来源 ─────────────────────────────────────

#[tokio::test]
pub(super) async fn activation_without_binding_uses_discovery_time_content() {
    let text = body("alpha", "Alpha skill");
    let digest = sha256_hex(&text);
    // legacy 条目：无 resources 绑定、正文在发现期已校验并缓存。
    let mut legacy = bound_meta(&digest, fm_json("alpha", "Alpha skill"));
    legacy.resources.clear();
    legacy.content = Some(text.clone());
    let fx = fixture(
        text.clone(),
        entry_json(&digest, fm_json("alpha", "Alpha skill")),
        vec![legacy.clone()],
    )
    .await;

    let content = super::super::activate(&fx.registry, &legacy, None)
        .await
        .expect("legacy 兜底必须可用");
    assert_eq!(content, text);
    assert_eq!(fx.reads(), 0, "legacy 兜底不产生激活期读取");

    // 无绑定且无缓存正文 → 显式缺口（不静默返回空）。
    legacy.content = None;
    let token: HandleToken = fx.handle();
    fx.registry
        .refresh_entries(SERVER, &token, vec![legacy.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &legacy, None)
            .await
            .unwrap_err(),
        ActivationError::MissingBinding
    );
}

#[tokio::test]
pub(super) async fn legacy_cached_content_requires_connected_open_handle() {
    let mut legacy = bound_meta("unused", fm_json("alpha", "Alpha skill"));
    legacy.resources.clear();
    legacy.content = Some("verified at discovery".into());
    let fx = fixture(String::new(), serde_json::Value::Null, vec![legacy.clone()]).await;
    // 缓存正文不需要 peer，但仍须持有当前连接的调用许可。
    let mut client = (*fx.handle()).clone();
    client.peer = None;
    let handle = Arc::new(client.clone());
    let token: HandleToken = handle.clone();
    fx.registry.mark_discovery_started(SERVER, token.clone());
    fx.registry
        .mark_discovery_completed(SERVER, token, vec![legacy.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &legacy, None)
            .await
            .unwrap(),
        "verified at discovery"
    );
    handle.call_gate.revoke();
    assert_eq!(
        super::super::activate(&fx.registry, &legacy, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable
    );
    handle.call_gate.drain().await;

    client.call_gate = Default::default();
    client.status = ClientStatus::Disconnected;
    let token: HandleToken = Arc::new(client);
    fx.registry.mark_discovery_started(SERVER, token.clone());
    fx.registry
        .mark_discovery_completed(SERVER, token, vec![legacy.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &legacy, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable
    );
    assert_eq!(fx.reads(), 0);
}

#[tokio::test]
pub(super) async fn legacy_cached_content_requires_current_discovered_entry() {
    let mut legacy = bound_meta("unused", fm_json("alpha", "Alpha skill"));
    legacy.resources.clear();
    legacy.content = Some("old verified content".into());
    let fx = fixture(String::new(), serde_json::Value::Null, vec![legacy.clone()]).await;
    let token: HandleToken = Arc::new((*fx.handle()).clone());
    fx.registry.mark_discovery_started(SERVER, token.clone());
    assert_eq!(
        super::super::activate(&fx.registry, &legacy, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable,
        "重连尚在发现期间不得激活旧缓存"
    );
    let mut current = legacy.clone();
    current.content = Some("new verified content".into());
    fx.registry
        .mark_discovery_completed(SERVER, token, vec![current.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &legacy, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable,
        "旧快照不得借用新连接激活旧缓存"
    );
    assert_eq!(
        super::super::activate(&fx.registry, &current, None)
            .await
            .unwrap(),
        "new verified content"
    );
    fx.registry.project_connected(&[]);
    assert_eq!(
        super::super::activate(&fx.registry, &current, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable
    );
    assert_eq!(fx.reads(), 0);
}

#[tokio::test]
pub(super) async fn bound_entry_cannot_fall_back_to_cached_content() {
    let mut bound = bound_meta("unused", fm_json("alpha", "Alpha skill"));
    bound.content = Some("cached content must not bypass binding checks".into());
    let fx = fixture(String::new(), serde_json::Value::Null, vec![bound.clone()]).await;
    let mut client = (*fx.handle()).clone();
    client.peer = None;
    let token: HandleToken = Arc::new(client);
    fx.registry.mark_discovery_started(SERVER, token.clone());
    fx.registry
        .mark_discovery_completed(SERVER, token.clone(), vec![bound.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &bound, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable,
        "绑定正文仍需当前 peer"
    );
    let mut incomplete = bound.clone();
    incomplete.frontmatter = None;
    fx.registry
        .refresh_entries(SERVER, &token, vec![incomplete.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &incomplete, None)
            .await
            .unwrap_err(),
        ActivationError::MissingBinding,
        "缺失 frontmatter 不得回落缓存正文"
    );
    incomplete = bound;
    incomplete.resources[0].uri = "skill://srv/other/SKILL.md".into();
    fx.registry
        .refresh_entries(SERVER, &token, vec![incomplete.clone()]);
    assert_eq!(
        super::super::activate(&fx.registry, &incomplete, None)
            .await
            .unwrap_err(),
        ActivationError::MissingBinding,
        "未包含当前技能的资源绑定不得回落缓存正文"
    );
    assert_eq!(fx.reads(), 0);
}

#[tokio::test]
pub(super) async fn activation_reports_unreachable_cancelled_and_non_mcp() {
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
        super::super::activate(&fx.registry, &meta, Some(&cancel))
            .await
            .unwrap_err(),
        ActivationError::Cancelled
    );
    assert_eq!(fx.reads(), 0);

    // 未连接（registry 无该 server）：Unreachable，不回落磁盘/缓存。
    let empty = McpSkillRegistry::new();
    assert_eq!(
        super::super::activate(&empty, &meta, None)
            .await
            .unwrap_err(),
        ActivationError::Unreachable
    );

    // 非 MCP 来源（本地/内置）：明确拒绝，由调用方走各自读取路径。
    let mut local = meta.clone();
    local.source = SkillSource::Project;
    local.origin = None;
    assert_eq!(
        super::super::activate(&fx.registry, &local, None)
            .await
            .unwrap_err(),
        ActivationError::NotMcpSourced
    );
}

// ─── 边界可发现性（W2）：空列表 / 模板兜底（legacy）────────────────────────

/// 空 `skills/list` 是合法结果：发现完成（空条目）、零读取、不报错。
#[tokio::test]
pub(super) async fn empty_skills_list_completes_without_reads() {
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
pub(super) async fn legacy_templates_only_server_probes_without_error() {
    let fx = fixture(String::new(), serde_json::Value::Null, Vec::new()).await;
    fx.templates.lock().push(serde_json::json!({
        "uriTemplate": "skill://{name}/SKILL.md",
        "name": "skill-entry",
    }));
    // legacy 句柄：skills_capable=false + 无 enrolled 资源
    let legacy = Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
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
pub(super) async fn skill_tool_activates_mcp_skill_through_registry() {
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
    assert!(!output.contains("mcp__srv__"), "不得伪造工具前缀");
    assert!(output.contains("# Body"), "应含激活正文：{output}");
    assert!(
        output.lines().next()
            == Some("This skill is served by MCP server \"srv\", uri: skill://srv/alpha/SKILL.md."),
        "MCP 来源标注必须保留（与 content::load 同源）：{output}"
    );
    assert_eq!(fx.reads(), 1, "SkillTool 正文恰来自一次 activation 读取");
}

/// X6（W2b 核实并锁定）：本地/系统**受信来源免逐技能批准，但不免完整性校验**
/// ——受信 origin 的条目同样要过 digest + frontmatter 全量比对，失败一律不注入。
#[tokio::test]
pub(super) async fn trusted_origin_still_verifies_content_binding() {
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

    let error = super::super::activate(&fx.registry, &meta, None)
        .await
        .expect_err("受信来源也必须通过 frontmatter 全量校验");
    assert_eq!(error, ActivationError::FrontmatterMismatch);
    assert_eq!(
        fx.gets(),
        0,
        "frontmatter 失败不是 stale，不触发 skills/get"
    );
}
