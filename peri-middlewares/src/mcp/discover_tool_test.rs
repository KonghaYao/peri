//! DiscoverMCPTool 测试：deferred/meta 元数据、search/list/detail 语义、
//! JSON-RPC 错误契约、只读断言（空 services 池 + 空 registry 不 panic）。

use std::{path::PathBuf, sync::Arc};

use peri_acp_types::{
    mcp_skills::{mcp_skill_name, HandleToken, McpSkillRegistry},
    skills::{SkillMetadata, SkillOrigin, SkillSource},
};
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::model::{Resource, Tool};
use serde_json::{json, Value};

use super::*;
use crate::mcp::client::{McpClientHandle, OAuthStatus};

fn schema() -> serde_json::Map<String, Value> {
    json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
        "required": ["text"]
    })
    .as_object()
    .unwrap()
    .clone()
}

fn make_tool(name: &str, description: &str) -> Tool {
    Tool::new(name.to_string(), description.to_string(), schema())
}

fn make_handle(name: &str, tools: Vec<Tool>, resources: Vec<Resource>) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        peer: None,
        tools,
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::Authorized,
        source: Some(ConfigSource::Project(PathBuf::from("/tmp/.mcp.json"))),
        url: Some(format!("https://{name}.example.com/mcp")),
        channel_capable: false,
    })
}

fn insert_handle(pool: &McpClientPool, handle: Arc<McpClientHandle>) {
    pool.clients.write().insert(handle.name.clone(), handle);
}

fn make_tool_box(registry: Option<Arc<McpSkillRegistry>>) -> DiscoverMCPTool {
    DiscoverMCPTool::new(Arc::new(McpClientPool::new_empty()), registry)
}

fn make_registry_with_skills() -> Arc<McpSkillRegistry> {
    let reg = Arc::new(McpSkillRegistry::new());
    let h: HandleToken = Arc::new(1u32);
    let skill = SkillMetadata {
        name: mcp_skill_name("zzsrv", "zzskill"),
        description: "A zz skill for zz query".to_string(),
        source: SkillSource::Mcp,
        origin: Some(SkillOrigin::Mcp {
            server: "zzsrv".to_string(),
            uri: "skill://zzskill/SKILL.md".to_string(),
        }),
        ..SkillMetadata::default()
    };
    reg.mark_discovery_started("zzsrv", h.clone());
    reg.mark_discovery_completed("zzsrv", h, vec![skill]);
    reg
}

async fn invoke(tool: &DiscoverMCPTool, method: &str, params: Value) -> Value {
    let out = tool
        .invoke(
            json!({ "method": method, "params": params }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .expect("invoke 恒 Ok");
    serde_json::from_str(&out).expect("invoke 输出应为合法 JSON")
}

// ─── 元数据：deferred / meta / schema ──────────────────────────────────────

#[test]
fn tool_metadata_is_deferred_meta() {
    let tool = make_tool_box(None);
    assert_eq!(tool.name(), "DiscoverMCP");
    assert!(!tool.is_direct(), "不覆写 is_direct → deferred");
    assert_eq!(tool.namespace(), Some("meta"));
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert_eq!(
        params["properties"]["method"]["enum"],
        json!(["search", "list", "detail"])
    );
    assert_eq!(params["required"], json!(["method"]));
}

// ─── search ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_hits_all_four_types_with_labels_and_schema() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "zz-server",
            vec![
                make_tool("zz_tool", "create zz issue"),
                make_tool("unrelated", "nothing here"),
            ],
            vec![Resource::new("zz://resource/1", "zz resource")],
        ),
    );
    let registry = make_registry_with_skills();
    let tool = DiscoverMCPTool::new(pool, Some(registry));

    let result = invoke(&tool, "search", json!({ "query": "zz" })).await;
    let arr = result.as_array().expect("search 应返回数组");
    assert_eq!(
        arr.len(),
        4,
        "server/tool/resource/skill 各一命中: {result}"
    );

    let types: std::collections::BTreeSet<&str> =
        arr.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        types.into_iter().collect::<Vec<_>>(),
        vec!["resource", "server", "skill", "tool"]
    );

    // server 结果带 name/status
    let server = arr.iter().find(|e| e["type"] == "server").unwrap();
    assert_eq!(server["name"], "zz-server");
    assert_eq!(server["status"], "connected");

    // tool 结果带完整 input schema（rmcp camelCase 序列化 inputSchema；供
    // ExecuteExtraTool 衔接）
    let tool_entry = arr.iter().find(|e| e["type"] == "tool").unwrap();
    assert_eq!(tool_entry["server"], "zz-server");
    assert_eq!(tool_entry["tool"]["name"], "zz_tool");
    assert_eq!(
        tool_entry["tool"]["inputSchema"]["properties"]["text"]["type"],
        "string"
    );

    // resource 结果带 uri
    let resource = arr.iter().find(|e| e["type"] == "resource").unwrap();
    assert_eq!(resource["uri"], "zz://resource/1");

    // skill 结果带 server（来自 origin）/name/description
    let skill_entry = arr.iter().find(|e| e["type"] == "skill").unwrap();
    assert_eq!(skill_entry["server"], "zzsrv");
    assert_eq!(skill_entry["name"], mcp_skill_name("zzsrv", "zzskill"));
}

