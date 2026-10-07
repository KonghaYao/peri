//! `bind_agent_catalog_from_pool` 的定点绑定回归（P0：冻结渲染早于 turn 级 bind）。
//!
//! 正例走**真实线路夹具**（`AgentFaceFixture`：生产 handler + 生产池句柄），断言
//! 绑定后端口目录非空——即会话创建期的冻结渲染确实能拿到候选；同时钉住关闭集
//! 派生的本地面关闭位（与 turn 级 bind 同形）。反例只钉「不适用 ⇒ false 且不
//! panic」，不需要线路。

use std::collections::HashSet;
use std::sync::Arc;

use peri_acp_types::ports::{AgentCatalogPort, McpPoolPort};

use super::{bind_agent_catalog_from_pool, AgentCatalogProvider, NoopAgentCatalog};
use crate::mcp::agent_face_fixture::AgentFaceFixture;

/// 正例：池与端口都是本 crate 实现 ⇒ 绑定成立，且绑定后目录非空。
///
/// 同一条线路做**差分对照**：只差关闭集是否命中 `SubAgentMiddleware`——命中 ⇒
/// 本地面关闭 ⇒ 空目录（证明关闭位随本函数进入 registry，不是恒 false/恒 true）。
#[tokio::test]
async fn bind_agent_catalog_from_pool_exposes_catalog_and_honours_closure() {
    const AGENT: &str = "frozen-prompt-probe";
    const SESSION: &str = "frozen-prompt-probe-session";

    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join(format!("{AGENT}.md")),
        format!(
            "---\nname: {AGENT}\ndescription: Frozen prompt probe\nmodel: haiku\n---\n\nProbe.\n"
        ),
    )
    .unwrap();
    // 夹具在 `connect` 时快照 `resources/list`，定义必须先落盘。
    let fixture = AgentFaceFixture::connect(dir.path()).await;
    let pool: Arc<dyn McpPoolPort> = fixture.pool.clone();

    let open_port: Arc<dyn AgentCatalogPort> = Arc::new(AgentCatalogProvider::new());
    assert!(
        bind_agent_catalog_from_pool(&open_port, &pool, SESSION, &HashSet::new()),
        "池与端口都是本 crate 实现 ⇒ 绑定必须成立"
    );
    let ids: Vec<String> = open_port.catalog(false).into_iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![AGENT.to_string()],
        "绑定后目录必须非空——这是会话创建期冻结渲染的前提"
    );

    // 关闭集命中 ⇒ 本地面关闭（与 `assembly/preparation.rs` 的 turn 级 bind 同形）。
    let closed_port: Arc<dyn AgentCatalogPort> = Arc::new(AgentCatalogProvider::new());
    let mut disabled = HashSet::new();
    disabled.insert(crate::assembly::SUB_AGENT_FACE_CLOSED_KEY.to_string());
    assert!(
        bind_agent_catalog_from_pool(&closed_port, &pool, SESSION, &disabled),
        "关闭集不影响绑定成立与否，只决定本地面开关"
    );
    assert!(
        closed_port.catalog(false).is_empty(),
        "关闭集命中 ⇒ 本地面关闭 ⇒ 空目录（X4/J5：不得回落磁盘）"
    );
}

/// 反例：端口不是本 crate 实现 ⇒ 返回 `false` 且不 panic（不回落降级实例）。
///
/// 池用真实 `McpClientPool`（`new_pending` 即可，downcast 成功），确保拒绝来自
/// **端口**分支而不是池分支。
#[test]
fn bind_agent_catalog_from_pool_rejects_foreign_port() {
    let pool: Arc<dyn McpPoolPort> = Arc::new(crate::mcp::McpClientPool::new_pending());
    let foreign: Arc<dyn AgentCatalogPort> = Arc::new(NoopAgentCatalog);
    assert!(
        !bind_agent_catalog_from_pool(&foreign, &pool, "foreign-port-session", &HashSet::new()),
        "端口非 `AgentCatalogProvider` ⇒ 不适用，返回 false"
    );
    assert!(
        foreign.catalog(true).is_empty(),
        "不适用路径不得产生任何目录内容"
    );
}
