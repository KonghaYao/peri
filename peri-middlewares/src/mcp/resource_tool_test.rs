//! Tests for resource_tool

use super::*;
use crate::mcp::client::{McpClientHandle, OAuthStatus};
use crate::mcp::ClientStatus;
use peri_acp_types::{
    mcp_skills::{HandleToken, McpSkillRegistry, ServerDiscoveryState},
    skills::{SkillMetadata, SkillOrigin, SkillResource, SkillSource},
};
use rmcp::RoleClient;
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn empty_pool() -> Arc<McpClientPool> {
    Arc::new(McpClientPool::new_empty())
}

fn empty_registry() -> Arc<McpSkillRegistry> {
    Arc::new(McpSkillRegistry::new())
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn test_name_returns_mcp_read_resource() {
    let tool = McpResourceTool::new(empty_pool(), empty_registry());
    assert_eq!(tool.name(), "mcp_read_resource");
}

#[test]
fn test_parameters_schema() {
    let tool = McpResourceTool::new(empty_pool(), empty_registry());
    let params = tool.parameters();
    assert!(params
        .get("properties")
        .unwrap()
        .get("server_name")
        .is_some());
    assert!(params.get("properties").unwrap().get("uri").is_some());
    let required = params.get("required").unwrap().as_array().unwrap();
    assert!(required.iter().any(|r| r.as_str() == Some("server_name")));
    assert!(required.iter().any(|r| r.as_str() == Some("uri")));
}

#[test]
fn test_description_empty_pool() {
    let tool = McpResourceTool::new(empty_pool(), empty_registry());
    let desc = tool.description();
    assert!(desc.contains("No resources currently available"));
}

#[tokio::test]
async fn test_invoke_missing_server_name() {
    let tool = McpResourceTool::new(empty_pool(), empty_registry());
    let result = tool
        .invoke(
            serde_json::json!({"uri": "file:///test"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("server_name"));
}

#[tokio::test]
async fn test_invoke_missing_uri() {
    let tool = McpResourceTool::new(empty_pool(), empty_registry());
    let result = tool
        .invoke(
            serde_json::json!({"server_name": "test"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("uri"));
}

#[tokio::test]
async fn test_invoke_server_not_found() {
    let tool = McpResourceTool::new(empty_pool(), empty_registry());
    let result = tool
        .invoke(
            serde_json::json!({"server_name": "nonexistent", "uri": "test://x"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("未找到"));
}

// ─── skill:// 内容绑定完整性校验（任务：读未列出文件/内容与 digest 不符
//     必须拒绝；无条目覆盖不校验）─────────────────────────────────────────

/// 极简资源服务器：对 `resources/read` 按 uri 返回预置文本（未知 uri →
/// -32602）；其余方法 → -32601。
async fn resource_server(io: tokio::io::DuplexStream, contents: Vec<(&'static str, &'static str)>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            line.clear();
            continue;
        };
        line.clear();
        let writer = Arc::clone(&writer);
        let contents = contents.clone();
        tokio::spawn(async move {
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let uri = parsed["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let response = if let Some((_, text)) = contents.iter().find(|(u, _)| *u == uri) {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "ttlMs": 60_000,
                        "cacheScope": "public",
                        "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
                    }
                })
            } else {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": "unknown resource" }
                })
            };
            let mut w = writer.lock().await;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
        });
    }
}

/// 读取面 server：`resources/read` 按 uri 返回预置内容（Text 或 Blob）；
/// `skills/get` 按 `GetReply` 应答（`skill_digest` None → -32602）。
/// 应答过 skills/get 的 uri 之后 read 返回 `get_new_text`（模拟 server 内容
/// 已热更新）；`get_done` 非 None 时 get 应答后 notify；`read_delay` 非零时
/// get 之后的 read 延迟该时长（handle 竞态测试同步用）。`request_log`
/// 非 None 时按序记录 `"<method> <uri>"`。
#[derive(Clone)]
struct ReadItem {
    uri: &'static str,
    text: Option<&'static str>,
    /// base64 编码的 Blob 内容（text 为 None 时生效）
    blob_b64: Option<&'static str>,
}

