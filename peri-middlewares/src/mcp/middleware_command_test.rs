use super::*;

// ─── 命令面投影（决策 1：双注册表）───────────────────────────────────────

/// with_command_registry 装配 → before_agent 以 `{server}` 来源键置 Started；
/// 断连 → 命令面按前缀批量注销（removed_any → on_change）。
#[tokio::test]
async fn before_agent_command_registry_projection_and_disconnect() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");

    // 第一轮：命令面 Started（srv 来源登记；注册表无公开 sources 查询，
    // 以断连清理行为 + on_change 侧证接线生效）。
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Started 不触发"
    );

    // 断连：pool 条目移除 → 下轮投影按 srv 前缀清理 → on_change 恰一次
    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "断连移除应触发 on_change 恰一次"
    );
    assert!(
        cmd_reg.snapshot().is_empty(),
        "无条目注册（真实 peer 返回空清单）"
    );
}

/// with_command_registry 未装配（默认 new()）→ 命令面零动作（兼容既有行为）。
#[tokio::test]
async fn before_agent_without_command_registry_noop() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool)).with_skill_discovery(
        Some(Arc::new(McpSkillRegistry::new())),
        AgentCancellationToken::new(),
    );
    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&mw, &mut state).await.unwrap();

    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "命令面未装配：注册表零写入"
    );
}

/// 命令面等价断言（对齐 :431 断连清理用例，Phase 6 A5）：断连 →
/// `{server}:` 前缀条目从 snapshot 消失 + on_change 恰一次（决策 1：
/// server 名即词法首段域，无 `mcp:` 域前缀）。
///
/// 发现回写模拟说明：测试 handle 持有真实 rmcp peer（返回 skills:[]
/// 并完成合法空目录），非空条目经 `mcp_route_entries` 转换后手动
/// `mark_source_completed`——与 A3 生产回写同构；先 `wait_discovered` 让
/// spawn 的空回写落定再手动回写，避免空回写注销覆盖。
#[tokio::test]
async fn before_agent_command_registry_disconnect_removes_namespace_and_fires_on_change() {
    let pool = Arc::new(McpClientPool::new_empty());
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");

    // 连接 → 命令面 Started；发现任务（真实空清单 peer）空回写落定
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, "srv").await;

    // 发现完成回写（A3 转换点同构）：srv:hello 入投影
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "完成回写应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "完成回写后 snapshot 应含 srv:hello"
    );
    let before_disconnect = counter.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(before_disconnect, 1, "完成回写应触发 on_change 一次");

    // 断连：pool 条目移除 → 下轮投影按 srv 前缀批量注销 → on_change 恰一次
    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        !cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "断连后 srv:hello 应从 snapshot 消失"
    );
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        before_disconnect + 1,
        "断连注销应触发 on_change 恰一次"
    );
    assert_eq!(state.messages().len(), 0, "断连清理静默");
}

/// 会话预热（决策 B 扩展，审查会话生命周期）：`prewarm_discovery` 不装配
/// chain 即触发幂等发现——新会话（/clear）在首 turn 前命令面即可获得
/// 条目；重复预热（Started/Discovered 去重）零动作、不触发 on_change。
#[tokio::test]
async fn prewarm_discovery_triggers_idempotent_discovery() {
    let pool = Arc::new(McpClientPool::new_empty());
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));

    // 模拟 session/new 路径（无 middleware 实例）：预热 → 发现任务
    // （真实空清单 peer）空回写落定。
    prewarm_discovery(
        &pool,
        &reg,
        &cmd_reg,
        "sess-1",
        &AgentCancellationToken::new(),
    );
    wait_discovered(&reg, "srv").await;

    // 完成回写（A3 转换点同构）：srv:hello 入投影
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "完成回写应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "预热后命令面应含 srv:hello（无需 before_agent）"
    );
    let after_first = counter.load(std::sync::atomic::Ordering::SeqCst);

    // 重复预热幂等：已 Discovered → 不重扫、不触发 on_change。
    prewarm_discovery(
        &pool,
        &reg,
        &cmd_reg,
        "sess-1",
        &AgentCancellationToken::new(),
    );
    prewarm_discovery(
        &pool,
        &reg,
        &cmd_reg,
        "sess-1",
        &AgentCancellationToken::new(),
    );
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        after_first,
        "重复预热零动作"
    );
}

/// 连接完成事件（决策 B，session/new 窗口期）：notifier 挂接后，Connected
/// 状态变化即触发幂等发现——「装配时连接未完成」场景在首 turn 前即被
/// 补偿（无 chain、无 before_agent）。notify_tx=None 仅发现触发。
#[tokio::test]
async fn attach_connection_notifier_triggers_discovery_on_connected() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    attach_connection_notifier(
        &pool,
        Some(&reg),
        Some(&cmd_reg),
        &AgentCancellationToken::new(),
        None,
    );

    // 模拟 session/new 后连接完成：pool 初始化完成 + 状态变更广播
    // （record_status_change → notifier → run_ensure_discovery）。
    // 注意 record_status_change 要求 initialized + old 为 Some 且状态
    // 确实变化（client.rs:854-869），故插入 Connected 前先置旧态。
    pool.mark_initialized();
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    pool.record_status_change("srv", Some(&ClientStatus::Disconnected));
    wait_discovered(&reg, "srv").await;

    // 完成回写（A3 转换点同构）：连接事件补偿路径下命令面直接可得。
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "连接事件补偿应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "连接完成后面板即可路由 srv:hello（无需首 turn）"
    );
}

