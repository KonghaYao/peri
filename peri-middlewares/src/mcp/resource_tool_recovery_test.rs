use super::*;

// ─── 读取面热更新闭环（digest 失败/未列出 → skills/get 刷新回写）──────────

/// 端到端恢复成功：server 内容已更新（读返回新内容），registry 条目仍为
/// 旧 digest（stale）→ 读后 digest 不匹配 → 自动 skills/get 刷新 → 按新
/// 条目重读并校验 → 返回新内容 + registry 条目更新（digest/内容为新）。
#[tokio::test]
async fn read_skill_digest_mismatch_recovers_via_skills_get() {
    let old_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v1\n";
    let new_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v2\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://demo/SKILL.md",
            text: Some(new_text),
            blob_b64: None,
        }],
        // get 返回当前条目快照：新 digest（匹配 new_text）
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(new_text))),
            ..GetReply::default()
        },
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
                // 旧 digest：只匹配 old_text（stale——server 内容已更新）
                digest: format!("sha256:{}", sha256_hex(old_text)),
            }],
        )],
    );
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("digest 失败后 skills/get 恢复成功应返回新内容");
    assert!(out.contains("# Demo v2"), "应返回新内容，实际: {out:?}");
    // registry 条目已回写（digest/内容为新）
    let skills = reg.skills_of("srv");
    assert_eq!(skills.len(), 1);
    assert!(
        skills[0].content.is_none(),
        "W2：条目只发布 metadata，不落正文"
    );
    let new_digest = format!("sha256:{}", sha256_hex(new_text));
    assert_eq!(
        skills[0].resources[0].digest, new_digest,
        "registry 条目 digest 应为新条目声明"
    );
}

/// 恢复失败：skills/get 不可用（-32602）→ 保持 VerificationFailed。
#[tokio::test]
async fn read_skill_digest_mismatch_get_failed_rejected() {
    let old_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v1\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://demo/SKILL.md",
            text: Some(old_text),
            blob_b64: None,
        }],
        GetReply::default(), // skill_digest None → get -32602
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
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("digest"),
        "get 失败 → 保持 VerificationFailed，实际: {err}"
    );
    // registry 未被回写
    assert_eq!(
        reg.skills_of("srv")[0].resources[0].digest,
        format!("sha256:{}", "0".repeat(64)),
        "恢复失败不得回写条目"
    );
}

/// handle 不一致时不回写：恢复 RPC 期间 registry 被重连（新 handle）覆盖
/// → refresh_entries 的 Arc::ptr_eq 拒绝回写（内容已全量校验，仍返回）。
#[tokio::test]
async fn read_skill_recovery_handle_mismatch_no_writeback() {
    let old_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v1\n";
    let new_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v2\n";
    let get_done = Arc::new(tokio::sync::Notify::new());
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://demo/SKILL.md",
            text: Some(new_text),
            blob_b64: None,
        }],
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(new_text))),
            ..GetReply::default()
        },
        None,
        Some(Arc::clone(&get_done)),
        // get 应答后延迟，给测试线程替换 registry 的时间
        std::time::Duration::from_millis(300),
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
                digest: format!("sha256:{}", sha256_hex(old_text)),
            }],
        )],
    );
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let invoke = tokio::spawn(async move {
        tool.invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
    });
    // get 应答后（恢复 RPC 进行中）：模拟重连重扫——新 handle 覆盖为
    // Discovered（带不同的重连条目）。
    get_done.notified().await;
    let reconnect_entry = {
        let mut e = mcp_entry(
            "skill://demo/SKILL.md",
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", sha256_hex("reconnected content")),
            }],
        );
        e.content = Some("reconnected content".to_string());
        e
    };
    let new_token: HandleToken = Arc::new(99u32);
    reg.mark_discovery_started("srv", new_token.clone());
    reg.mark_discovery_completed("srv", new_token.clone(), vec![reconnect_entry]);

    let result = invoke.await.unwrap();
    let out = result.expect("内容已全量校验，仍应返回新内容");
    assert!(
        out.contains("# Demo v2"),
        "应返回恢复后的新内容，实际: {out:?}"
    );
    // registry 条目未被旧 handle 回写：仍是重连条目
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Discovered { handle, entries }) => {
            assert!(
                Arc::ptr_eq(&handle, &new_token),
                "handle 应保持重连后的新 token"
            );
            assert_eq!(
                entries[0].content.as_deref(),
                Some("reconnected content"),
                "旧 handle 恢复不得覆盖重连条目"
            );
        }
        other => panic!("应为 Discovered，实际: {other:?}"),
    }
}

