//! W4b/F6 收口的 `core:{skill}` 投影差分：**宿主技能面关闭位**
//! （`BuiltinInstanceContext.skills_face_closed`）的投影函数级与管道级证据。
//!
//! 位的事实源 = 宿主装配从同一份 `disabled_middlewares` 派生的
//! `"SkillsMiddleware" ∈ disabled`（与 A24 `closed` 同源）；语义 = 宿主技能面
//! （`core:{skill}` 裸名命令投影）关闭，**不是**实例关闭。
//!
//! 覆盖面：
//! 1. [`core_projection_registers_builtin_system_skills_with_aliases`]：位为假 ⇒
//!    系统来源（`ConfigSource::Builtin` 经 `mark_system_origins` 标注）的技能注册为
//!    `core:{bare}`（含 frontmatter `aliases` 派生别名）；
//! 2. [`core_projection_withdraws_when_skills_face_closed_and_keeps_mcp_face`]：位为真 ⇒
//!    既有 `core:` 条目整体撤下，且 `{server}:{skill}` MCP 发现面不受该位影响；
//! 3. [`before_agent_core_projection_follows_pool_context_skills_face_bit`]：管道级
//!    （`McpMiddleware::before_agent` → `run_ensure_discovery`）从 pool 注入的上下文
//!    一次读出该位并驱动同一差分（正例：路由随发现出现在面板；关闭：同批撤下）。
//!
//! 落点说明（STD-SIZE-001）：`skill_discovery_test.rs` 已超限（存量），本文件承载
//! F6 投影路径的聚焦用例（此前 `project_core_skill_commands` / `core_route_entries`
//! 无任何测试调用）。

use std::collections::BTreeSet;
use std::sync::Arc;

use peri_acp_types::command::command_route::{CommandEntryKind, CommandSource};
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::skills::SkillMetadata;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::middleware::r#trait::Middleware;

use crate::mcp::builtin::context::BuiltinInstanceContext;
use crate::mcp::client::{ClientStatus, McpClientHandle, McpClientPool, OAuthStatus};
use crate::mcp::middleware::McpMiddleware;

use super::{mark_system_origins, mcp_route_entries, project_core_skill_commands};

