//! W4b：技能来源下沉后的端到端证据（生产装配路径，无测试专用构造器）。
//!
//! 覆盖面（`requests_test.rs` 的子模块，与 `meta_resources.rs` 同夹具模式）：
//! 1. [`new_session_frozen_summary_comes_from_workspace_origin`]：项目级
//!    `.claude/skills/<name>/SKILL.md` 经 builtin `workspace` 实例的
//!    `skills/list` 进入 P4 快照 → 冻结技能摘要含该技能与 scope 标签
//!    （J1：system 来源摘要进冻结面；宿主侧无扫描点，唯一通路是 MCP）；
//! 2. [`disable_bundled_skills_true_hides_builtin_catalog`]（差分对照）：
//!    `disableBundledSkills=true` ⇒ builtin 静态资产不出现，项目技能仍在
//!    （证明关的是 builtin 面而不是整个技能面）；
//! 3. [`closed_workspace_instance_makes_skills_unavailable_without_disk_fallback`]
//!    （差分对照）：同一夹具只差 `meta_harness.WorkspaceMiddleware=false` ⇒
//!    技能整体不可得，且磁盘哨兵不出现在冻结面（X4/J5：关闭即不可发现、
//!    零 FS 兜底）。
//!
//! 磁盘哨兵：SKILL.md 的 description 是唯一会进摘要的字段，因此「摘要不含
//! 哨兵 description / 完全为空」直接证明宿主没有读盘。

use super::*;

/// 项目级技能名（`{cwd}/.claude/skills/<name>/SKILL.md`）。
const PROJECT_SKILL: &str = "e2e-project-skill";
/// W5：项目指令（`AGENTS.md`）哨兵——用于证明指令经资源面获得、关闭后零磁盘兜底。
const PROJECT_INSTRUCTION_SENTINEL: &str = "W5-PROJECT-INSTRUCTION-SENTINEL";
/// 项目级技能描述：同时作为摘要哨兵（D4 摘要只放 name + 来源标签，
/// 因此本字段不应出现——出现即说明走的是别的路径）。
const PROJECT_SKILL_DESC: &str = "W4B-E2E-PROJECT-SKILL-DESC";
/// builtin 静态资产名（provider 侧 `BUILTIN_SKILLS` 中稳定存在的一项）。
const BUILTIN_SKILL: &str = "use-artifacts";

/// 夹具：隔离 `$HOME` + `startup`（装配起点）+ `target`（会话 cwd）。
///
/// **HOME 隔离（TEST-HERMETIC-001）**：W4b 后技能面覆盖 User 根
/// （`~/.claude/skills`）与全局配置（`~/.peri/settings.json` 的 `skillsDir` /
/// `disableBundledSkills`）。不重定向 HOME 会读到运行机器的真实技能目录
/// （非确定），也无法构造 `disableBundledSkills=true`——该位按 F12 语义只读
/// 全局配置。`HomeDirGuard` 复用父模块的进程级重定向（本文件用例因此全部
/// `#[serial]`）。
///
/// `.claude/skills/<PROJECT_SKILL>/SKILL.md` 的 frontmatter 用**逐字** name /
/// description（X3：wire 不改写），scope 由 provider 按根归属标为 `project`。
struct SkillFixture {
    _tmp: tempfile::TempDir,
    _home: super::HomeDirGuard,
    startup: String,
    target: String,
}

