//! P0 回归：会话创建期的**冻结 system prompt 必须看到 Agent 候选目录**。
//!
//! 缺陷形状：`{{available_agents}}` 段在 `SessionEnvironment::assemble_with_frozen`
//! **返回之后**渲染（`session_lifecycle.rs` → `prepared.rs::build_frozen_after_activation`
//! → `session/frozen.rs` 经目录端口渲染），而 turn 级装配 bind 要等首轮才发生——
//! 绑定缺失时新建会话的目录恒为 `No agents currently configured`，主 Agent 看不到
//! 任何子 Agent 候选。
//!
//! 用例走生产链路（`session/new` → `assemble_with_frozen` → 冻结渲染），目录来自
//! 真实 builtin `workspace` 实例的 `resources/list` 投影（会话 cwd 的
//! `.claude/agents/*.md`），没有测试专用构造器参与。
//! 反向验证（mutation）：注释掉 `assemble_with_frozen` 里的定点绑定调用 ⇒ 本用例失败。

use super::*;

/// 会话 cwd 下的 project agent 定义 id（= 文件名词干）。
const AGENT_ID: &str = "frozen-catalog-probe-agent";

/// 夹具：`startup`（装配起点）与 `target`（会话 cwd，含 `.claude/agents` 定义与 settings）。
///
/// `target` 的项目 settings **自带 provider**：会话 cwd 与 `startup_cwd` 不同目录时，
/// 准备路径按「异目录只读一次」在会话目录重解析配置。`meta_harness` 是 settings 里
/// `meta_harness` 对象的正文（空串 = 不设开关），关闭集差分与基线用例共用同一夹具。
fn agent_catalog_fixture(meta_harness: &str) -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    let agents = target.join(".claude").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    // 定义必须**在装配之前**落盘：builtin 实例在装配期快照 `resources/list`。
    std::fs::write(
        agents.join(format!("{AGENT_ID}.md")),
        format!(
            "---\nname: {AGENT_ID}\ndescription: Frozen catalog probe\nmodel: haiku\ndisallowedTools: [Write, Edit, Bash, folder_operations, cron_register]\n---\n\nProbe.\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(target.join(".peri")).unwrap();
    std::fs::write(
        target.join(".peri/settings.json"),
        format!(
            r#"{{"config":{{"active_alias":"sonnet","providers":[{{"id":"test","type":"openai","apiKey":"key","models":{{"sonnet":"model"}}}}],"meta_harness":{{{meta_harness}}}}}}}"#
        ),
    )
    .unwrap();
    (
        tmp,
        startup.to_string_lossy().into_owned(),
        target.to_string_lossy().into_owned(),
    )
}

/// 生产装配的宿主配置：bare（只建 workspace 池）+ 指定的启动目录。
async fn agent_catalog_server_config(
    tmp: &tempfile::TempDir,
    startup_cwd: String,
) -> AcpServerConfig {
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd,
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    cfg
}

/// `session/new` 的冻结 system prompt 必须逐字含会话 cwd 的 project agent 条目。
///
/// 断言面选在**冻结字节**（`frozen.system_prompt()`）而非端口返回值：端口被绑定
/// 但渲染次序错位同样会让目录为空，只有冻结产物能证明「渲染时目录已就位」。
///
/// `#[serial]`：builtin 注入开关是进程级 env（`PERI_MCP_BUILTIN`，由注入关闭的
/// 串行用例改写），本用例必须有 builtin 实例在场。
#[tokio::test]
#[serial]
async fn new_session_frozen_prompt_lists_agent_catalog_entries() {
    let (tmp, startup, target) = agent_catalog_fixture("");
    let cfg = agent_catalog_server_config(&tmp, startup).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 必须成功");

    let id = created["sessionId"].as_str().unwrap();
    let frozen = sessions[id].frozen.as_ref().expect("创建必须发布 frozen");
    let prompt = frozen.system_prompt();

    assert!(
        !prompt.contains("No agents currently configured"),
        "冻结目录不得为空：目录面必须在渲染前绑定（P0）；冻结 prompt 的 available_agents 段={}",
        available_agents_section(prompt)
    );
    assert!(
        prompt.contains(&format!("- {AGENT_ID} [haiku] [readonly]")),
        "冻结 prompt 必须含 project agent 条目（id / tier / access）；实际 available_agents 段={}",
        available_agents_section(prompt)
    );
    // 端口直读：与关闭集差分用例共用同一观测面（会话 cfg 上的目录端口）——打开配置下
    // 必须能看见候选，否则关闭侧的「端口为空」断言会退化成恒真。
    let environment = sessions[id]
        .environment
        .as_ref()
        .expect("会话环境（生产装配路径）");
    let port_ids: Vec<String> =
        peri_acp_types::ports::AgentCatalogPort::catalog(&*environment.cfg.agent_catalog, false)
            .into_iter()
            .map(|entry| entry.id)
            .collect();
    assert!(
        port_ids.contains(&AGENT_ID.to_string()),
        "打开配置：会话创建期 bind 后端口目录必须含 project 候选；实际={port_ids:?}"
    );

    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

/// 关闭集差分（同一夹具，只差 `meta_harness.SubAgentMiddleware=false`）。
///
/// 本修复在**两个** bind 点各自派生链槽关闭位：会话创建期（`assemble_with_frozen`，
/// 源自本用例的 settings）与 turn 级（`assembly/preparation.rs`）——两侧同源靠共用
/// `McpAgentRegistry::for_session` 保证。本用例从两个观测面钉住会话创建期这一处：
///
/// - **端口**（`environment.cfg.agent_catalog`）：关闭集命中 ⇒ 目录必须为空。这是
///   唯一能分辨「关闭位派生错成恒 false」的观测面——`11_subagent` 段落本身随关闭集
///   消失，只断言冻结 prompt 会掩盖该缺陷形状；
/// - **冻结字节**（`frozen.system_prompt()`）：不得列出候选，也不得泄漏未替换的
///   `{{available_agents}}` 原文。
///
/// `#[serial]` 同基线用例（builtin 注入开关是进程级 env）。
#[tokio::test]
#[serial]
async fn new_session_frozen_prompt_hides_catalog_when_sub_agent_face_closed() {
    let (tmp, startup, target) = agent_catalog_fixture(r#""SubAgentMiddleware":false"#);
    let cfg = agent_catalog_server_config(&tmp, startup).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（链槽关闭）必须成功——关闭不阻塞创建");

    let id = created["sessionId"].as_str().unwrap();
    let frozen = sessions[id].frozen.as_ref().expect("创建必须发布 frozen");
    let prompt = frozen.system_prompt();

    assert!(
        !prompt.contains(AGENT_ID),
        "SubAgentMiddleware 关闭 ⇒ 本地面关闭 ⇒ 冻结 prompt 不得列出候选；实际 available_agents 段={}",
        available_agents_section(prompt)
    );
    assert!(
        !prompt.contains("{{available_agents}}"),
        "占位符必须已被替换：不得原文泄漏进冻结 prompt"
    );
    // 端口直读（与段落收集**无关**的观测面）：`11_subagent` 段落随关闭集消失会掩盖
    // 「关闭位被派生成恒 false」这一缺陷形状，只有绑在会话 cfg 上的端口能分辨——
    // 关闭位若派生错，目录仍会经端口可见，即「工具面已关闭、目录面仍可见」（X4/J5）。
    let environment = sessions[id]
        .environment
        .as_ref()
        .expect("会话环境（生产装配路径）");
    assert!(
        peri_acp_types::ports::AgentCatalogPort::catalog(&*environment.cfg.agent_catalog, false)
            .is_empty(),
        "关闭集命中 ⇒ 会话创建期 bind 派生的本地面关闭 ⇒ 端口目录必须为空"
    );

    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

/// 失败诊断：只截取 available_agents 段，避免把整份 system prompt 打进断言输出。
fn available_agents_section(prompt: &str) -> String {
    let start = prompt
        .find("subagent catalog")
        .or_else(|| prompt.find("No agents currently configured"))
        .unwrap_or(0);
    prompt[start..].chars().take(400).collect()
}