/// `skills/get` 应答配置（热更新恢复测试用）。
#[derive(Clone, Default)]
struct GetReply {
    /// SKILL.md digest（None → get 返回 -32602）
    skill_digest: Option<String>,
    /// 额外资源条目 (uri, digest)——Unlisted 恢复测试（新条目列出该 uri）
    extra_resources: Vec<(String, String)>,
    /// 返回错误 uri（模拟 server 违规）
    wrong_uri: bool,
}

#[allow(clippy::too_many_arguments)]
async fn read_server(
    io: tokio::io::DuplexStream,
    contents: Vec<ReadItem>,
    get_reply: GetReply,
    get_new_text: Option<&'static str>,
    get_done: Option<Arc<tokio::sync::Notify>>,
    read_delay: std::time::Duration,
    request_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let get_served: Arc<std::sync::Mutex<std::collections::HashSet<String>>> = Default::default();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            line.clear();
            continue;
        };
        line.clear();
        let writer = Arc::clone(&writer);
        let contents = contents.clone();
        let get_reply = get_reply.clone();
        let get_done = get_done.clone();
        let request_log = request_log.clone();
        let get_served = Arc::clone(&get_served);
        tokio::spawn(async move {
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let method = parsed
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            let uri = parsed["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if let Some(log) = &request_log {
                log.lock().unwrap().push(format!("{method} {uri}"));
            }
            let response = match method.as_str() {
                "resources/read" => {
                    let served = get_served.lock().unwrap().contains(&uri);
                    if served && !read_delay.is_zero() {
                        tokio::time::sleep(read_delay).await;
                    }
                    let item = contents.iter().find(|c| c.uri == uri);
                    match item {
                        Some(item) => {
                            // get 之后返回新内容（模拟热更新）
                            let text = match (served, get_new_text) {
                                (true, Some(new)) => new,
                                _ => item.text.unwrap_or(""),
                            };
                            let contents_json = if let Some(b64) = item.blob_b64 {
                                serde_json::json!([{ "uri": uri, "mimeType": "application/octet-stream", "blob": b64 }])
                            } else {
                                serde_json::json!([{ "uri": uri, "mimeType": "text/markdown", "text": text }])
                            };
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": { "contents": contents_json }
                            })
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown resource" }
                        }),
                    }
                }
                "skills/get" => {
                    get_served.lock().unwrap().insert(uri.clone());
                    let response = match &get_reply.skill_digest {
                        Some(digest) => {
                            // 当前条目快照（uri 可与请求不一致模拟 server 违规）。
                            // frontmatter 从"get 后 read 将返回的文本"推导——
                            // 与 verify_and_build 的全等比对保持一致。
                            let get_uri = if get_reply.wrong_uri {
                                "skill://wrong/SKILL.md"
                            } else {
                                uri.as_str()
                            };
                            let item = contents.iter().find(|c| c.uri == uri);
                            let read_text = get_new_text
                                .or_else(|| item.and_then(|i| i.text))
                                .unwrap_or("");
                            let (name, description) = fm_of(read_text);
                            // resources = SKILL.md 自身 + 额外条目（Unlisted
                            // 恢复场景：新条目可能已列出请求 uri）
                            let mut resources = vec![serde_json::json!({
                                "uri": uri,
                                "digest": digest,
                            })];
                            for (extra_uri, extra_digest) in &get_reply.extra_resources {
                                resources.push(serde_json::json!({
                                    "uri": extra_uri,
                                    "digest": extra_digest,
                                }));
                            }
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "skill": {
                                        "uri": get_uri,
                                        "frontmatter": { "name": name, "description": description },
                                        "resources": resources,
                                    }
                                }
                            })
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown skill" }
                        }),
                    };
                    // get 应答后 notify（同步点：get 已应答、恢复 RPC 进行中——
                    // 测试线程此刻替换 registry，recover 的 handle 快照已取，
                    // 回写必被旧 handle 拒绝；不依赖 tokio 调度顺序）。
                    if let Some(n) = &get_done {
                        n.notify_one();
                    }
                    response
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
            drop(w);
        });
    }
}

/// 极简 frontmatter 提取（测试文本均为 `---\nname: X\ndescription: Y\n---`
/// 形态；server 侧用它与内容保持一致，供 verify_and_build 全等比对）。
fn fm_of(text: &str) -> (String, String) {
    let mut name = String::new();
    let mut description = String::new();
    for line in text.lines().skip(1) {
        if line == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        }
        if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        }
    }
    (name, description)
}