/// Unlisted 恢复成功：请求 uri 未列入旧条目 resources（读前拒绝）→
/// skills/get 新条目已列出该 uri → 按新条目读内容校验 → 返回 + registry
/// 条目回写（resources 含该 uri）。
#[tokio::test]
async fn read_unlisted_recovers_when_new_entry_lists_uri() {
    let skill_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo\n";
    let notes_text = "# Notes v2\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![
            ReadItem {
                uri: "skill://demo/SKILL.md",
                text: Some(skill_text),
                blob_b64: None,
            },
            ReadItem {
                uri: "skill://demo/notes.md",
                text: Some(notes_text),
                blob_b64: None,
            },
        ],
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(skill_text))),
            // 新条目把 notes.md 也列入了 resources（热更新后新增文件）
            extra_resources: vec![(
                "skill://demo/notes.md".to_string(),
                format!("sha256:{}", sha256_hex(notes_text)),
            )],
            ..GetReply::default()
        },
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
            // 旧条目 resources 未列出 notes.md → 读它时 Unlisted
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", sha256_hex(skill_text)),
            }],
        )],
    );
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/notes.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("Unlisted 经 skills/get 恢复成功应返回新内容");
    assert!(
        out.contains("# Notes v2"),
        "应返回 notes 内容，实际: {out:?}"
    );
    // registry 条目已回写：resources 含 notes.md
    let skills = reg.skills_of("srv");
    assert_eq!(skills.len(), 1);
    assert!(
        skills[0]
            .resources
            .iter()
            .any(|r| r.uri == "skill://demo/notes.md"),
        "回写后条目 resources 应含 notes.md"
    );
}

/// 恢复路径负向（读取面）：skills/get 返回的条目 uri 与请求不一致
/// （GetReply.wrong_uri=true，server 违规）→ 拒绝恢复 → VerificationFailed，
/// registry 不回写。
#[tokio::test]
async fn read_skill_recovery_wrong_uri_rejected() {
    let old_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v1\n";
    let new_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo v2\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://demo/SKILL.md",
            text: Some(new_text),
            blob_b64: None,
        }],
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(new_text))),
            wrong_uri: true,
            ..GetReply::default()
        },
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
                // 旧 digest：只匹配 old_text（stale——触发恢复）
                digest: format!("sha256:{}", sha256_hex(old_text)),
            }],
        )],
    );
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("digest"),
        "get uri 核对违规 → 恢复拒绝，保持 VerificationFailed，实际: {err}"
    );
    // registry 未被回写（仍为旧条目）
    assert_eq!(
        reg.skills_of("srv")[0].resources[0].digest,
        format!("sha256:{}", sha256_hex(old_text)),
        "恢复被拒不得回写条目"
    );
}

