//! 发现纯函数测试：select_skill_resources 过滤、parse_mcp_skill_md 解析
//! （frontmatter 校验、uri 段提取、非法字符替换、origin/content 字段）。

use super::*;
use crate::mcp::client::{McpClientHandle, OAuthStatus};
use crate::mcp::ClientStatus;
use peri_acp_types::mcp_skills::ServerDiscoveryState;
use std::sync::Arc;

fn resource(uri: &str) -> Resource {
    Resource::new(uri.to_string(), "desc".to_string())
}

// ─── select_skill_resources ────────────────────────────────────────────────

#[test]
fn select_skill_resources_filters_to_skill_md() {
    let resources = vec![
        resource("skill://demo/SKILL.md"),
        resource("skill://other/sub/SKILL.md"),
        // 附属资源：同前缀非 SKILL.md → 过滤
        resource("skill://demo/notes/README.md"),
        resource("skill://demo/scripts/run.sh"),
        // 非 skill:// 前缀 → 过滤
        resource("https://example.com/skill://demo/SKILL.md"),
        resource("file:///skill.md"),
        // 非 /SKILL.md 后缀 → 过滤
        resource("skill://demo/SKILL.md.bak"),
        resource("skill://demo/skill.md"),
    ];
    let selected = select_skill_resources(&resources);
    let uris: Vec<&str> = selected.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        vec!["skill://demo/SKILL.md", "skill://other/sub/SKILL.md"],
        "仅 skill:// 前缀 + /SKILL.md 后缀入选"
    );
}

#[test]
fn select_skill_resources_empty() {
    assert!(select_skill_resources(&[]).is_empty());
    assert!(select_skill_resources(&[resource("https://x/SKILL.md")]).is_empty());
}

// ─── parse_mcp_skill_md ────────────────────────────────────────────────────

const SAMPLE_MD: &str = "---\nname: demo-skill\ndescription: Say hello\n---\n\n# Hello\n";

#[test]
fn parse_ok_builds_metadata() {
    let meta = parse_mcp_skill_md(SAMPLE_MD, "demo", "skill://demo/SKILL.md").expect("应解析成功");
    assert_eq!(
        meta.name,
        mcp_skill_name("demo", "demo"),
        "注册名来自 uri 段"
    );
    assert_eq!(meta.description, "Say hello");
    assert_eq!(meta.path, PathBuf::new());
    assert_eq!(meta.source, SkillSource::Mcp);
    assert_eq!(meta.plugin_name, None);
    assert_eq!(
        meta.origin,
        Some(SkillOrigin::Mcp {
            server: "demo".to_string(),
            uri: "skill://demo/SKILL.md".to_string(),
        })
    );
    assert_eq!(meta.content.as_deref(), Some(SAMPLE_MD), "content 存全文");
}

#[test]
fn parse_missing_name_returns_none() {
    let content = "---\ndescription: no name here\n---\n\n# Body\n";
    assert!(
        parse_mcp_skill_md(content, "srv", "skill://srv/SKILL.md").is_none(),
        "缺 name → None"
    );
}

#[test]
fn parse_missing_description_returns_none() {
    let content = "---\nname: orphan\n---\n\n# Body\n";
    assert!(
        parse_mcp_skill_md(content, "srv", "skill://srv/SKILL.md").is_none(),
        "缺 description → None"
    );
}

#[test]
fn parse_invalid_yaml_returns_none() {
    assert!(parse_mcp_skill_md("not: [valid\nyaml", "srv", "skill://srv/SKILL.md").is_none());
    assert!(
        parse_mcp_skill_md("# 无 frontmatter\n\n正文", "srv", "skill://srv/SKILL.md").is_none()
    );
}

#[test]
fn parse_uri_segment_extraction_and_sanitization() {
    // 嵌套段：'/' 替换为 '_'
    let meta = parse_mcp_skill_md(SAMPLE_MD, "srv", "skill://ns/sub/SKILL.md").unwrap();
    assert_eq!(meta.name, "mcp__srv__ns_sub");

    // 非法字符（空格/点/中文）替换为 '_'；- 与 _ 保留
    let meta2 = parse_mcp_skill_md(SAMPLE_MD, "srv", "skill://my skill/v1.0-β/SKILL.md").unwrap();
    assert_eq!(meta2.name, "mcp__srv__my_skill_v1_0-_");
}

#[test]
fn parse_uri_without_skill_prefix_or_suffix_returns_none() {
    assert!(
        parse_mcp_skill_md(SAMPLE_MD, "srv", "https://x/SKILL.md").is_none(),
        "非 skill:// 前缀 → None"
    );
    assert!(
        parse_mcp_skill_md(SAMPLE_MD, "srv", "skill://srv/other.md").is_none(),
        "非 /SKILL.md 后缀 → None"
    );
}