#[tokio::test]
async fn search_matches_tool_description_and_is_case_insensitive() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "srv",
            vec![make_tool("quiet_name", "Secret Weapon X")],
            vec![],
        ),
    );
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(&tool, "search", json!({ "query": "WEAPON x" })).await;
    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 1, "描述子串 + 大小写无关: {result}");
    assert_eq!(arr[0]["type"], "tool");
    assert_eq!(arr[0]["tool"]["name"], "quiet_name");
}

#[tokio::test]
async fn search_skill_without_origin_parses_server_from_name() {
    let reg = Arc::new(McpSkillRegistry::new());
    let h: HandleToken = Arc::new(2u32);
    let skill = SkillMetadata {
        name: mcp_skill_name("plain", "myskill"),
        description: "plain skill".to_string(),
        ..SkillMetadata::default()
    };
    reg.mark_discovery_started("plain", h.clone());
    reg.mark_discovery_completed("plain", h, vec![skill]);

    let tool = make_tool_box(Some(reg));
    let result = invoke(&tool, "search", json!({ "query": "myskill" })).await;
    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "skill");
    assert_eq!(arr[0]["server"], "plain", "无 origin 时从全名解析 server");
}

#[tokio::test]
async fn search_empty_result_returns_empty_array() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "search", json!({ "query": "nothing_matches" })).await;
    assert_eq!(result, json!([]), "空结果返回 [] 而非错误对象");
}

#[tokio::test]
async fn search_missing_query_returns_32602() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "search", json!({})).await;
    assert_eq!(result["error"]["code"], -32602);
    assert!(result.get("id").is_none());
}

#[tokio::test]
async fn search_non_string_query_returns_32602() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "search", json!({ "query": 42 })).await;
    assert_eq!(result["error"]["code"], -32602);
}

#[tokio::test]
async fn search_max_results_defaults_to_five() {
    let pool = Arc::new(McpClientPool::new_empty());
    for i in 0..8 {
        insert_handle(&pool, make_handle(&format!("common{i}"), vec![], vec![]));
    }
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(&tool, "search", json!({ "query": "common" })).await;
    assert_eq!(
        result.as_array().unwrap().len(),
        5,
        "缺省 max_results = 5: {result}"
    );
}

#[tokio::test]
async fn search_max_results_capped_at_twenty() {
    let pool = Arc::new(McpClientPool::new_empty());
    let resources: Vec<Resource> = (0..25)
        .map(|i| Resource::new(format!("cap://res/{i}"), format!("r{i}")))
        .collect();
    insert_handle(&pool, make_handle("capsrv", vec![], resources));
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(
        &tool,
        "search",
        json!({ "query": "cap://", "max_results": 100 }),
    )
    .await;
    assert_eq!(
        result.as_array().unwrap().len(),
        20,
        "max_results 上限 clamp 20: {result}"
    );
}