/// 构造已连接 server 的 pool + handle（读路径复用 discovery 测试的
/// serve_directly 客户端面；handle 的 resources 为空——invoke 不依赖它，
/// 完整性校验走 registry）。
fn make_connected_pool(peer: rmcp::Peer<rmcp::RoleClient>) -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients.write().insert(
        "srv".to_string(),
        Arc::new(McpClientHandle {
            name: "srv".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: Some(peer),
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    pool
}

/// 构造带 resources 绑定的 MCP skill 条目（模拟发现任务回写 registry 的
/// 形态：origin + 完整 resources 清单）。
fn mcp_entry(uri: &str, resources: Vec<SkillResource>) -> SkillMetadata {
    SkillMetadata {
        name: "mcp__srv__demo".to_string(),
        aliases: Vec::new(),
        description: "Demo skill".to_string(),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: uri.to_string(),
        }),
        content: None,
        resources,
        frontmatter: None,
    }
}

/// 把条目 seed 进 registry（Started → Completed，模拟发现完成态）。
fn seed_registry(reg: &McpSkillRegistry, entries: Vec<SkillMetadata>) {
    let token: HandleToken = Arc::new(9u32);
    reg.mark_discovery_started("srv", token.clone());
    reg.mark_discovery_completed("srv", token, entries);
}

const DEMO_MD: &str = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo\n";

/// 注入 registry 含条目（digest 正确）→ 读 SKILL.md 成功。
#[tokio::test]
async fn read_skill_md_with_matching_digest_ok() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(resource_server(
        server_io,
        vec![("skill://demo/SKILL.md", DEMO_MD)],
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", sha256_hex(DEMO_MD)),
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let result = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let out = result.expect("digest 一致应读取成功");
    assert!(out.contains("# Demo"), "内容应返回，实际: {out:?}");
}

/// digest 不匹配 → Err（验证失败）。
#[tokio::test]
async fn read_skill_md_digest_mismatch_rejected() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(resource_server(
        server_io,
        vec![("skill://demo/SKILL.md", DEMO_MD)],
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let mut pool = make_connected_pool(running.peer().clone());
    let cache_dir = tempfile::tempdir().unwrap();
    Arc::get_mut(&mut pool)
        .expect("测试中 pool 尚无其他 Arc")
        .resource_cache =
        crate::mcp::resource_cache::McpResourceCache::at(cache_dir.path().to_path_buf());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", "0".repeat(64)),
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("digest"),
        "错误应说明 digest 不一致，实际: {err}"
    );
    let origin = crate::mcp::resource_cache::cache_origin("srv", None);
    let cached: Option<rmcp::model::ReadResourceResult> = pool
        .resource_cache
        .get(&origin, "resources/read", "skill://demo/SKILL.md")
        .await;
    assert!(cached.is_none(), "digest 校验失败的 Skill 响应不得落盘");
}

/// uri 在 skill 根前缀内但未列入 resources → Err（server 有该文件也拒绝——
/// 校验在读前发生）。
#[tokio::test]
async fn read_unlisted_skill_file_rejected() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    // server 侧提供 notes.md（内容存在），但条目 resources 未列出它
    tokio::spawn(resource_server(
        server_io,
        vec![
            ("skill://demo/SKILL.md", DEMO_MD),
            ("skill://demo/notes.md", "# Notes\n"),
        ],
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", sha256_hex(DEMO_MD)),
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/notes.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("未列入"),
        "错误应说明未列入 resources，实际: {err}"
    );
}

/// 空 registry（无任何条目覆盖）→ 行为同现状：不校验，普通读。
#[tokio::test]
async fn read_with_empty_registry_no_verification() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(resource_server(
        server_io,
        vec![
            ("skill://demo/SKILL.md", DEMO_MD),
            ("file:///tmp/x.md", "# Local\n"),
        ],
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    // 空注册表：无条目 → skill:// 与普通 uri 均不校验
    let tool = McpResourceTool::new(pool, empty_registry());
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("空 registry 不校验，应读取成功");
    assert!(out.contains("# Demo"));
    let out2 = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "file:///tmp/x.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("非 skill:// 前缀不校验");
    assert!(out2.contains("# Local"));
}

/// 条目 resources 为空（动态生成技能）→ 无法内容绑定，不校验（规范 MAY
/// 省略 resources）。
#[tokio::test]
async fn read_skill_md_unbound_entry_no_verification() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(resource_server(
        server_io,
        vec![("skill://demo/SKILL.md", DEMO_MD)],
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(&reg, vec![mcp_entry("skill://demo/SKILL.md", vec![])]);
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("resources 为空（动态技能）→ 不校验");
    assert!(out.contains("# Demo"));
}

// ─── locate_skill_binding 纯函数单测（最长根优先 / 前缀边界 / 判定序）─────

/// 多条目覆盖时取 root 最长的（更具体）；前缀边界：skill://a 不覆盖
/// skill://ab/...；uri == root 也算覆盖。
#[test]
fn locate_skill_binding_picks_longest_root_and_respects_prefix_boundary() {
    let shallow = mcp_entry(
        "skill://a/SKILL.md",
        vec![SkillResource {
            uri: "skill://a/x.md".to_string(),
            digest: "sha256:dd".to_string(),
        }],
    );
    let deep = mcp_entry(
        "skill://a/b/SKILL.md",
        vec![SkillResource {
            uri: "skill://a/b/x.md".to_string(),
            digest: "sha256:ee".to_string(),
        }],
    );
    let entries = vec![shallow.clone(), deep.clone()];
    // 嵌套技能场景：根 skill://a 与 skill://a/b 都覆盖 skill://a/b/x.md
    // → 取最长根（skill://a/b）的条目
    let (entry, binding) =
        locate_skill_binding(&entries, "skill://a/b/x.md").expect("嵌套覆盖应命中");
    assert_eq!(
        entry.origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://a/b/SKILL.md".to_string(),
        }),
        "多条目覆盖取 root 最长的（更具体）"
    );
    assert!(matches!(binding, SkillBinding::Listed("sha256:ee")));
    // 前缀边界：skill://a 条目不覆盖 skill://ab/...（根以 "/" 为界）
    assert!(
        locate_skill_binding(std::slice::from_ref(&shallow), "skill://ab/x.md").is_none(),
        "前缀边界：skill://a 不覆盖 skill://ab/x.md"
    );
    // uri == root：读 SKILL.md 自身
    let (entry, _) = locate_skill_binding(&entries, "skill://a/b/SKILL.md").expect("根相等应命中");
    assert_eq!(
        entry.origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://a/b/SKILL.md".to_string(),
        })
    );
    // SKILL.md 自身未列入 resources（条目 resources 只列附属资源）→ Unlisted
    let shallow_only = [shallow];
    let listed = locate_skill_binding(&shallow_only, "skill://a/SKILL.md").unwrap();
    assert!(
        matches!(listed.1, SkillBinding::Unlisted),
        "覆盖但 SKILL.md 未列出 → Unlisted"
    );
}

