// ─── collect_skill_entries 排序（legacy：每请求独立 spawn 服务器）─────────

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
///
/// read 响应的 frontmatter name = uri 最后一段（与资源过滤语义对齐：
/// 最终段即技能名）。
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
                    .and_then(|u| u.rsplit('/').next())
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

async fn private_zero_ttl_skill_server(
    io: tokio::io::DuplexStream,
    request_count: Arc<std::sync::atomic::AtomicUsize>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let parsed = serde_json::from_str::<serde_json::Value>(line.trim_end()).ok();
        line.clear();
        let Some(parsed) = parsed else { continue };
        let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let uri = parsed["params"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text = "---\nname: cached\ndescription: Cached skill\n---\n\n# Cached\n";
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }],
                "cacheScope": "private",
                "ttlMs": 0,
            }
        });
        let mut writer = writer.lock().await;
        writer
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    }
}

// ─── run_discovery 级测试（legacy 路径）───────────────────────────────────

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
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn Debug) {
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

/// 构造 discovery 用 McpClientHandle（peer 已连接 duplex 客户端面；
/// legacy：skills_capable=false）。
fn make_discovery_handle(
    running: &rmcp::service::RunningService<RoleClient, ()>,
    resources: Vec<Resource>,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: "srv".to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
}

// ─── run_discovery 级测试（SEP-2640 规范路径）─────────────────────────────

/// 规范模式测试 server：`skills/list` 返回固定条目（digest 由文本计算，
/// 可被 `digest_override` 篡改）；`resources/read` 返回条目文本；
/// `skills/get` 返回当前条目快照（可配置与 list 不同的内容模拟 stale 后
/// 更新，或返回 -32602 模拟 get 失败）。`request_log` 非 None 时按请求
/// 顺序记录 `"<method> <uri>"`。
#[derive(Clone)]
struct SpecSkill {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
    /// list/read 用的内容（旧）
    text: &'static str,
    /// 非 None 时覆盖 skills/list 中的 digest（制造校验失败场景）
    digest_override: Option<String>,
    /// skills/get 返回的内容（None → 与 text 相同）
    get_text: Option<&'static str>,
    /// skills/get 对该 uri 返回 -32602（模拟 get 失败）
    get_error: bool,
    /// skills/get 返回错误 uri（与请求不一致，模拟 server 违规）
    get_wrong_uri: bool,
}

async fn spec_skill_server(
    io: tokio::io::DuplexStream,
    skills: Vec<SpecSkill>,
    first_done: Option<Arc<tokio::sync::Notify>>,
    request_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    cacheable: bool,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, writer) = tokio::io::split(io);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    // 已应答过 skills/get 的 uri 集合：模拟"stale → 更新"——get 后
    // resources/read 返回新内容（get_text），get 前返回旧内容（text）。
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
        let first_done = first_done.clone();
        let skills = skills.clone();
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
                "skills/list" => {
                    let entries: Vec<serde_json::Value> = skills
                        .iter()
                        .map(|s| {
                            let digest = match &s.digest_override {
                                Some(d) => d.clone(),
                                None => format!("sha256:{}", sha256_hex(s.text)),
                            };
                            serde_json::json!({
                                "uri": s.uri,
                                "frontmatter": { "name": s.name, "description": s.description },
                                "resources": [{ "uri": s.uri, "digest": digest }],
                            })
                        })
                        .collect();
                    let mut result = serde_json::json!({ "skills": entries });
                    if cacheable {
                        result["cacheScope"] = serde_json::json!("public");
                        result["ttlMs"] = serde_json::json!(60_000);
                    }
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
                }
                "resources/read" => {
                    match skills.iter().find(|s| s.uri == uri) {
                        Some(s) => {
                            // stale→更新：get 已应答过的技能返回新内容
                            let served = get_served.lock().unwrap().contains(&uri);
                            let text = if served {
                                s.get_text.unwrap_or(s.text)
                            } else {
                                s.text
                            };
                            let mut result = serde_json::json!({
                                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
                            });
                            if cacheable {
                                result["cacheScope"] = serde_json::json!("public");
                                result["ttlMs"] = serde_json::json!(60_000);
                            }
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": result
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
                    // 当前条目快照（与 skills/list 条目同构）：digest 优先
                    // 沿用 list 的 digest（含 override——stale 快照一致性），
                    // 其次按 get 内容计算；get_error → -32602。应答后该技能
                    // 进入"已更新"态（后续读返回 get_text）。
                    match skills.iter().find(|s| s.uri == uri) {
                        Some(s) if s.get_error => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown skill" }
                        }),
                        Some(s) => {
                            get_served.lock().unwrap().insert(uri.clone());
                            let get_text = s.get_text.unwrap_or(s.text);
                            let digest = s
                                .digest_override
                                .clone()
                                .unwrap_or_else(|| format!("sha256:{}", sha256_hex(get_text)));
                            // uri 核对违规：返回与请求不一致的 uri（模拟
                            // server 违规，host 应拒绝恢复）。
                            let get_uri = if s.get_wrong_uri {
                                "skill://wrong/SKILL.md"
                            } else {
                                s.uri
                            };
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "skill": {
                                        "uri": get_uri,
                                        "frontmatter": { "name": s.name, "description": s.description },
                                        "resources": [{ "uri": s.uri, "digest": digest }],
                                    }
                                }
                            })
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "unknown skill" }
                        }),
                    }
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
            if let Some(n) = &first_done {
                n.notify_one();
            }
        });
    }
}

/// 规范模式 handle：skills_capable=true，resources 为空也能发现
/// （skills/list 是独立原语，不依赖 resources 扫描）。
fn make_spec_handle(
    running: &rmcp::service::RunningService<RoleClient, ()>,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: "srv".to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: true,
    })
}