#[tokio::test]
async fn search_explicit_max_results() {
    let pool = Arc::new(McpClientPool::new_empty());
    for i in 0..4 {
        insert_handle(&pool, make_handle(&format!("sub{i}"), vec![], vec![]));
    }
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(&tool, "search", json!({ "query": "sub", "max_results": 2 })).await;
    assert_eq!(result.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn search_non_object_params_returns_32602() {
    let tool = make_tool_box(None);
    let out = tool
        .invoke(
            json!({ "method": "search", "params": "not-an-object" }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["error"]["code"], -32602);
}

#[tokio::test]
async fn search_non_string_query_message_is_query_must_be_a_string() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "search", json!({ "query": 42 })).await;
    assert_eq!(result["error"]["code"], -32602);
    assert_eq!(result["error"]["message"], "query must be a string");
    // 缺失 query 仍走缺失消息（与类型错误区分）
    let missing = invoke(&tool, "search", json!({})).await;
    assert_eq!(missing["error"]["code"], -32602);
    assert_eq!(missing["error"]["message"], "缺少 query 参数（string）");
}

/// 同 type 多条目（≥2 tool、≥2 resource）：排序键完整后结果顺序跨运行
/// 稳定（resource 按 server+uri、tool 按 server+tool.name）。
#[tokio::test]
async fn search_same_type_entries_sorted_stably() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "srv",
            vec![
                make_tool("zebra_tool", "z"),
                make_tool("alpha_tool", "z"),
                make_tool("mid_tool", "z"),
            ],
            vec![
                Resource::new("z://res/z1", "z"),
                Resource::new("z://res/a1", "z"),
                Resource::new("z://res/m1", "z"),
            ],
        ),
    );
    let tool = DiscoverMCPTool::new(pool, None);
    let first = invoke(&tool, "search", json!({ "query": "z", "max_results": 20 })).await;
    let arr = first.as_array().expect("search 应返回数组");
    assert_eq!(arr.len(), 6, "3 tool + 3 resource 命中: {first}");

    // 排序：type 字典序 resource < tool；同 type 内 resource 按 uri、tool 按 name
    fn key_str(e: &Value) -> String {
        let (a, b, c) = sort_key(e);
        format!("{a}|{b}|{c}")
    }
    let keys: Vec<String> = arr.iter().map(key_str).collect();
    assert_eq!(
        keys,
        vec![
            "resource|srv|z://res/a1",
            "resource|srv|z://res/m1",
            "resource|srv|z://res/z1",
            "tool|srv|alpha_tool",
            "tool|srv|mid_tool",
            "tool|srv|zebra_tool",
        ],
        "统一排序键后的稳定顺序: {first}"
    );

    // 跨运行重复调用结果一致（消除 HashMap 迭代抖动）
    for _ in 0..5 {
        let again = invoke(&tool, "search", json!({ "query": "z", "max_results": 20 })).await;
        assert_eq!(first, again, "重复运行应产出相同序列");
    }
}

/// 截断子集确定：同 type 多条目 + max_results 截断时，多次调用截取的
/// 子集一致（排序稳定 + truncate 截前 N 条）。
#[tokio::test]
async fn search_truncated_subset_deterministic() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "srv",
            vec![
                make_tool("zebra_tool", "z"),
                make_tool("alpha_tool", "z"),
                make_tool("mid_tool", "z"),
            ],
            vec![
                Resource::new("z://res/z1", "z"),
                Resource::new("z://res/a1", "z"),
                Resource::new("z://res/m1", "z"),
            ],
        ),
    );
    let tool = DiscoverMCPTool::new(pool, None);
    let first = invoke(&tool, "search", json!({ "query": "z", "max_results": 4 })).await;
    let arr = first.as_array().unwrap();
    assert_eq!(arr.len(), 4, "截断到 4: {first}");
    let keys: Vec<String> = arr
        .iter()
        .map(|e| {
            let (a, b, c) = sort_key(e);
            format!("{a}|{b}|{c}")
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            "resource|srv|z://res/a1",
            "resource|srv|z://res/m1",
            "resource|srv|z://res/z1",
            "tool|srv|alpha_tool",
        ],
        "截断子集应确定: {first}"
    );
    for _ in 0..5 {
        let again = invoke(&tool, "search", json!({ "query": "z", "max_results": 4 })).await;
        assert_eq!(first, again, "重复运行截断子集一致");
    }
}

// ─── list ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_three_domain_summary() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "srv",
            vec![make_tool("t1", "d"), make_tool("t2", "d")],
            vec![Resource::new("file:///a", "a")],
        ),
    );
    let registry = make_registry_with_skills();
    let tool = DiscoverMCPTool::new(pool, Some(registry));

    let result = invoke(&tool, "list", json!({ "server": "srv" })).await;
    assert_eq!(result["server"], "srv");
    assert_eq!(result["tools"], json!(["t1", "t2"]));
    assert_eq!(result["resources"], json!(["file:///a"]));
    assert_eq!(result["skills"], json!([]), "srv 无远端 skill");
}

#[tokio::test]
async fn list_single_domain() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "srv",
            vec![make_tool("t1", "d")],
            vec![Resource::new("file:///a", "a")],
        ),
    );
    let tool = DiscoverMCPTool::new(pool, None);

    let tools = invoke(&tool, "list", json!({ "server": "srv", "domain": "tools" })).await;
    assert_eq!(tools, json!(["t1"]));
    let resources = invoke(
        &tool,
        "list",
        json!({ "server": "srv", "domain": "resources" }),
    )
    .await;
    assert_eq!(resources, json!(["file:///a"]));
    let skills = invoke(
        &tool,
        "list",
        json!({ "server": "srv", "domain": "skills" }),
    )
    .await;
    assert_eq!(skills, json!([]));
}

#[tokio::test]
async fn list_unknown_domain_returns_32602() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(&pool, make_handle("srv", vec![], vec![]));
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(
        &tool,
        "list",
        json!({ "server": "srv", "domain": "prompts" }),
    )
    .await;
    assert_eq!(result["error"]["code"], -32602);
}

