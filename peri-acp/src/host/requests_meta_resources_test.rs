//! J6 / W4a：段落覆盖（`peri-meta://workspace/{section_id}`）经**生产装配路径**进入
//! frozen system prompt 的端到端证据（`requests_test.rs` 的子模块；从
//! `requests_workspace_cases_test.rs` 按职责拆出，该文件同时回到 STD-SIZE-001 限内）。
//!
//! 两个用例是**差分对照**：同一夹具（文档在盘 + `01_intro` 启用 + 同名正文哨兵），
//! 只差 builtin `workspace` 实例的关闭位（`meta_harness.WorkspaceMiddleware`）：
//! - [`new_session_meta_override_unavailable_keeps_builtin_and_never_reads_disk`]：
//!   实例关闭 ⇒ 覆盖不可得，保持内置段落且**不回落磁盘**（X8 / ARC-CAPABILITY-CLOSURE-001）；
//! - [`new_session_meta_override_flows_through_builtin_workspace_resources`]：
//!   实例可用 ⇒ 覆盖正文逐字进入冻结 system prompt（J6 全链闭合）。
//!
//! 两条合起来即「宿主侧零 `.peri/meta` 文件系统读取」的对照证据：同一份磁盘文件在资源面
//! 可用时生效、在资源面不可用时**完全不生效**，说明正文只可能经 MCP 资源通道到达，而不是
//! 宿主自己读盘的结果（宿主侧 scanner 已删除，读盘点为零）。
//!
//! 装配路径（两条用例相同，均为生产构造点）：`session/new` → 会话环境装配
//! （`SessionEnvironment::assemble_with_frozen`，资源面输入随 `HostAssemblyInput` 一次注入）
//! → `assemble_server_config_with_mcp_profile` 把输入放进 `BuiltinInstanceContext` →
//! `McpClientPool::set_builtin_instance_context`（早于 `run_initialize`）→ builtin spawn →
//! `dispatch::builtin_server_handler` 的 `workspace` arm 调
//! `WorkspaceMcpServer::with_resources` → 冻结期 P4 经真实 MCP 连接 `resources/list` +
//! `resources/read`。**没有测试专用构造器**参与这条链。

use super::*;
use peri_acp_types::ports::McpPoolPort as _;

/// 覆盖段落 ID（∈ 契约 `SECTION_IDS`）。
const META_SECTION: &str = "01_intro";
/// 覆盖资源 URI（provider 冻结形状：authority = `workspace`）。
const META_URI: &str = "peri-meta://workspace/01_intro";
/// 正文哨兵：含内部换行，用于逐字比对（证明 provider 字节不被 trim）。
const META_BODY: &str = "PERI-META-E2E-OVERRIDE-L1\nPERI-META-E2E-OVERRIDE-L2";