/// 判定顺序：无覆盖 → None；resources 空 → Unbound；未列出 → Unlisted；
/// 非 skill:// scheme（大小写变体）→ None。
#[test]
fn locate_skill_binding_decision_order_and_scheme_case() {
    // 无任何条目覆盖 → None
    assert!(
        locate_skill_binding(&[], "skill://demo/notes.md").is_none(),
        "无条目 → None"
    );
    // 非 skill:// 前缀（含大小写变体）→ None
    let entry = mcp_entry(
        "skill://demo/SKILL.md",
        vec![SkillResource {
            uri: "skill://demo/SKILL.md".to_string(),
            digest: "sha256:aa".to_string(),
        }],
    );
    assert!(locate_skill_binding(std::slice::from_ref(&entry), "file:///tmp/x.md").is_none());
    assert!(locate_skill_binding(std::slice::from_ref(&entry), "HTTP://demo/SKILL.md").is_none());
    // scheme 大小写不敏感：SKILL:// 命中同一条目
    let entry_only = [entry.clone()];
    let (found, binding) =
        locate_skill_binding(&entry_only, "SKILL://demo/SKILL.md").expect("大小写 scheme 命中");
    assert!(matches!(binding, SkillBinding::Listed("sha256:aa")));
    assert_eq!(
        found.origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://demo/SKILL.md".to_string(),
        })
    );
    // resources 空（动态技能）→ Unbound（不校验）
    let unbound = mcp_entry("skill://demo/SKILL.md", vec![]);
    let unbound_only = [unbound];
    let (_, binding) = locate_skill_binding(&unbound_only, "skill://demo/notes.md").unwrap();
    assert!(matches!(binding, SkillBinding::Unbound));
    // 覆盖但未列入 → Unlisted
    let (_, binding) = locate_skill_binding(&entry_only, "skill://demo/notes.md").unwrap();
    assert!(matches!(binding, SkillBinding::Unlisted));
}