#[tokio::test]
async fn list_missing_server_returns_32602() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "list", json!({})).await;
    assert_eq!(result["error"]["code"], -32602);
}

#[tokio::test]
async fn list_unknown_server_returns_32000() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "list", json!({ "server": "ghost" })).await;
    assert_eq!(result["error"]["code"], -32000);
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ghost"));
}

#[tokio::test]
async fn list_not_connected_returns_32000() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mut handle = make_handle("srv", vec![], vec![]);
    Arc::get_mut(&mut handle).unwrap().status = ClientStatus::Failed("boom".to_string());
    insert_handle(&pool, handle);
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(&tool, "list", json!({ "server": "srv" })).await;
    assert_eq!(result["error"]["code"], -32000);
}

// ─── detail ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn detail_full_fields_peer_none() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_handle(
        &pool,
        make_handle(
            "srv",
            vec![make_tool("t1", "d"), make_tool("t2", "d")],
            vec![Resource::new("file:///a", "a")],
        ),
    );
    let registry = make_registry_with_skills(); // zzsrv 有 1 skill
    let tool = DiscoverMCPTool::new(pool, Some(registry));

    let result = invoke(&tool, "detail", json!({ "server": "srv" })).await;
    assert_eq!(result["server"], "srv");
    assert_eq!(result["status"], "connected");
    assert_eq!(result["oauth_status"], "authorized");
    assert_eq!(result["source"], "project");
    assert_eq!(result["url"], "https://srv.example.com/mcp");
    assert_eq!(result["tool_count"], 2);
    assert_eq!(result["resource_count"], 1);
    assert_eq!(result["skill_count"], 0);
    // peer = None 分支：不产生 protocol_version/capabilities 字段
    assert!(result.get("protocol_version").is_none());
    assert!(result.get("capabilities").is_none());
}

#[tokio::test]
async fn detail_unknown_server_returns_32000() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "detail", json!({ "server": "ghost" })).await;
    assert_eq!(result["error"]["code"], -32000);
}

#[tokio::test]
async fn detail_not_connected_returns_32000() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mut handle = make_handle("srv", vec![], vec![]);
    Arc::get_mut(&mut handle).unwrap().status = ClientStatus::Disconnected;
    insert_handle(&pool, handle);
    let tool = DiscoverMCPTool::new(pool, None);
    let result = invoke(&tool, "detail", json!({ "server": "srv" })).await;
    assert_eq!(result["error"]["code"], -32000);
}

#[tokio::test]
async fn detail_missing_server_param_returns_32602() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "detail", json!({})).await;
    assert_eq!(result["error"]["code"], -32602);
}

// ─── 未知 method / 错误对象结构 ────────────────────────────────────────────

#[tokio::test]
async fn unknown_method_returns_32601() {
    let tool = make_tool_box(None);
    let result = invoke(&tool, "call", json!({ "server": "srv" })).await;
    assert_eq!(result["error"]["code"], -32601);
    assert_eq!(
        result,
        json!({ "error": { "code": -32601, "message": "未知 method: call" } }),
        "错误对象结构 {{error:{{code,message}}}} 且无 id"
    );
}

#[tokio::test]
async fn missing_method_returns_32602() {
    let tool = make_tool_box(None);
    let out = tool
        .invoke(json!({}), ToolContext::new(&[], "/tmp"))
        .await
        .unwrap();
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["error"]["code"], -32602);
    assert!(result.get("id").is_none());
}

// ─── 只读断言（验收 6 半边）：空 services 池 + 空 registry 不 panic ─────────

#[tokio::test]
async fn empty_pool_and_registry_search_and_list_do_not_panic() {
    // new_empty 只建池不建 services；registry 为 None
    let tool = make_tool_box(None);

    let search = invoke(&tool, "search", json!({ "query": "any" })).await;
    assert_eq!(search, json!([]), "空环境 search 返回空数组");

    let list = invoke(&tool, "list", json!({ "server": "any" })).await;
    assert_eq!(list["error"]["code"], -32000);

    let detail = invoke(&tool, "detail", json!({ "server": "any" })).await;
    assert_eq!(detail["error"]["code"], -32000);
}

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
        name: mcp_skill_name("zzs1", "skill-z"),
        description: "zz skill".to_string(),
        ..Default::default()
    };
    let skill_a = SkillMetadata {
        name: mcp_skill_name("zzs1", "skill-a"),
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
            mcp_skill_name("zzs1", "skill-a").as_str(),
            mcp_skill_name("zzs1", "skill-z").as_str(),
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

    let found = index.get_tool("DiscoverMCP").expect("按名可取回");
    assert_eq!(found.namespace(), Some("meta"), "namespace 为 meta 分组");
    assert!(!found.is_direct(), "deferred 工具不进直接工具列表");
}