/// builtin 来源（`ConfigSource::Builtin`）的连接句柄：`mark_system_origins` 的
/// 判定事实是「连接事实」而不是名字自称，因此夹具必须以真实标记构造。
fn builtin_handle(name: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: Some(ConfigSource::Builtin {
            instance: name.to_string(),
        }),
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

/// `workspace` 系统来源的一条技能（含 frontmatter `aliases`）。
fn workspace_skill() -> SkillMetadata {
    SkillMetadata {
        name: "mcp__workspace__hello".into(),
        description: "hello skill".into(),
        frontmatter: Some(
            serde_json::json!({ "aliases": ["hi", "greet"] })
                .as_object()
                .expect("对象字面量")
                .clone(),
        ),
        ..SkillMetadata::default()
    }
}

/// 把「`workspace` 为系统来源 + 发现完成」的 registry 状态播种好（与
/// `run_discovery_with_cache` 完成回写的状态同形；句柄身份用于防 ABA 的
/// `Arc::ptr_eq` 判定，因此**同一份** `Arc` 必须回填给 registry）。
fn seed_system_skills(
    registry: &Arc<McpSkillRegistry>,
    handle: &Arc<McpClientHandle>,
) -> HandleToken {
    let token: HandleToken = Arc::clone(handle) as HandleToken;
    registry.mark_discovery_started("workspace", token.clone());
    registry.mark_discovery_completed("workspace", token.clone(), vec![workspace_skill()]);
    mark_system_origins(registry, std::slice::from_ref(handle), &BTreeSet::new());
    token
}

/// 投影函数级正例：位为假 ⇒ 系统来源技能注册为 `core:{bare}`，
/// frontmatter `aliases` 派生为命令别名，裸名 `/hello` 直接命中。
#[test]
fn core_projection_registers_builtin_system_skills_with_aliases() {
    let registry = Arc::new(McpSkillRegistry::new());
    let command_registry = Arc::new(CommandRegistry::new());
    let handle = builtin_handle("workspace");
    seed_system_skills(&registry, &handle);

    project_core_skill_commands(&Some(Arc::clone(&command_registry)), &registry, false);

    let entry = command_registry
        .resolve("/core:hello")
        .expect("位为假：core 裸名命令必须注册");
    assert_eq!(entry.entry.kind, CommandEntryKind::Skill);
    assert_eq!(entry.entry.provenance.source, CommandSource::Core);
    assert_eq!(
        entry.entry.aliases,
        vec!["hi".to_string(), "greet".to_string()],
        "frontmatter aliases 必须派生为命令别名（X3：wire 不改写，宿主派生）"
    );
    assert_eq!(
        command_registry
            .resolve("/hello")
            .expect("裸名 /hello 命中同一入口")
            .entry
            .fullname,
        "core:hello"
    );
}

/// 投影函数级关闭差分：位为真 ⇒ 既有 `core:` 条目同批撤下（`reconcile` 撤旧），
/// 而 `{server}:{skill}` MCP 发现面（不归该位）逐条保留。
#[test]
fn core_projection_withdraws_when_skills_face_closed_and_keeps_mcp_face() {
    let registry = Arc::new(McpSkillRegistry::new());
    let command_registry = Arc::new(CommandRegistry::new());
    let handle = builtin_handle("workspace");
    let token = seed_system_skills(&registry, &handle);

    // MCP 发现面（`{server}:{skill}`）经同一完成回写注册：与本位无关。
    command_registry.mark_source_started("workspace", token.clone());
    assert_eq!(
        command_registry.mark_source_completed(
            "workspace",
            token,
            mcp_route_entries(&registry, "workspace", &registry.skills_of("workspace")),
        ),
        1,
        "mcp 面完成回写注册 1 条"
    );

    // 位为假：core 面注册。
    project_core_skill_commands(&Some(Arc::clone(&command_registry)), &registry, false);
    assert!(
        command_registry.resolve("/core:hello").is_some(),
        "前置：core 面已注册（关闭差分才有可撤下的对象）"
    );

    // 位为真：core 面整体撤下。
    project_core_skill_commands(&Some(Arc::clone(&command_registry)), &registry, true);
    assert!(
        command_registry.resolve("/core:hello").is_none(),
        "位为真：core 裸名命令必须撤下"
    );
    assert!(
        command_registry.resolve("/hello").is_none(),
        "位为真：裸名也不得残留（reconcile 撤旧 + 无新条目）"
    );
    assert!(
        command_registry
            .snapshot()
            .iter()
            .all(|entry| !(entry.kind == CommandEntryKind::Skill
                && matches!(entry.provenance.source, CommandSource::Core))),
        "位为真：不得残留任何 core 域技能条目"
    );
    assert!(
        command_registry.resolve("/workspace:hello").is_some(),
        "`{{server}}:{{skill}}` MCP 发现面不归该位治理，必须保留"
    );
}

/// 管道级差分（生产挂点）：`before_agent` → `run_ensure_discovery` 从 pool 注入
/// 的 `BuiltinInstanceContext` 一次读出该位——位为假时正例（路由随发现出现在
/// 面板），位为真时同批撤下。两阶段用同一 registry / 命令注册表 / 句柄，只差
/// pool 上下文的位，因此差分只可能来自该位。
#[tokio::test]
async fn before_agent_core_projection_follows_pool_context_skills_face_bit() {
    let registry = Arc::new(McpSkillRegistry::new());
    let command_registry = Arc::new(CommandRegistry::new());
    let handle = builtin_handle("workspace");
    seed_system_skills(&registry, &handle);

    // 阶段 1：位为假（宿主未关闭 SkillsMiddleware）。
    let open_pool = Arc::new(McpClientPool::new_empty());
    open_pool
        .clients
        .write()
        .insert("workspace".to_string(), Arc::clone(&handle));
    open_pool
        .set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new("/tmp").with_skills_face_closed(false),
        ))
        .expect("首次注入");
    let open_mw = McpMiddleware::new(Arc::clone(&open_pool))
        .with_skill_discovery(Some(Arc::clone(&registry)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&command_registry)));
    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    Middleware::before_agent(&open_mw, &mut state)
        .await
        .expect("before_agent");
    assert!(
        command_registry.resolve("/core:hello").is_some(),
        "位为假：发现管线必须投影 core 裸名命令（正例）"
    );

    // 阶段 2：位为真（同一批事实，只差 pool 上下文）。
    let closed_pool = Arc::new(McpClientPool::new_empty());
    closed_pool
        .clients
        .write()
        .insert("workspace".to_string(), Arc::clone(&handle));
    closed_pool
        .set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new("/tmp").with_skills_face_closed(true),
        ))
        .expect("首次注入");
    let closed_mw = McpMiddleware::new(Arc::clone(&closed_pool))
        .with_skill_discovery(Some(Arc::clone(&registry)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&command_registry)));
    Middleware::before_agent(&closed_mw, &mut state)
        .await
        .expect("before_agent");
    assert!(
        command_registry.resolve("/core:hello").is_none(),
        "位为真：发现管线必须撤下 core 裸名命令（关闭差分）"
    );
    assert_eq!(state.messages().len(), 0, "投影静默：无消息推送");
}