#[test]
fn test_connection_notifier_does_not_strongly_retain_pool() {
    let pool = Arc::new(McpClientPool::new_empty());
    let pool_weak = Arc::downgrade(&pool);
    let registry = Arc::new(McpSkillRegistry::new());
    let registry_weak = Arc::downgrade(&registry);
    let commands = Arc::new(CommandRegistry::new());
    attach_connection_notifier(
        &pool,
        Some(&registry),
        Some(&commands),
        &AgentCancellationToken::new(),
        None,
    );
    drop(registry);
    drop(commands);

    drop(pool);

    assert!(pool_weak.upgrade().is_none());
    assert!(registry_weak.upgrade().is_none());
}

/// 初始连接补发（决策 B 扩展）：`run_initialize` 收口时
/// `notify_initial_connections` 为每个 Connected server 补发一次连接
/// 通知——初始化期间的连接事件不经过 `record_status_change`
/// （`run_initialize` 直接插入 Connected handle），连接事件 notifier
/// 需靠收口补发驱动「刚进入、未说话」时的 skill 发现。
#[tokio::test]
async fn notify_initial_connections_triggers_discovery_on_startup() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    attach_connection_notifier(
        &pool,
        Some(&reg),
        Some(&cmd_reg),
        &AgentCancellationToken::new(),
        None,
    );

    // 模拟 run_initialize：直接插入 Connected handle（不调用
    // record_status_change——初始连接事件不产生通知）。
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    pool.notify_initial_connections();
    wait_discovered(&reg, "srv").await;

    // 完成回写（A3 转换点同构）：补发路径下命令面直接可得。
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "初始化补发应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "补发后面板即可路由 srv:hello（无需首 turn）"
    );
}

/// 重连顺序性（Phase 6 A5 验收核心）：连接 → 发现 → 注册（投影含
/// `demo:hello`）→ 断连（投影收缩）→ 重连（新 handle）→ 重扫完成前
/// 投影**不含**新条目（`Started → Discovered` 不占位）→ 完成回写（投影
/// 复现 + `resolve` 路由一致）；旧任务回写（旧 handle）被 ptr_eq 拒绝
/// （无 ABA）。
///
/// 驱动形态：连接 / 断连 / 重连经 `before_agent` 投影（`project_sources` →
/// `mark_source_started`），发现回写经 `mcp_route_entries` 转换后手动
/// `mark_source_completed`（测试 handle 无 peer，spawn 任务只能产出空
/// 条目）；每轮先 `wait_discovered` 让 spawn 空回写落定再手动回写。
#[tokio::test]
async fn before_agent_command_registry_reconnect_sequence_no_aba() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");

    // A3 转换点同构的回写载荷（skill 名剥 `mcp__demo__` 前缀 → demo:hello）
    let route_entries = || {
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "demo",
            &[SkillMetadata {
                name: "mcp__demo__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        )
    };
    let snapshot_has_hello =
        |reg: &CommandRegistry| reg.snapshot().iter().any(|e| e.fullname == "demo:hello");

    // 1) 连接 → 命令面 Started；发现任务（真实空清单 peer）空回写落定
    let h1 = insert_skill_handle(&pool, "demo", vec![]);
    let token1: HandleToken = h1.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, "demo").await;

    // 2) 发现完成回写 → 投影含 demo:hello；resolve 路由一致
    assert_eq!(
        cmd_reg.mark_source_completed("demo", token1.clone(), route_entries()),
        1,
        "首次完成回写应注册 1 条"
    );
    assert!(
        snapshot_has_hello(&cmd_reg),
        "完成回写后投影应含 demo:hello"
    );
    let r1 = cmd_reg.resolve("demo:hello").expect("resolve 应命中");
    assert_eq!(r1.entry.fullname, "demo:hello");
    assert_eq!(r1.entry.kind, CommandEntryKind::McpSkill);
    assert_eq!(r1.entry.description, "hello skill");
    assert_eq!(
        r1.entry.provenance,
        CommandProvenance {
            source: CommandSource::Mcp {
                server: "demo".into()
            },
            lifecycle: CommandLifecycle::Discovered,
        },
        "路由条目 provenance 应与 mcp_route_entries 产出一致"
    );
    assert_eq!(r1.args, "");

    // 3) 断连 → 投影收缩（demo:hello 从 snapshot 消失）
    pool.clients.write().remove("demo");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(!snapshot_has_hello(&cmd_reg), "断连后投影应收缩");

    // 4) 重连（新 Arc handle → token 变化）→ 重扫完成前投影不含新条目
    //    （Started → Discovered 不占位；current_thread：spawn 尚未调度）
    let h2 = insert_skill_handle(&pool, "demo", vec![]);
    let token2: HandleToken = h2.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        !Arc::ptr_eq(&token1, &token2),
        "重连 handle 必须新址（防 ABA 前提；旧 token 由测试持有保持分配存活）"
    );
    assert!(
        !snapshot_has_hello(&cmd_reg),
        "重扫完成前不得占位注册（Started → Discovered 不占位）"
    );

    // 5) 重扫（真实空清单 peer 空回写）落定后，新 handle 回写 → 投影复现 + 路由一致
    wait_discovered(&reg, "demo").await;
    assert_eq!(
        cmd_reg.mark_source_completed("demo", token2.clone(), route_entries()),
        1,
        "重连完成回写应注册 1 条"
    );
    assert!(
        snapshot_has_hello(&cmd_reg),
        "重连完成回写后投影应复现 demo:hello"
    );
    let r2 = cmd_reg
        .resolve("demo:hello")
        .expect("重连后 resolve 应命中");
    assert_eq!(r2.entry.fullname, "demo:hello");
    assert_eq!(r2.entry.kind, CommandEntryKind::McpSkill);

    // 6) 旧任务回写（旧 handle token1）被 ptr_eq 拒绝：不注册、不覆盖、不触发
    let before_stale = counter.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        cmd_reg.mark_source_completed("demo", token1, route_entries()),
        0,
        "旧 handle 回写应被 ptr_eq 拒绝（无 ABA）"
    );
    assert!(snapshot_has_hello(&cmd_reg), "旧回写不得清除/替换新条目");
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        before_stale,
        "旧任务回写不得触发 on_change"
    );
    assert_eq!(state.messages().len(), 0);
}