impl SkillFixture {
    fn new(meta_harness: &str, disable_bundled: bool) -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let startup = tmp.path().join("startup");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(&startup).unwrap();
        let skill_dir = target.join(".claude").join("skills").join(PROJECT_SKILL);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {PROJECT_SKILL}\ndescription: {PROJECT_SKILL_DESC}\n---\n\n# Project skill body\n"),
        )
        .unwrap();
        // W5：项目指令（资源面 `peri-instruction://workspace/main` 的来源文件）。
        std::fs::write(
            target.join("AGENTS.md"),
            format!("# Project rules\n\n{PROJECT_INSTRUCTION_SENTINEL}\n"),
        )
        .unwrap();
        // 全局配置（HOME）：`disableBundledSkills` 是全局位（F12 语义）。
        std::fs::create_dir_all(home.join(".peri")).unwrap();
        std::fs::write(
            home.join(".peri/settings.json"),
            format!(r#"{{"config":{{"disableBundledSkills":{disable_bundled}}}}}"#),
        )
        .unwrap();
        // 项目配置（target）：会话 cwd 与 startup 不同目录时，准备路径按
        // 「异目录只读一次」在会话目录重解析配置——自带 provider 让该解析不依赖
        // 进程级 env/HOME 残留（同批用例可独立运行）。
        std::fs::create_dir_all(target.join(".peri")).unwrap();
        std::fs::write(
            target.join(".peri/settings.json"),
            format!(
                r#"{{"config":{{"active_alias":"sonnet","providers":[{{"id":"test","type":"openai","apiKey":"key","models":{{"sonnet":"model"}}}}],"meta_harness":{{{meta_harness}}}}}}}"#
            ),
        )
        .unwrap();
        let guard = super::HomeDirGuard::set(&home);
        Self {
            _tmp: tmp,
            _home: guard,
            startup: startup.to_string_lossy().into_owned(),
            target: target.to_string_lossy().into_owned(),
        }
    }
}

/// 生产装配的宿主配置：bare（只建 workspace 池）+ 指定启动目录。
async fn skill_server_config(tmp: &tempfile::TempDir, startup_cwd: String) -> AcpServerConfig {
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd,
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    cfg
}

/// 失败诊断：会话池里真实 builtin `workspace` 句柄的资源清单投影。
fn workspace_resource_uris(sessions: &HashMap<String, SessionState>, id: &str) -> Vec<String> {
    let Some(environment) = sessions[id].environment.as_ref() else {
        return vec!["无执行环境".to_owned()];
    };
    let Some(pool) = environment.cfg.mcp_pool.as_ref() else {
        return vec!["无 MCP 池".to_owned()];
    };
    let Some(pool) = pool
        .as_any()
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
    else {
        return vec!["池不是生产 McpClientPool".to_owned()];
    };
    pool.get_resources("workspace")
        .iter()
        .map(|resource| resource.uri.clone())
        .collect()
}