// ─── Blob 内容 digest 校验（base64 解码后 sha256 bytes）───────────────────

/// Blob 内容（base64 编码）digest 匹配 → 读取成功。
#[tokio::test]
async fn read_skill_blob_with_matching_digest_ok() {
    let blob_bytes = b"binary payload bytes";
    let blob_b64 = base64::engine::general_purpose::STANDARD.encode(blob_bytes);
    let digest = sha256_bytes_hex(blob_bytes);
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://demo/SKILL.md",
            text: None,
            blob_b64: Some(Box::leak(blob_b64.into_boxed_str())),
        }],
        GetReply::default(),
        None,
        None,
        std::time::Duration::ZERO,
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{digest}"),
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("Blob digest 匹配应读取成功");
    assert!(
        out.contains("bytes of binary data"),
        "Blob 输出为占位描述，实际: {out:?}"
    );
}

/// Blob 内容 digest 不匹配 → 验证失败（恢复失败路径：get 不可用）。
#[tokio::test]
async fn read_skill_blob_digest_mismatch_rejected() {
    let blob_bytes = b"binary payload bytes";
    let blob_b64 = base64::engine::general_purpose::STANDARD.encode(blob_bytes);
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://demo/SKILL.md",
            text: None,
            blob_b64: Some(Box::leak(blob_b64.into_boxed_str())),
        }],
        GetReply::default(),
        None,
        None,
        std::time::Duration::ZERO,
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", "0".repeat(64)),
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("digest"),
        "Blob digest 不一致应拒绝，实际: {err}"
    );
}

/// 非法 base64 Blob → 无法校验，视为不匹配（MUST NOT use）。
#[test]
fn verify_blob_digest_invalid_base64_fails() {
    assert!(!verify_blob_digest("not-valid-base64!!!", "sha256:abc"));
}

// ─── 多 contents 响应：逐项校验（任一不匹配即拒绝）────────────────────────

/// 多 contents 全部匹配 → 放行。
#[tokio::test]
async fn read_skill_multi_contents_all_match_ok() {
    let text = "# Multi\n";
    let digest = format!("sha256:{}", sha256_hex(text));
    // 定制 server：两个相同文本的 contents（digest 相同，均匹配）
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(multi_content_server(server_io, text, text));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest,
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("多 contents 全部匹配 digest → 读取成功");
    assert!(out.contains("# Multi"), "内容应返回，实际: {out:?}");
}

/// 多 contents 中任一不匹配 → 验证失败（顺带修 LOW-2：不再只校验首个——
/// 首个匹配、第二个不匹配 → 拒绝）。
#[tokio::test]
async fn read_skill_multi_contents_any_mismatch_rejected() {
    let first = "# Multi\n";
    // digest 只匹配首个 content；第二个 "# Tampered\n" 不匹配
    let digest = format!("sha256:{}", sha256_hex(first));
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(multi_content_server(server_io, first, "# Tampered\n"));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let pool = make_connected_pool(running.peer().clone());
    let reg = empty_registry();
    seed_registry(
        &reg,
        vec![mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest,
            }],
        )],
    );
    let tool = McpResourceTool::new(Arc::clone(&pool), reg);
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("digest"),
        "contents 任一 digest 不匹配应拒绝，实际: {err}"
    );
}

/// 多 contents server：固定返回两个 Text content（内容由调用方给定——
/// 相同 = 全匹配场景；不同 = 任一不匹配场景）。
async fn multi_content_server(
    io: tokio::io::DuplexStream,
    first: &'static str,
    second: &'static str,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            line.clear();
            continue;
        };
        line.clear();
        let writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let uri = parsed["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "contents": [
                        { "uri": uri, "mimeType": "text/markdown", "text": first },
                        { "uri": uri, "mimeType": "text/plain", "text": second },
                    ]
                }
            });
            let mut w = writer.lock().await;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
        });
    }
}

fn sha256_bytes_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[path = "resource_tool_recovery_test.rs"]
mod recovery_tests;