#[test]
fn parse_description_trimmed() {
    // YAML 折叠标量尾部可能带 \n，与 loader.rs 一致做 trim
    let content = "---\nname: d\ndescription: >\n  hello\n  world\n---\n\n# Body\n";
    let meta = parse_mcp_skill_md(content, "srv", "skill://d/SKILL.md").unwrap();
    assert_eq!(meta.description, "hello world");
}

// ─── collect_skill_entries 排序（组 D：每请求独立 spawn 服务器）────────────

/// 单请求响应规则：uri 命中 `segment` 子串时应用；`delay` 为响应前延迟；
/// `error` 为 true 时返回 JSON-RPC error（模拟 read 失败）。
#[derive(Clone)]
struct RespondRule {
    segment: &'static str,
    delay: std::time::Duration,
    error: bool,
}

/// 原始 JSON-RPC responder（仅用 client feature，不引入 rmcp server 面）：
/// 逐行读请求，**每请求独立 spawn** 响应任务（并发写经 Mutex<WriteHalf>
/// 串行化，消息边界安全）；`first_done` 非 None 时在首个响应写出后 notify
/// （cancel 时序同步用）；`completion_log` 非 None 时按写出顺序记录 uri 段
/// （锁定完成序，日志先于响应写出保证可见性）。
async fn raw_skill_server(
    io: tokio::io::DuplexStream,
    rules: Vec<RespondRule>,
    first_done: Option<Arc<tokio::sync::Notify>>,
    completion_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
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
        let first_done = first_done.clone();
        let completion_log = completion_log.clone();
        let rules = rules.clone();
        tokio::spawn(async move {
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let uri = parsed["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let rule = rules.iter().find(|r| uri.contains(r.segment)).cloned();
            if let Some(r) = &rule {
                if !r.delay.is_zero() {
                    tokio::time::sleep(r.delay).await;
                }
            }
            let response = if rule.as_ref().map(|r| r.error).unwrap_or(false) {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32000, "message": "read failed" }
                })
            } else {
                let segment = uri
                    .strip_prefix("skill://")
                    .and_then(|u| u.strip_suffix("/SKILL.md"))
                    .unwrap_or("unknown");
                let text = format!(
                    "---\nname: {segment}\ndescription: desc for {segment}\n---\n\n# Body\n"
                );
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "contents": [{ "uri": uri, "mimeType": "text/plain", "text": text }]
                    }
                })
            };
            if let Some(log) = &completion_log {
                log.lock().unwrap().push(uri.clone());
            }
            let mut w = writer.lock().await;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
            drop(w);
            if let Some(n) = &first_done {
                n.notify_one();
            }
        });
    }
}

#[tokio::test]
async fn collect_skill_entries_sorts_by_name_despite_completion_order() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    // 服务端每请求独立 spawn：zebra 立即返回、alpha 延迟 200ms → 完成序
    // 确定性为 [zebra, alpha]（与 name 排序相反）；若 collect 不排序，
    // entries 将保持完成序。
    let completion_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_millis(200),
            error: false,
        }],
        None,
        Some(Arc::clone(&completion_log)),
    ));

    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer().clone();

    let resources = vec![
        resource("skill://srv/zebra/SKILL.md"),
        resource("skill://srv/alpha/SKILL.md"),
    ];
    let cancel = AgentCancellationToken::new();
    let entries = collect_skill_entries(peer, "srv", resources, cancel).await;

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    // uri 段 = "srv/alpha" → 注册名 mcp__srv__srv_alpha（sanitize 后）
    assert_eq!(
        names,
        vec!["mcp__srv__srv_alpha", "mcp__srv__srv_zebra"],
        "JoinSet 完成序非确定，输出应按 name 排序"
    );
    // 完成序确定性：zebra（无延迟）先于 alpha（200ms 延迟）写出——与排序序
    // 相反，证明排序断言确实覆盖了乱序输入。
    assert_eq!(
        *completion_log.lock().unwrap(),
        vec![
            "skill://srv/zebra/SKILL.md".to_string(),
            "skill://srv/alpha/SKILL.md".to_string()
        ],
        "服务端完成序应为 [zebra, alpha]，与排序序相反"
    );
}

// ─── run_discovery 级测试（组 D）────────────────────────────────────────────

/// 极简 tracing Subscriber：只捕获 WARN 事件的 message 字段（不引入
/// tracing-subscriber dev-dependency；tracing 根导出全套 Subscriber/Visit）。
struct WarnCaptureSubscriber {
    warns: Arc<std::sync::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for WarnCaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(0)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct MessageVisitor(String);
        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.warns.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// 构造 discovery 用 McpClientHandle（peer 已连接 duplex 客户端面）。
fn make_discovery_handle(
    running: &rmcp::service::RunningService<RoleClient, ()>,
    resources: Vec<Resource>,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: "srv".to_string(),
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        channel_capable: false,
    })
}