/// P1-1 回归：plugin 提供的 MCP server key（`plugin:p1:demosrv`，含冒号）
/// 命令面来源键统一为末段 `demosrv`（决策 1：与 fullname 首段同构）——
/// 断连按 `demosrv:` 前缀批量注销（幽灵条目不残留），重连复现无
/// Conflict（验收 :414/:415 在 plugin server 形态下成立）。
#[tokio::test]
async fn before_agent_command_registry_plugin_server_disconnect_reconnect() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");
    let server = "plugin:p1:demosrv";

    // 来源键派生断言：三处键（来源登记 / 注销前缀 / fullname 首段）必须
    // 同构（旧实现用 `mcp:plugin:p1:demosrv`，注销前缀匹配不到 fullname
    // `demosrv:beta` → 幽灵条目）。
    assert_eq!(
        crate::mcp::skill_discovery::mcp_source_key(server),
        "demosrv",
        "plugin server 来源键必须取末段（P1-1）"
    );
    let route_entries = || {
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            server,
            &[SkillMetadata {
                name: "mcp__plugin:p1:demosrv__beta".into(),
                aliases: Vec::new(),
                description: "beta skill".into(),
                ..SkillMetadata::default()
            }],
        )
    };
    assert_eq!(route_entries()[0].fullname, "demosrv:beta");
    let snapshot_has_beta =
        |reg: &CommandRegistry| reg.snapshot().iter().any(|e| e.fullname == "demosrv:beta");

    // 1) 连接 → 命令面以末段来源键置 Started；发现任务（真实空清单 peer）空回写落定
    let h1 = insert_skill_handle(&pool, server, vec![]);
    let token1: HandleToken = h1.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, server).await;

    // 2) 完成回写（A3 转换点同构，来源键 = demosrv）→ 投影含条目
    assert_eq!(
        cmd_reg.mark_source_completed(
            &crate::mcp::skill_discovery::mcp_source_key(server),
            token1.clone(),
            route_entries(),
        ),
        1,
        "完成回写应注册 1 条（末段来源键）"
    );
    assert!(
        snapshot_has_beta(&cmd_reg),
        "完成回写后投影应含 demosrv:beta"
    );

    // 3) 断连 → 按 demosrv: 前缀批量注销，幽灵条目不残留
    pool.clients.write().remove(server);
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        !snapshot_has_beta(&cmd_reg),
        "断连后 demosrv:beta 必须收缩（P1-1 幽灵条目回归）"
    );

    // 4) 重连（新 Arc handle → token 变化）→ 重扫落定 → 新 token 回写复现
    //    （幽灵条目残留时同键重注册 → Conflict 纯拒绝 → added=0）
    let h2 = insert_skill_handle(&pool, server, vec![]);
    let token2: HandleToken = h2.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, server).await;
    assert_eq!(
        cmd_reg.mark_source_completed(
            &crate::mcp::skill_discovery::mcp_source_key(server),
            token2.clone(),
            route_entries(),
        ),
        1,
        "重连完成回写应注册 1 条（幽灵条目残留时此处 Conflict → 0）"
    );
    assert!(snapshot_has_beta(&cmd_reg), "重连后投影复现 demosrv:beta");
    let r = cmd_reg
        .resolve("demosrv:beta")
        .expect("重连后 resolve 应命中");
    assert_eq!(r.entry.kind, CommandEntryKind::McpSkill);
    assert_eq!(state.messages().len(), 0, "断连/重连清理静默");
}
