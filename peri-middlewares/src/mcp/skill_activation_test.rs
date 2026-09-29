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
async fn raw_activation_server(
    io: tokio::io::DuplexStream,
    body: String,
    entry: Arc<parking_lot::Mutex<serde_json::Value>>,
    reads: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
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
        let response = match method.as_str() {
            "resources/read" => {
                reads.fetch_add(1, Ordering::SeqCst);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "contents": [ { "uri": uri, "mimeType": "text/plain", "text": body } ]
                    }
                })
            }
            "skills/get" => {
                gets.fetch_add(1, Ordering::SeqCst);
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
    reads: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
    entry: Arc<parking_lot::Mutex<serde_json::Value>>,
    _server: tokio::task::JoinHandle<()>,
    _client: RunningService<RoleClient, ()>,
}

impl Fixture {
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn gets(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }

    fn set_entry(&self, entry: serde_json::Value) {
        *self.entry.lock() = entry;
    }
}

/// 装配：duplex 原始 server + 真客户端 peer + registry Discovered（handle =
/// 真实 `McpClientHandle`，激活经 `peer_of` downcast 解析当前代 peer）。
async fn fixture(
    body: String,
    entry: serde_json::Value,
    discovered: Vec<SkillMetadata>,
) -> Fixture {
    let reads = Arc::new(AtomicUsize::new(0));
    let gets = Arc::new(AtomicUsize::new(0));
    let entry = Arc::new(parking_lot::Mutex::new(entry));
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(raw_activation_server(
        server_io,
        body,
        Arc::clone(&entry),
        Arc::clone(&reads),
        Arc::clone(&gets),
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
        channel_capable: false,
    });
    let registry = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = handle.clone();
    registry.mark_discovery_started(SERVER, token.clone());
    registry.mark_discovery_completed(SERVER, token, discovered);
    Fixture {
        registry,
        reads,
        gets,
        entry,
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

    let cached = Arc::new(std::sync::RwLock::new(Some(fx.registry.all_skills())));
    let tool = crate::skills::tools::SkillTool::new(cached, Some(Arc::clone(&fx.registry)));
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