/// W4b 主链：项目技能经 workspace 实例进入**冻结技能摘要**（J1 / F3）。
///
/// 断言链：
/// 1. provider 的资源清单含 `skill://project/{name}/SKILL.md`（技能面真的接线了，
///    与 W4a 只接 meta 面的口径相反——本条同时是「W4a 域隔离已解除」的证据）；
/// 2. 冻结技能摘要含该技能名与 `[project]` scope 标签（来源由 URI 派生）；
/// 3. 冻结数据里**不含**磁盘哨兵 description（D4 摘要不放 description——因此
///    「不含」不能单独证明未读盘，真正的证明是与用例 3 的差分）。
#[tokio::test]
#[serial]
async fn new_session_frozen_summary_comes_from_workspace_origin() {
    let fixture = SkillFixture::new(r#""01_intro":true"#, false);
    let cfg = skill_server_config(&fixture._tmp, fixture.startup.clone()).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({"cwd": fixture.target.clone()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（技能面已接线）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let frozen = sessions[&id].frozen.as_ref().expect("创建必须发布 frozen");

    let uris = workspace_resource_uris(&sessions, &id);
    assert!(
        uris.contains(&format!("skill://project/{PROJECT_SKILL}/SKILL.md")),
        "技能资源面必须经 workspace 实例暴露：{uris:?}"
    );

    let summary = frozen.skill_summary().unwrap_or_default();
    assert!(
        summary.contains(PROJECT_SKILL),
        "冻结技能摘要必须含 workspace origin 的项目技能：summary={summary:?}, uris={uris:?}"
    );
    assert!(
        summary.contains("[project]"),
        "来源标签必须由资源 URI 的 scope 派生：summary={summary:?}"
    );
    assert!(
        !summary.contains(PROJECT_SKILL_DESC),
        "摘要只放 name + 来源标签（D4），不放 description：summary={summary:?}"
    );
    assert_eq!(
        summary.matches(PROJECT_SKILL).count(),
        1,
        "同一技能不得重复出现在摘要（不双源）：summary={summary:?}"
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

/// 差分对照：`disableBundledSkills=true` ⇒ builtin 静态资产不进目录，项目技能照常。
#[tokio::test]
#[serial]
async fn disable_bundled_skills_true_hides_builtin_catalog() {
    let enabled = SkillFixture::new(r#""01_intro":true"#, false);
    let cfg_enabled = skill_server_config(&enabled._tmp, enabled.startup.clone()).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions_enabled = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": enabled.target.clone()}),
        &cfg_enabled,
        &mut sessions_enabled,
        &transport,
    )
    .await
    .expect("session/new（builtin 启用）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let summary_enabled = sessions_enabled[&id]
        .frozen
        .as_ref()
        .expect("frozen")
        .skill_summary()
        .unwrap_or_default()
        .to_owned();
    assert!(
        summary_enabled.contains(BUILTIN_SKILL),
        "默认（builtin 启用）时静态资产必须出现：summary={summary_enabled:?}"
    );

    // 同一夹具、只差 `disableBundledSkills`
    let disabled = SkillFixture::new(r#""01_intro":true"#, true);
    let cfg = skill_server_config(&disabled._tmp, disabled.startup.clone()).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": disabled.target.clone()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（builtin 关闭）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let frozen = sessions[&id].frozen.as_ref().expect("frozen");
    let summary = frozen.skill_summary().unwrap_or_default();

    assert!(
        !summary.contains(BUILTIN_SKILL),
        "disableBundledSkills=true ⇒ builtin 静态资产不得出现：summary={summary:?}"
    );
    assert!(
        summary.contains(PROJECT_SKILL),
        "关的是 builtin 面，项目级来源照常：summary={summary:?}"
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

/// 差分对照：A24 关闭 builtin `workspace` ⇒ 技能整体不可得，且零磁盘兜底。
#[tokio::test]
#[serial]
async fn closed_workspace_instance_makes_skills_unavailable_without_disk_fallback() {
    // 同 `new_session_frozen_summary_comes_from_workspace_origin` 的夹具，
    // 只差关闭位 `meta_harness.WorkspaceMiddleware=false`。
    let fixture = SkillFixture::new(r#""01_intro":true,"WorkspaceMiddleware":false"#, false);
    let cfg = skill_server_config(&fixture._tmp, fixture.startup.clone()).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({"cwd": fixture.target.clone()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（实例关闭）必须成功——关闭不阻塞创建");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let frozen = sessions[&id].frozen.as_ref().expect("frozen");

    let summary = frozen.skill_summary().unwrap_or_default();
    assert!(
        !summary.contains(PROJECT_SKILL),
        "关闭实例 ⇒ 技能不可得（X4）：summary={summary:?}, uris={:?}",
        workspace_resource_uris(&sessions, &id)
    );
    assert!(
        summary.is_empty(),
        "关闭实例时冻结摘要必须为空（不回落任何来源）：summary={summary:?}"
    );
    assert!(
        !frozen.system_prompt().contains(PROJECT_SKILL),
        "磁盘哨兵不得出现在冻结 system prompt（零 FS 兜底）"
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

/// 轮询等待命令出现（发现管线异步；上界覆盖内进程链路的 RTT 余量）。
async fn wait_command(
    registry: &Arc<peri_acp_types::command_registry::CommandRegistry>,
    name: &str,
    deadline: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        if registry.resolve(name).is_some() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// W4b 收口（F6 关闭位，差分对照）：`meta_harness.SkillsMiddleware=false` ⇒
/// 发现完成后 `core:{skill}` 裸名命令**不注册**（链槽关闭的配套半边：
/// `SkillsMiddleware` 不构造 ⇒ 13_skills 段落 + SkillTool/DiscoverSkillsTool
/// 消失，命令面不得留下幽灵路由）；默认配置 ⇒ 有。
///
/// 同批锁定「不是实例关闭」的边界：`{server}:{skill}` MCP 发现面与冻结技能
/// 摘要数据照常（`SkillsMiddleware` 关闭不改变 workspace 实例与 MCP 发现），
/// 且摘要不得经旁路进入冻结 system prompt（13_skills 段随 disabled 集不收集）。
///
/// 驱动形态 = 生产 `server_loop` 的等价路径：`session/new` → 环境 cfg 上
/// `after_new_response`（命令首发 + MCP 发现预热）→ 轮询会话命令注册表。
#[tokio::test]
#[serial]
async fn skills_middleware_disabled_hides_core_commands_but_keeps_mcp_face() {
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // ── 正例：默认配置（SkillsMiddleware 在链上）──
    let enabled = SkillFixture::new(r#""01_intro":true"#, false);
    let cfg_enabled = skill_server_config(&enabled._tmp, enabled.startup.clone()).await;
    let mut sessions_enabled = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": enabled.target.clone()}),
        &cfg_enabled,
        &mut sessions_enabled,
        &transport,
    )
    .await
    .expect("session/new（默认配置）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    {
        let environment = sessions_enabled[&id]
            .environment
            .as_ref()
            .expect("会话环境（生产装配路径）");
        super::session_lifecycle::after_new_response(&environment.cfg, &transport, &id).await;
    }
    let registry_enabled = cfg_enabled
        .session_manager
        .command_registry_for(&id)
        .expect("会话命令注册表");
    assert!(
        wait_command(
            &registry_enabled,
            "/core:e2e-project-skill",
            std::time::Duration::from_secs(15)
        )
        .await,
        "默认配置：发现完成后 /core:{{skill}} 必须出现在命令面（正例）；snapshot={:?}",
        registry_enabled
            .snapshot()
            .iter()
            .map(|e| e.fullname.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        sessions_enabled[&id]
            .frozen
            .as_ref()
            .expect("frozen")
            .system_prompt()
            .contains("# Skills"),
        "默认配置：13_skills 段落应在冻结 system prompt 中（差分对照的基线）"
    );

    // ── 关闭差分：同一夹具，只差 meta_harness.SkillsMiddleware=false ──
    let closed = SkillFixture::new(r#""01_intro":true,"SkillsMiddleware":false"#, false);
    let cfg = skill_server_config(&closed._tmp, closed.startup.clone()).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": closed.target.clone()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（链槽关闭）必须成功——关闭不阻塞创建");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    {
        let environment = sessions[&id]
            .environment
            .as_ref()
            .expect("会话环境（生产装配路径）");
        super::session_lifecycle::after_new_response(&environment.cfg, &transport, &id).await;
    }
    let registry = cfg
        .session_manager
        .command_registry_for(&id)
        .expect("会话命令注册表");
    // 发现落定信号 = `{server}:{skill}` MCP 面出现（与 core 投影同一批同步回写）。
    assert!(
        wait_command(
            &registry,
            "/workspace:e2e-project-skill",
            std::time::Duration::from_secs(15)
        )
        .await,
        "SkillsMiddleware 关闭不改变 MCP 发现面：workspace:{{skill}} 必须注册"
    );
    // core 条目一旦写入即持久（只能被下一次投影撤下），因此有界宽限后仍缺席
    // 即「从未写入」，不存在观察竞态。
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        registry.resolve("/core:e2e-project-skill").is_none(),
        "SkillsMiddleware 关闭 ⇒ /core:{{skill}} 不得注册（链槽关闭的配套半边）；snapshot={:?}",
        registry
            .snapshot()
            .iter()
            .map(|e| e.fullname.clone())
            .collect::<Vec<_>>()
    );

    // 关闭位不泄漏技能摘要进冻结 prompt：摘要数据仍非空（技能经 MCP 可得），
    // 但 13_skills 段随 disabled 集不收集、摘要无旁路渲染。
    let frozen = sessions[&id].frozen.as_ref().expect("frozen");
    assert!(
        !frozen.skill_summary().unwrap_or_default().is_empty(),
        "关的是宿主技能面：workspace 实例照常，冻结技能摘要数据必须非空"
    );
    assert!(
        !frozen.system_prompt().contains("# Skills"),
        "13_skills 段落随 disabled 集不收集（不得旁路渲染）"
    );
    assert!(
        !frozen.system_prompt().contains(PROJECT_SKILL),
        "冻结技能摘要不得进入 system prompt: {}",
        frozen.system_prompt()
    );
}

/// W5 关闭矩阵（指令面，差分对照）：同一夹具只差
/// `meta_harness.WorkspaceMiddleware=false`。
///
/// - 默认会话：冻结 system prompt **含**项目指令正文（经 P4 的
///   `peri-instruction://workspace/main` 资源读取，E15）；
/// - 实例关闭会话：冻结 prompt **不含**该哨兵——磁盘上的 `AGENTS.md` 仍在，
///   证明零磁盘兜底（X4/J5）。
#[tokio::test]
#[serial]
async fn closed_workspace_instance_makes_instructions_unavailable_without_disk_fallback() {
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // ── 默认（指令面开启） ──
    let open = SkillFixture::new(r#""01_intro":true"#, false);
    let open_cfg = skill_server_config(&open._tmp, open.startup.clone()).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": open.target.clone()}),
        &open_cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（指令面开启）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let frozen = sessions[&id].frozen.as_ref().expect("frozen");
    assert!(
        frozen
            .claude_md()
            .unwrap_or_default()
            .contains(PROJECT_INSTRUCTION_SENTINEL),
        "默认会话的冻结指令正文必须含哨兵（经 P4 的 peri-instruction 资源读取）"
    );
    assert!(
        workspace_resource_uris(&sessions, &id)
            .contains(&"peri-instruction://workspace/main".to_string()),
        "指令资源必须经 workspace 实例暴露"
    );
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &open_cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // ── 关闭实例（同一夹具，只差关闭位） ──
    let closed = SkillFixture::new(r#""01_intro":true,"WorkspaceMiddleware":false"#, false);
    let closed_cfg = skill_server_config(&closed._tmp, closed.startup.clone()).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": closed.target.clone()}),
        &closed_cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（实例关闭）必须成功——关闭不阻塞创建");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let frozen = sessions[&id].frozen.as_ref().expect("frozen");
    assert!(
        std::path::Path::new(&closed.target)
            .join("AGENTS.md")
            .is_file(),
        "零磁盘兜底的前提：哨兵文件仍在磁盘上"
    );
    assert!(
        !frozen
            .claude_md()
            .unwrap_or_default()
            .contains(PROJECT_INSTRUCTION_SENTINEL),
        "关闭 workspace ⇒ 指令不可得，且不得回落磁盘"
    );
    assert!(
        !frozen
            .system_prompt()
            .contains(PROJECT_INSTRUCTION_SENTINEL),
        "关闭会话的冻结 prompt 同样不得携带旧指令正文"
    );
    // 不误伤其他面：关闭位只摘指令/技能面，会话自身仍可创建并关闭。
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &closed_cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

/// 写一个 project agent 定义（`{cwd}/<root>/<id>.md`）。
///
/// frontmatter 的 description 作为哨兵：provider 把它逐字投影进资源
/// `description`，因此「清单里剩哪一版」可观察（URI 只含文件名 id，区分不出根）。
fn write_project_agent(cwd: &str, root: &str, id: &str, description: &str) {
    let dir = std::path::Path::new(cwd).join(root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.md")),
        format!("---\nname: {id}\ndescription: {description}\n---\n\nSentinel {description}.\n"),
    )
    .unwrap();
}

/// 失败诊断：按 URI 取会话池里 builtin `workspace` 资源的 description。
fn workspace_resource_description(
    sessions: &HashMap<String, SessionState>,
    id: &str,
    uri: &str,
) -> Option<String> {
    let environment = sessions[id].environment.as_ref()?;
    let pool = environment.cfg.mcp_pool.as_ref()?;
    let pool = pool
        .as_any()
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()?;
    pool.get_resources("workspace")
        .iter()
        .find(|resource| resource.uri == uri)
        .and_then(|resource| resource.description.clone())
}

/// W5 第二个 project agent 根（`{cwd}/agents`）的**宿主接线**证据 + 同 id 冲突
/// 的先到先得（生产装配：`session/new` → 资源清单）。
///
/// provider 侧只覆盖了「给两个 project 根时谁先到先得」的排序（`agents_test.rs`
/// 的 `two_project_roots_prefer_the_first_one_for_same_id`），宿主是否真的把
/// `{cwd}/agents` 接进输入没有任何断言——本例补这一条：
/// 1. 只存在于 `{cwd}/agents` 的 agent ⇒ 资源清单含
///    `agent://project/<id>/agent.md`（裸根真的进了链路）；
/// 2. 同 id 两处并存 ⇒ 该 id 只公开一次，且胜出者的 description 来自
///    `.claude/agents` 版——`scan_catalog` 对同 (scope, plugin, id) 先到先得，
///    宿主按 `.claude/agents` 先、`{cwd}/agents` 后的顺序给根。
#[tokio::test]
#[serial]
async fn bare_agents_root_is_wired_and_loses_same_id_to_the_claude_root() {
    /// 只存在于裸根（`{cwd}/agents`）的 agent。
    const BARE_ONLY: &str = "e2e-bare-root-only-agent";
    /// 只存在于 `.claude/agents` 的 agent（既有根的对照）。
    const CLAUDE_ONLY: &str = "e2e-claude-root-only-agent";
    /// 两个根同名的 agent（冲突探针）。
    const CONFLICT: &str = "e2e-conflict-agent";
    const CONFLICT_CLAUDE_DESC: &str = "E2E-CONFLICT-CLAUDE-ROOT-WINS";
    const CONFLICT_BARE_DESC: &str = "E2E-CONFLICT-BARE-ROOT-LOSES";

    let fixture = SkillFixture::new(r#""01_intro":true"#, false);
    write_project_agent(&fixture.target, "agents", BARE_ONLY, "E2E-BARE-ROOT-ONLY");
    write_project_agent(
        &fixture.target,
        ".claude/agents",
        CLAUDE_ONLY,
        "E2E-CLAUDE-ROOT-ONLY",
    );
    write_project_agent(
        &fixture.target,
        ".claude/agents",
        CONFLICT,
        CONFLICT_CLAUDE_DESC,
    );
    write_project_agent(&fixture.target, "agents", CONFLICT, CONFLICT_BARE_DESC);

    let cfg = skill_server_config(&fixture._tmp, fixture.startup.clone()).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": fixture.target.clone()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（agent 根已接线）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();

    let uris = workspace_resource_uris(&sessions, &id);
    assert!(
        uris.contains(&format!("agent://project/{BARE_ONLY}/agent.md")),
        "宿主必须接线 `{{cwd}}/agents`（仅存在于裸根的 agent 应进资源清单）: {uris:?}"
    );
    assert!(
        uris.contains(&format!("agent://project/{CLAUDE_ONLY}/agent.md")),
        "`.claude/agents` 根必须照常接线（对照）: {uris:?}"
    );

    let conflict_uri = format!("agent://project/{CONFLICT}/agent.md");
    assert_eq!(
        uris.iter().filter(|uri| **uri == conflict_uri).count(),
        1,
        "同 (scope, plugin, id) 先到先得：该 id 只公开一次: {uris:?}"
    );
    assert_eq!(
        workspace_resource_description(&sessions, &id, &conflict_uri).as_deref(),
        Some(CONFLICT_CLAUDE_DESC),
        "胜出者是先到的 `.claude/agents` 版（description = frontmatter 逐字投影）"
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
