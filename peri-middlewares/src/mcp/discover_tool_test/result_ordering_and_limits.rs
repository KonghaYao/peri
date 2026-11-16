// ─── 确定性排序（组 A 评审修复）────────────────────────────────────────────

/// search 结果必须在 truncate 前按 (type, name) 稳定排序：来源含 HashMap /
/// 快照迭代，顺序不定——截断前排序消除跨运行抖动（ARC-SERIAL-001）。
#[tokio::test]
async fn search_results_sorted_by_type_then_name() {
    let pool = Arc::new(McpClientPool::new_empty());
    // 插入顺序故意与字典序相反：b 先入、a 后入（HashMap 迭代顺序本身不定，
    // 断言只看排序后输出）
    insert_handle(
        &pool,
        make_handle(
            "zz-srv-b",
            vec![make_tool("zz_tool", "create zz")],
            vec![Resource::new("zz://res/1", "zz resource")],
        ),
    );
    insert_handle(&pool, make_handle("zz-srv-a", vec![], vec![]));

    // registry：同一 server 内条目序故意反字典序（z 先于 a）——排序必须把
    // skill-a 提到 skill-z 前（证明同 type 内按 name 排序生效）
    let reg = Arc::new(McpSkillRegistry::new());
    let h: HandleToken = Arc::new(3u32);
    let skill_z = SkillMetadata {
        name: mcp_skills::mcp_skill_name("zzs1", "skill-z"),
        aliases: Vec::new(),
        description: "zz skill".to_string(),
        ..Default::default()
    };
    let skill_a = SkillMetadata {
        name: mcp_skills::mcp_skill_name("zzs1", "skill-a"),
        aliases: Vec::new(),
        description: "zz skill".to_string(),
        ..Default::default()
    };
    reg.mark_discovery_started("zzs1", h.clone());
    reg.mark_discovery_completed("zzs1", h, vec![skill_z, skill_a]);

    let tool = DiscoverMCPTool::new(pool, Some(reg));
    let result = invoke(&tool, "search", json!({ "query": "zz", "max_results": 20 })).await;
    let arr = result.as_array().expect("search 应返回数组");

    // (type, name) 序列整体非降
    let pairs: Vec<(String, String)> = arr
        .iter()
        .map(|e| {
            (
                e["type"].as_str().unwrap_or("").to_string(),
                e["name"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let mut sorted = pairs.clone();
    sorted.sort();
    assert_eq!(pairs, sorted, "结果必须按 (type, name) 稳定排序: {result}");

    // type 字典序：resource < server < skill < tool；同 type 内 name 升序
    let types: Vec<&str> = arr.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        vec!["resource", "server", "server", "skill", "skill", "tool"]
    );
    let server_names: Vec<&str> = arr
        .iter()
        .filter(|e| e["type"] == "server")
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        server_names,
        vec!["zz-srv-a", "zz-srv-b"],
        "server 按 name 升序"
    );
    let skill_names: Vec<&str> = arr
        .iter()
        .filter(|e| e["type"] == "skill")
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        skill_names,
        vec![
            mcp_skills::mcp_skill_name("zzs1", "skill-a").as_str(),
            mcp_skills::mcp_skill_name("zzs1", "skill-z").as_str(),
        ],
        "skill 按 name 升序（条目序被排序纠正）"
    );
}

// ─── max_results 参数错误（组 A 评审修复）───────────────────────────────────

#[tokio::test]
async fn search_negative_max_results_returns_32602() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "search", json!({ "query": "zz", "max_results": -1 })).await;
    assert_eq!(result["error"]["code"], -32602);
    assert_eq!(
        result["error"]["message"], "max_results must be a non-negative integer",
        "负数不得静默归零，必须走 -32602 参数错误契约"
    );
}

#[tokio::test]
async fn search_float_max_results_returns_32602() {
    let tool = make_tool_box(None);
    let result = invoke(
        &tool,
        "search",
        json!({ "query": "zz", "max_results": 1.5 }),
    )
    .await;
    assert_eq!(result["error"]["code"], -32602);
    assert_eq!(
        result["error"]["message"],
        "max_results must be a non-negative integer"
    );
}

// ─── ToolSearch 索引可见性（验收 1 锁定）────────────────────────────────────

/// DiscoverMCP 进 ToolSearchIndex 后 search("discover") 可命中、且 namespace
/// 为 meta 分组可见（deferred + meta 契约的工具搜索半边）。
#[test]
fn tool_search_index_hits_discover_mcp_with_meta_namespace() {
    let tool: Arc<dyn BaseTool> = Arc::new(make_tool_box(None));
    let index = crate::tool_search::ToolSearchIndex::new();
    index.build(vec![Arc::clone(&tool) as Arc<dyn BaseTool>]);

    let results = index.search("discover", 10);
    assert!(
        results.iter().any(|r| r.name == "DiscoverMCP"),
        "search(\"discover\") 必须命中 DiscoverMCP: {:?}",
        results.iter().map(|r| &r.name).collect::<Vec<_>>()
    );

    let found = index.tool("DiscoverMCP").expect("按名可取回");
    assert_eq!(found.namespace(), Some("meta"), "namespace 为 meta 分组");
    assert!(!found.is_direct(), "deferred 工具不进直接工具列表");
}