/// candidates 非空 + 全部 read 失败 → 汇总 warn（回写空条目 Discovered）。
#[test]
fn run_discovery_all_reads_fail_emits_warn() {
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: Arc::clone(&warns),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let reg = Arc::new(McpSkillRegistry::new());
                let token: HandleToken = Arc::new(3u32);
                let (client_io, server_io) = tokio::io::duplex(8192);
                // 空段规则匹配所有 uri → 全部返回 JSON-RPC error（read 失败）
                tokio::spawn(raw_skill_server(
                    server_io,
                    vec![RespondRule {
                        segment: "",
                        delay: std::time::Duration::ZERO,
                        error: true,
                    }],
                    None,
                    None,
                ));
                let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
                    (),
                    client_io,
                    None::<rmcp::model::ServerPeerInfo>,
                );
                let handle = make_discovery_handle(
                    &running,
                    vec![
                        resource("skill://srv/a/SKILL.md"),
                        resource("skill://srv/b/SKILL.md"),
                    ],
                );
                reg.mark_discovery_started("srv", token.clone());
                let cancel = AgentCancellationToken::new();
                run_discovery(reg.clone(), handle, token.clone(), cancel).await;
                assert!(
                    matches!(
                        reg.discovery_state("srv"),
                        Some(ServerDiscoveryState::Discovered { entries, .. })
                            if entries.is_empty()
                    ),
                    "全部 read 失败应回写空条目 Discovered"
                );
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        warns.iter().any(|m| m.contains("全部读取失败")),
        "candidates 非空且全部 read 失败应发汇总 warn，实际: {warns:?}"
    );
}

/// cancel 提前退出 → clear_discovery_started 回退：首条响应后触发 cancel，
/// run_discovery 结束断言 discovery_state 为 None（Started 已清除）。
#[tokio::test]
async fn run_discovery_cancel_after_first_response_clears_started() {
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(4u32);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let first_done = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_secs(2),
            error: false,
        }],
        Some(Arc::clone(&first_done)),
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_discovery_handle(
        &running,
        vec![
            resource("skill://srv/zebra/SKILL.md"),
            resource("skill://srv/alpha/SKILL.md"),
        ],
    );
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    let discovery = tokio::spawn(run_discovery(
        reg.clone(),
        handle,
        token.clone(),
        cancel.clone(),
    ));

    // zebra 立即响应、alpha 延迟 2s → 首条响应后触发 cancel
    first_done.notified().await;
    cancel.cancel();
    discovery.await.unwrap();

    assert!(
        reg.discovery_state("srv").is_none(),
        "cancel 后应 clear_discovery_started 回退（discovery_state None）"
    );
}

/// cancel 提前退出不得误报汇总 warn：首条响应为 read 失败（entries 保持空），
/// 随后 cancel → 走 clear 路径，不经过 entries.is_empty() 的 warn 分支。
#[test]
fn run_discovery_cancel_before_warn_does_not_emit_warn() {
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: Arc::clone(&warns),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let reg = Arc::new(McpSkillRegistry::new());
                let token: HandleToken = Arc::new(5u32);
                let (client_io, server_io) = tokio::io::duplex(8192);
                let first_done = Arc::new(tokio::sync::Notify::new());
                tokio::spawn(raw_skill_server(
                    server_io,
                    vec![
                        // zebra：立即返回 error（entries 保持空）
                        RespondRule {
                            segment: "zebra",
                            delay: std::time::Duration::ZERO,
                            error: true,
                        },
                        // alpha：延迟 2s（cancel 在其完成前触发）
                        RespondRule {
                            segment: "alpha",
                            delay: std::time::Duration::from_secs(2),
                            error: false,
                        },
                    ],
                    Some(Arc::clone(&first_done)),
                    None,
                ));
                let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
                    (),
                    client_io,
                    None::<rmcp::model::ServerPeerInfo>,
                );
                let handle = make_discovery_handle(
                    &running,
                    vec![
                        resource("skill://srv/zebra/SKILL.md"),
                        resource("skill://srv/alpha/SKILL.md"),
                    ],
                );
                reg.mark_discovery_started("srv", token.clone());
                let cancel = AgentCancellationToken::new();
                let discovery = tokio::spawn(run_discovery(
                    reg.clone(),
                    handle,
                    token.clone(),
                    cancel.clone(),
                ));
                first_done.notified().await;
                cancel.cancel();
                discovery.await.unwrap();
                assert!(
                    reg.discovery_state("srv").is_none(),
                    "cancel 后应 clear_discovery_started 回退"
                );
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        !warns.iter().any(|m| m.contains("全部读取失败")),
        "cancel 提前退出不得误报汇总 warn，实际: {warns:?}"
    );
}