/// 夹具：`startup`（装配起点）与 `target`（会话 cwd，含 `.peri/meta` 与 settings）。
///
/// `target` 的项目 settings **自带 provider**：会话 cwd 与 `startup_cwd` 不同目录时，
/// 准备路径按「异目录只读一次」在会话目录重解析配置（`PreparedConfiguration`），
/// 自带 provider 让该解析不依赖进程级 env/HOME 残留（同批用例可独立运行）。
/// `meta_harness` 由调用方给定——本模块两个用例只差关闭位这一项。
fn meta_fixture(meta_harness: &str) -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir_all(target.join(".peri/meta")).unwrap();
    std::fs::write(
        target.join(".peri/meta").join(format!("{META_SECTION}.md")),
        META_BODY,
    )
    .unwrap();
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
async fn meta_server_config(tmp: &tempfile::TempDir, startup_cwd: String) -> AcpServerConfig {
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

/// 失败诊断（只在断言失败路径上调用）：workspace 句柄的现状。
///
/// `PERI_MCP_BUILTIN` 是**进程级** env（builtin 注入关闭的用例经 `#[serial]` 守卫改它），
/// 本文件两个用例因此也标 `#[serial]`；若将来仍有别的进程级事实摘掉 builtin 实例，
/// 这条诊断让失败可归因，而不是只看到「overrides 为空」。
fn workspace_client_state(
    environment: Option<&Arc<crate::host::workspace::SessionEnvironment>>,
) -> String {
    let Some(environment) = environment else {
        return "无执行环境".to_owned();
    };
    let Some(pool) = environment.cfg.mcp_pool.as_ref() else {
        return "无 MCP 池".to_owned();
    };
    let Some(pool) = pool
        .as_any()
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
    else {
        return "池不是生产 McpClientPool".to_owned();
    };
    match pool.get_client("workspace") {
        Some(handle) => format!(
            "status={:?}, resources={}",
            handle.status,
            handle.resources.len()
        ),
        None => format!("句柄未出现（initPhase={}）", pool.snapshot()["initPhase"]),
    }
}

/// J6/W4a：覆盖正文本就存在的全链——`session/new` 的冻结 system prompt 逐字含覆盖正文。
///
/// 证据分三层（都取自生产路径）：
/// 1. `section_overrides` 与磁盘文件**字节相等**（provider 读到什么就冻结什么）；
/// 2. 冻结 `system_prompt()` 逐字含该正文且**恰出现一次**（无重复注入 / 无双源同名）；
/// 3. 会话池里真实 builtin `workspace` 句柄的资源清单（providers 侧
///    `resources/list` 响应的投影）含 `peri-meta://workspace/01_intro` 与既有
///    `workspace://git/ref`，且**不含** `skill://`（W4a 只接 meta 面的域口径）。
///
/// `#[serial]`：builtin 注入开关是进程级 env（`PERI_MCP_BUILTIN`，由注入关闭的串行用例
/// 改写），本用例必须有 builtin 实例在场，故与那些用例互斥执行。
#[tokio::test]
#[serial]
async fn new_session_meta_override_flows_through_builtin_workspace_resources() {
    let (tmp, startup, target) = meta_fixture(r#""01_intro":true"#);
    let cfg = meta_server_config(&tmp, startup).await;
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
    .expect("session/new（覆盖可得）必须成功");

    let id = created["sessionId"].as_str().unwrap();
    let frozen = sessions[id].frozen.as_ref().expect("创建必须发布 frozen");
    let override_body = frozen
        .meta_harness()
        .section_overrides
        .get(META_SECTION)
        .unwrap_or_else(|| {
            panic!(
                "覆盖必须经资源面进入 frozen（W4a 已接线）：overrides={:?}；workspace 句柄={}",
                frozen
                    .meta_harness()
                    .section_overrides
                    .keys()
                    .collect::<Vec<_>>(),
                workspace_client_state(sessions[id].environment.as_ref()),
            )
        });
    assert_eq!(
        override_body.as_ref(),
        META_BODY,
        "覆盖正文必须与 provider 读到的磁盘字节逐字一致（不 trim）"
    );
    let prompt = frozen.system_prompt();
    assert!(
        prompt.contains(META_BODY),
        "覆盖正文必须逐字出现在冻结 system prompt"
    );
    assert_eq!(
        prompt.matches(META_BODY).count(),
        1,
        "覆盖正文在 system prompt 中必须恰出现一次（不得双源/重复注入）"
    );

    // provider 侧可见请求：连接期 `resources/list` 的响应投影（生产句柄）。
    let environment = sessions[id]
        .environment
        .as_ref()
        .expect("会话必须持有执行环境");
    let pool = environment
        .cfg
        .mcp_pool
        .as_ref()
        .expect("会话必须有 MCP 池");
    let pool = pool
        .as_any()
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
        .expect("会话池是生产 McpClientPool");
    let uris: Vec<String> = pool
        .get_resources("workspace")
        .iter()
        .map(|resource| resource.uri.clone())
        .collect();
    assert!(
        uris.contains(&META_URI.to_string()),
        "provider 的 resources/list 必须暴露覆盖资源：{uris:?}"
    );
    assert!(
        uris.contains(&"workspace://git/ref".to_string()),
        "既有 git ref 资源面必须保留：{uris:?}"
    );
    assert!(
        !uris.iter().any(|uri| uri.starts_with("skill://")),
        "W4a 只接 meta 面：技能面必须保持关闭：{uris:?}"
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

/// J6/X8：覆盖不可得时保持内置段落，且**不回落磁盘读 `.peri/meta`**。
///
/// W4a 前该不可得性来自「生产装配未接资源 provider」；接线后同一场景改由**关闭集**
/// 构造（`meta_harness.WorkspaceMiddleware = false`，A24 非物理关闭 +
/// ARC-CAPABILITY-CLOSURE-001）：文档仍在盘上、section 仍启用，覆盖照样不可得 ⇒
/// 保持内置（逐字），且 prompt 不含磁盘哨兵。
///
/// 这也是「宿主侧零 `.peri/meta` 文件系统读取」的**对照臂**：即便关闭实例，宿主也没有
/// 「读盘兜底」这条路可走（scanner 已删除）。
///
/// `#[serial]`：与同批用例同一理由（进程级 builtin 注入开关）。
#[tokio::test]
#[serial]
async fn new_session_meta_override_unavailable_keeps_builtin_and_never_reads_disk() {
    let (tmp, startup, target) = meta_fixture(r#""01_intro":true,"WorkspaceMiddleware":false"#);
    let cfg = meta_server_config(&tmp, startup).await;
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
    .expect("session/new（覆盖不可得）必须成功——X8 不阻塞创建");

    let id = created["sessionId"].as_str().unwrap();
    let frozen = sessions[id].frozen.as_ref().expect("创建必须发布 frozen");
    assert!(
        frozen.meta_harness().section_overrides.is_empty(),
        "覆盖不可得时必须保持内置（X8）"
    );
    assert!(
        !frozen.system_prompt().contains(META_BODY),
        "宿主不得回落到磁盘读 `.peri/meta`（X8 零 FS 兜底；实例关闭 ⇒ 覆盖不可得）"
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