/// 恢复路径负向：新条目列出了请求 uri 但 digest 与重读内容不匹配 →
/// 拒绝恢复（VerificationFailed），registry 不回写。invoke 层错误文案对
/// Unlisted 场景固定为"未列入"（不区分恢复失败的具体原因）；重读已发生
/// （request_log 含 notes.md）证明失败点确在 digest 校验分支。
#[tokio::test]
async fn read_unlisted_recovery_digest_mismatch_rejected() {
    let skill_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo\n";
    let notes_text = "# Notes v2\n";
    let request_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![
            ReadItem {
                uri: "skill://demo/SKILL.md",
                text: Some(skill_text),
                blob_b64: None,
            },
            ReadItem {
                uri: "skill://demo/notes.md",
                text: Some(notes_text),
                blob_b64: None,
            },
        ],
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(skill_text))),
            // 新条目列出了 notes.md 但 digest 给错（与重读内容不一致）
            extra_resources: vec![(
                "skill://demo/notes.md".to_string(),
                format!("sha256:{}", "0".repeat(64)),
            )],
            ..GetReply::default()
        },
        None,
        None,
        std::time::Duration::ZERO,
        Some(Arc::clone(&request_log)),
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
            // 旧条目 resources 未列出 notes.md → 读它时 Unlisted
            vec![SkillResource {
                uri: "skill://demo/SKILL.md".to_string(),
                digest: format!("sha256:{}", sha256_hex(skill_text)),
            }],
        )],
    );
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/notes.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    // Unlisted 场景 invoke 层错误文案固定为"未列入"（不区分恢复失败原因）
    assert!(
        err.to_string().contains("完整性校验失败"),
        "digest 不一致 → 恢复拒绝，保持 VerificationFailed，实际: {err}"
    );
    // 重读已发生（request_log 含 notes.md）→ 失败点确在 digest 校验分支
    let log = request_log.lock().unwrap();
    assert!(
        log.iter()
            .any(|e| e == "resources/read skill://demo/notes.md"),
        "新条目已列出 → 应重读请求资源再做 digest 校验，实际: {log:?}"
    );
    drop(log);
    // registry 未被回写（resources 仍不含 notes.md）
    assert!(
        !reg.skills_of("srv")[0]
            .resources
            .iter()
            .any(|r| r.uri == "skill://demo/notes.md"),
        "恢复失败不得回写条目"
    );
}

/// 恢复路径负向：skills/get 返回的新条目仍未列出请求 uri → 拒绝恢复
/// （VerificationFailed），registry 不回写。request_log 不含 notes.md 重读
/// （未列出即拒绝，不会重读请求资源）。
#[tokio::test]
async fn read_unlisted_recovery_new_entry_still_unlisted_rejected() {
    let skill_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo\n";
    let notes_text = "# Notes v2\n";
    let request_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![
            ReadItem {
                uri: "skill://demo/SKILL.md",
                text: Some(skill_text),
                blob_b64: None,
            },
            ReadItem {
                uri: "skill://demo/notes.md",
                text: Some(notes_text),
                blob_b64: None,
            },
        ],
        GetReply {
            // get 正常应答，但新条目 resources 仍不含 notes.md
            skill_digest: Some(format!("sha256:{}", sha256_hex(skill_text))),
            ..GetReply::default()
        },
        None,
        None,
        std::time::Duration::ZERO,
        Some(Arc::clone(&request_log)),
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
                digest: format!("sha256:{}", sha256_hex(skill_text)),
            }],
        )],
    );
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let err = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://demo/notes.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("未列入"),
        "新条目仍未列出请求 uri → 拒绝，实际: {err}"
    );
    let log = request_log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|e| e == "resources/read skill://demo/notes.md"),
        "新条目未列出 → 不重读请求资源即拒绝，实际: {log:?}"
    );
}

/// `refresh_entry_and_content` 直接单测（负向分支）：新条目列出了请求 uri
/// 但 digest 与重读内容不匹配 → None（拒绝）。
#[tokio::test]
async fn refresh_entry_and_content_digest_mismatch_rejected() {
    let skill_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo\n";
    let notes_text = "# Notes v2\n";
    let request_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![
            ReadItem {
                uri: "skill://demo/SKILL.md",
                text: Some(skill_text),
                blob_b64: None,
            },
            ReadItem {
                uri: "skill://demo/notes.md",
                text: Some(notes_text),
                blob_b64: None,
            },
        ],
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(skill_text))),
            extra_resources: vec![(
                "skill://demo/notes.md".to_string(),
                format!("sha256:{}", "0".repeat(64)),
            )],
            ..GetReply::default()
        },
        None,
        None,
        std::time::Duration::ZERO,
        Some(Arc::clone(&request_log)),
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer().clone();
    let result = crate::mcp::skill_discovery::refresh_entry_and_content(
        &peer,
        "srv",
        "skill://demo/SKILL.md",
        "skill://demo/notes.md",
    )
    .await;
    assert!(result.is_none(), "digest 不匹配 → 拒绝，实际: {result:?}");
    let log = request_log.lock().unwrap();
    assert!(
        log.iter()
            .any(|e| e == "resources/read skill://demo/notes.md"),
        "新条目已列出 → 应重读请求资源再做 digest 校验，实际: {log:?}"
    );
}

