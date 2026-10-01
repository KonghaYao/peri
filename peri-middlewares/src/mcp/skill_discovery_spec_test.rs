use super::*;

/// 规范模式端到端（W2）：skills/list 发现**只发布 metadata**（不读正文）——
/// digest 不一致的条目同样注册（其完整性由 activation 在激活时判定）。
#[tokio::test]
async fn run_discovery_spec_mode_via_skills_list() {
    let ok_text = "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha\n";
    let bad_text = "---\nname: beta\ndescription: Beta skill\n---\n\n# Beta\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![
            SpecSkill {
                uri: "skill://alpha/SKILL.md",
                name: "alpha",
                description: "Alpha skill",
                text: ok_text,
                digest_override: None,
                get_text: None,
                get_error: false,
                get_wrong_uri: false,
            },
            // beta：digest 故意给错 → 内容校验失败 → skills/get 返回同一
            // stale 快照 → 恢复失败 → 拒绝
            SpecSkill {
                uri: "skill://beta/SKILL.md",
                name: "beta",
                description: "Beta skill",
                text: bad_text,
                digest_override: Some(format!("sha256:{}", "0".repeat(64))),
                get_text: None,
                get_error: false,
                get_wrong_uri: false,
            },
        ],
        None,
        None,
        false,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(6u32);
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;

    let skills = reg.all_skills();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["mcp__srv__alpha", "mcp__srv__beta"],
        "W2：发现期只发布 metadata——结构合法的条目全部注册；beta 的 digest 不一致\
         属正文完整性，延迟到 activation 判定（不再于发现期过滤）"
    );
    assert!(
        skills.iter().all(|s| s.content.is_none()),
        "发现期不得携带正文（正文只在 activation 读取）"
    );
    assert!(
        skills.iter().all(|s| s.frontmatter.is_some()),
        "条目必须携带 frontmatter 快照供激活时全量比对"
    );
    assert_eq!(
        skills[0].resources.len(),
        1,
        "manifest 保留（内容绑定依据）"
    );
    assert_eq!(
        skills[0].origin,
        Some(SkillOrigin::Mcp {
            server: "srv".to_string(),
            uri: "skill://alpha/SKILL.md".to_string(),
        })
    );
}

/// 规范模式端到端（W2）：发现期**零正文读取、零 `skills/get` 恢复**——stale
/// digest、get 失败、frontmatter 不一致三类条目一律只发布 metadata；完整性
/// 判定与 stale 恢复归统一 activation（见 `mcp::skill_activation::tests`）。
#[tokio::test]
async fn run_discovery_spec_mode_publishes_entries_without_reads() {
    let old_text = "---\nname: gamma\ndescription: Gamma skill\n---\n\n# Gamma v1\n";
    let new_text = "---\nname: gamma\ndescription: Gamma skill\n---\n\n# Gamma v2\n";
    let bad_text = "---\nname: delta\ndescription: Delta skill\n---\n\n# Delta\n";
    let fm_mismatch_text = "---\nname: epsilon\ndescription: Eps content\n---\n\n# Eps\n";
    let request_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![
            // gamma：list 给新 digest + 旧内容（stale）→ 发现期不再恢复
            SpecSkill {
                uri: "skill://gamma/SKILL.md",
                name: "gamma",
                description: "Gamma skill",
                text: old_text,
                digest_override: Some(format!("sha256:{}", sha256_hex(new_text))),
                get_text: Some(new_text),
                get_error: false,
                get_wrong_uri: false,
            },
            // delta：get 会失败——发现期不再调用 get
            SpecSkill {
                uri: "skill://delta/SKILL.md",
                name: "delta",
                description: "Delta skill",
                text: bad_text,
                digest_override: Some(format!("sha256:{}", "0".repeat(64))),
                get_text: None,
                get_error: true,
                get_wrong_uri: false,
            },
            // epsilon：frontmatter 与条目快照不一致——同样只发布 metadata
            SpecSkill {
                uri: "skill://epsilon/SKILL.md",
                name: "epsilon",
                description: "Eps entry",
                text: fm_mismatch_text,
                digest_override: None,
                get_text: None,
                get_error: false,
                get_wrong_uri: false,
            },
        ],
        None,
        Some(Arc::clone(&request_log)),
        false,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(7u32);
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;

    let names: Vec<String> = reg.all_skills().iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        vec!["mcp__srv__delta", "mcp__srv__epsilon", "mcp__srv__gamma"],
        "三类条目（stale / get 失败 / frontmatter 不一致）都只发布 metadata"
    );
    assert!(
        reg.all_skills().iter().all(|s| s.content.is_none()),
        "发现期不携带正文"
    );

    let log = request_log.lock().unwrap();
    assert!(
        !log.iter().any(|e| e.starts_with("resources/read")),
        "发现期必须零 resources/read（正文只在 activation 读取），实际: {log:?}"
    );
    assert!(
        !log.iter().any(|e| e.starts_with("skills/get")),
        "发现期不做 stale 恢复（skills/get 归 activation），实际: {log:?}"
    );
}