/// `refresh_entry_and_content` 直接单测（负向分支）：新条目未列出请求 uri
/// → None（拒绝，不重读请求资源）。
#[tokio::test]
async fn refresh_entry_and_content_new_entry_unlisted_rejected() {
    let skill_text = "---\nname: demo\ndescription: Demo skill\n---\n\n# Demo\n";
    let notes_text = "# Notes v2\n";
    let request_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![
            ReadItem {
                uri: "skill://demo/SKILL.md",
                text: Some(skill_text),
                blob_b64: None,
            },
            ReadItem {
                uri: "skill://demo/notes.md",
                text: Some(notes_text),
                blob_b64: None,
            },
        ],
        GetReply {
            // get 正常应答，但新条目 resources 仍不含 notes.md
            skill_digest: Some(format!("sha256:{}", sha256_hex(skill_text))),
            ..GetReply::default()
        },
        None,
        None,
        std::time::Duration::ZERO,
        Some(Arc::clone(&request_log)),
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer().clone();
    let result = crate::mcp::skill_discovery::refresh_entry_and_content(
        &peer,
        "srv",
        "skill://demo/SKILL.md",
        "skill://demo/notes.md",
    )
    .await;
    assert!(result.is_none(), "新条目未列出 → 拒绝，实际: {result:?}");
    let log = request_log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|e| e == "resources/read skill://demo/notes.md"),
        "新条目未列出 → 不重读请求资源即拒绝，实际: {log:?}"
    );
}

/// 恢复回写保留原条目 name（A-LOW-5）：registry 条目名经 disambiguate_names
/// 消歧（mcp__srv__acme_billing_refunds），而恢复出的 meta 名是未消歧的
/// mcp__srv__refunds——回写若直接替换会漂移/撞名。断言恢复后 name 保持
/// 消歧名，description/content 刷新为新值。
#[tokio::test]
async fn read_skill_recovery_writeback_keeps_original_name() {
    let old_text = "---\nname: refunds\ndescription: Refunds skill\n---\n\n# Refunds v1\n";
    let new_text = "---\nname: refunds\ndescription: Refunds skill\n---\n\n# Refunds v2\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(read_server(
        server_io,
        vec![ReadItem {
            uri: "skill://acme/billing/refunds/SKILL.md",
            text: Some(new_text),
            blob_b64: None,
        }],
        GetReply {
            skill_digest: Some(format!("sha256:{}", sha256_hex(new_text))),
            ..GetReply::default()
        },
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
    let mut entry = mcp_entry(
        "skill://acme/billing/refunds/SKILL.md",
        vec![SkillResource {
            uri: "skill://acme/billing/refunds/SKILL.md".to_string(),
            // 旧 digest：只匹配 old_text（stale——触发恢复）
            digest: format!("sha256:{}", sha256_hex(old_text)),
        }],
    );
    // 模拟 disambiguate_names 消歧后的注册名（未消歧名是 mcp__srv__refunds）
    entry.name = "mcp__srv__acme_billing_refunds".to_string();
    seed_registry(&reg, vec![entry]);
    let tool = McpResourceTool::new(pool, Arc::clone(&reg));
    let out = tool
        .invoke(
            serde_json::json!({"server_name": "srv", "uri": "skill://acme/billing/refunds/SKILL.md"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("恢复成功应返回新内容");
    assert!(out.contains("# Refunds v2"), "应返回新内容，实际: {out:?}");
    let skills = reg.skills_of("srv");
    assert_eq!(skills.len(), 1);
    assert_eq!(
        skills[0].name, "mcp__srv__acme_billing_refunds",
        "回写必须保留原条目 name（消歧名），不得漂移为未消歧名"
    );
    assert!(
        skills[0].content.is_none(),
        "W2：条目只发布 metadata，不落正文"
    );
    assert_eq!(
        skills[0].resources[0].digest,
        format!("sha256:{}", sha256_hex(new_text)),
        "刷新后的 manifest digest 必须回写"
    );
}
