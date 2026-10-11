//! T4–T7b：真实 `workspace` MCP handler 的线路证据（`builtin_subscription_wire_test.rs`
//! 的子模块——拆分原因见父文件尾部注释与 STD-SIZE-001）。
//!
//! 与父文件探针夹具的分工：探针用替身 handler 验证订阅推送面；本模块用**真实**
//! `WorkspaceMcpServer`（生产 handler、7 个真实工具、git 采样）经**生产**链路装配
//! （`spawn_builtin_transport_with_handler`）与**生产** client 握手段，验证
//! `workspace://git/ref` 的对外契约与回传闭环。

use super::*;
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedPayload, SessionInbox,
};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderCategory, ReminderDelivery, ReminderSeverity,
};
use peri_mcp_workspace::WorkspaceMcpServer;
use rmcp::model::{
    CallToolRequestParams, ErrorCode, GetMeta, ReadResourceRequestParams, ServerNotification,
};
use rmcp::service::ServiceError;
use std::{path::Path, process::Command as StdCommand};

use crate::mcp::builtin::context::BuiltinInstanceContext;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::{ClientStatus, McpClientHandle, McpClientPool};

// ─── T4–T6：workspace 真实 handler 的线路证据 ────────────────────────────────
//
// 与父文件探针夹具的分工：探针用替身 handler 验证订阅推送面；本节用**真实**
// `WorkspaceMcpServer`（生产 handler、7 个真实工具、git 采样）经**生产**链路装配
// （`spawn_builtin_transport_with_handler`）与**生产** client 握手段，验证
// `workspace://git/ref` 的对外契约与回传闭环。

/// 一条已握手的 workspace 链路（真实 handler + 生产链路装配 + 生产 client 握手）。
struct WorkspaceLink {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
}

impl WorkspaceLink {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：关闭 client 半边后有界收敛 server task（**断言收敛**，不接受 abort）。
    ///
    /// 前提（调用方负责）：关闭前必须释放所有会保活 client 传输的引用——活跃订阅句柄
    /// （`Subscription`，持有 `Peer` 克隆且在服务端留下一笔在途 `listen` 请求）与
    /// `McpClientHandle.peer`。生产路径同样这么做：`remove_server` / `set_disabled` 先停
    /// 订阅 task 再关连接，`shutdown()` 先把每个 handle 的 `peer` 置 `None` 再关 service
    /// （`lifecycle.rs:342-352`）。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let outcome = self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        assert!(
            !matches!(
                outcome.server,
                crate::mcp::builtin::runtime::BuiltinServerExit::AbortedAfterTimeout
            ),
            "server task 必须收敛，不得 abort：{outcome:?}"
        );
    }
}

/// 连接一个 workspace 实例（节流窗口归零：线路用例需要在一次会话内观察两次采样）。
async fn connect_workspace(cwd: &str) -> WorkspaceLink {
    let transport = spawn_builtin_transport_with_handler(
        "workspace",
        WorkspaceMcpServer::new(cwd, None).with_git_throttle(Duration::ZERO),
    );
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时（同进程链路）")
        .expect("builtin 握手不得失败");
    WorkspaceLink {
        service,
        supervisor,
    }
}

/// git ref 订阅 filter（生产默认订阅的同一 URI）。
fn git_filter() -> SubscriptionFilter {
    SubscriptionFilter::builder()
        .resource_subscriptions([SPIKE_URI])
        .build()
}

/// 初始化一个 git 仓库（`git init -b main` + user.* + 首个 commit），返回临时目录与 cwd。
fn git_workspace() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("临时目录夹具必须可创建");
    let path = dir.path();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
    ] {
        let status = StdCommand::new("git")
            .args(&args)
            .current_dir(path)
            .output()
            .expect("git 必须可用");
        assert!(status.status.success(), "git {args:?} 必须成功");
    }
    std::fs::write(path.join("README.md"), "hi").unwrap();
    for args in [vec!["add", "README.md"], vec!["commit", "-m", "init"]] {
        let status = StdCommand::new("git")
            .args(&args)
            .current_dir(path)
            .output()
            .expect("git 必须可用");
        assert!(status.status.success(), "git {args:?} 必须成功");
    }
    let cwd = path.canonicalize().expect("夹具目录必须可规范化");
    (dir, cwd.to_string_lossy().to_string())
}

/// 追加一次 commit（制造 HEAD 变化）。
fn commit_again(repo: &Path) {
    std::fs::write(repo.join("README.md"), "changed").unwrap();
    let status = StdCommand::new("git")
        .args(["commit", "-am", "second"])
        .current_dir(repo)
        .output()
        .expect("git 必须可用");
    assert!(status.status.success(), "第二次 commit 必须成功");
}

/// `tools/call`（成功形态）——触发门要求 `is_error != Some(true)`。
async fn call_tool_ok(peer: &Peer<RoleClient>, name: &str, arguments: serde_json::Value) {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = arguments.as_object().cloned();
    let response = peer
        .call_tool_once(request)
        .await
        .unwrap_or_else(|error| panic!("{name} 必须可经线路调用：{error}"));
    match response {
        rmcp::model::CallToolResponse::Complete(result) => assert_ne!(
            result.is_error,
            Some(true),
            "{name} 必须成功返回（触发门的必要条件）"
        ),
        other => panic!("{name} 期望 Complete 结果，实际 {other:?}"),
    }
}

/// `resources/read` 的正文（唯一内容块的文本）。
async fn read_body(peer: &Peer<RoleClient>, uri: &str) -> String {
    let result = peer
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .unwrap_or_else(|error| panic!("resources/read({uri}) 必须成功：{error}"));
    result
        .contents
        .iter()
        .find_map(|content| match content {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("resources/read({uri}) 必须返回文本内容"))
}

/// 有界等待资源正文满足条件（采样是后台 spawn，不能靠固定 sleep 断言）。
async fn wait_for_body(
    peer: &Peer<RoleClient>,
    uri: &str,
    label: &str,
    pred: impl Fn(&str) -> bool,
) {
    let observed = timeout(OBSERVE_TIMEOUT, async {
        loop {
            if pred(&read_body(peer, uri).await) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(observed.is_ok(), "等待超时：{label}");
}

// ─── T4：能力位 / resources/list / read / filter 交集 ────────────────────────

#[tokio::test]
async fn t4_workspace_exposes_git_ref_resource_and_subscribe_capability() {
    let _env = peri_mcp_common::process_env::lock().expect("process env lock");
    let (_dir, cwd) = git_workspace();
    let link = connect_workspace(&cwd).await;
    let peer = link.peer();

    let info = peer
        .peer_info()
        .expect("modern 握手后 peer_info 必须是 Some");
    let resources_caps = info
        .capabilities
        .resources
        .as_ref()
        .expect("必须声明 resources 能力（否则 resources/* 不可达）");
    assert_eq!(
        resources_caps.subscribe,
        Some(true),
        "必须声明 resources.subscribe，否则 SDK 会把订阅 filter 求交为空"
    );
    assert_ne!(
        resources_caps.list_changed,
        Some(true),
        "git ref 资源是固定单条，不声明 list_changed"
    );

    let resources = peer
        .list_all_resources()
        .await
        .expect("resources/list 必须成功");
    assert_eq!(resources.len(), 1, "资源面恰一条 git ref");
    assert_eq!(resources[0].uri, SPIKE_URI);
    assert_eq!(resources[0].mime_type.as_deref(), Some("text/plain"));

    // 命中：未采样时的固定正文。
    assert_eq!(
        read_body(&peer, SPIKE_URI).await,
        UNSAMPLED_RESOURCE_TEXT,
        "未采样时的资源正文必须是固定文本"
    );

    // 未命中：-32602。
    let error = peer
        .read_resource(ReadResourceRequestParams::new("workspace://not-a-resource"))
        .await
        .expect_err("未知资源必须失败");
    match error {
        ServiceError::McpError(error) => assert_eq!(
            error.code,
            ErrorCode::INVALID_PARAMS,
            "未知资源按协议回 -32602"
        ),
        other => panic!("未知资源期望 McpError，实际 {other:?}"),
    }

    // filter 交集语义：请求两个 URI，ack 只保留 git ref（其余类别未声明 ⇒ 不 ack）。
    let requested = SubscriptionFilter::builder()
        .resource_subscriptions([SPIKE_URI, "workspace://not-a-resource"])
        .build();
    let subscription = peer.listen(requested).await.expect("listen 必须成功");
    assert_eq!(
        subscription.acknowledged().resource_subscriptions,
        Some(vec![SPIKE_URI.to_string()]),
        "ack 必须只保留服务端声明的 git ref 资源"
    );
    drop(subscription);

    link.shutdown().await;
}

// ─── T5：工具调用 → commit → 通知 → 回读 notice 正文 ─────────────────────────

#[tokio::test]
async fn t5_commit_after_tool_call_pushes_update_and_read_returns_notice() {
    let _env = peri_mcp_common::process_env::lock().expect("process env lock");
    let (dir, cwd) = git_workspace();
    let link = connect_workspace(&cwd).await;
    let peer = link.peer();
    let mut subscription = peer.listen(git_filter()).await.expect("listen 必须成功");

    // 首次工具调用：建立基线（快照正文 = 无变化形态），不产生通知。
    call_tool_ok(
        &peer,
        "Read",
        serde_json::json!({ "file_path": "README.md" }),
    )
    .await;
    wait_for_body(&peer, SPIKE_URI, "首采样完成", |body| {
        body.contains("Repository ref snapshot:")
    })
    .await;

    // HEAD 变化后再次调用工具：采样发现变化 ⇒ 推送 resource updated。
    commit_again(dir.path());
    call_tool_ok(
        &peer,
        "Read",
        serde_json::json!({ "file_path": "README.md" }),
    )
    .await;

    let notification = timeout(OBSERVE_TIMEOUT, subscription.next())
        .await
        .expect("git ref 变化必须在期限内推送")
        .expect("订阅流不得出错")
        .expect("必须收到一条通知");
    match notification {
        ServerNotification::ResourceUpdatedNotification(ref update) => {
            assert_eq!(update.params.uri, SPIKE_URI);
            assert_eq!(
                notification
                    .get_meta()
                    .get(peri_acp_types::mcp::MCP_MESSAGE_KIND_META_KEY)
                    .and_then(serde_json::Value::as_str),
                Some("info"),
            );
            assert_eq!(
                notification
                    .get_meta()
                    .subscription_id()
                    .map(|id| id.to_string()),
                Some(subscription.id().to_string()),
                "通知必须携带订阅 id 供路由"
            );
        }
        other => panic!("期望 ResourceUpdatedNotification，实际 {other:?}"),
    }

    // 随后 resources/read 返回变化 notice 正文（提醒正文的事实源）。
    let body = read_body(&peer, SPIKE_URI).await;
    assert!(
        body.contains("[Git watch] Repository ref changed since the last sample:"),
        "notice 标题必须逐字一致：{body}"
    );
    assert!(body.contains("- HEAD:"), "HEAD 变化必须出现：{body}");

    // 先释放订阅（取消在途 listen），server task 才能真正收敛。
    drop(subscription);
    drop(peer);
    link.shutdown().await;
}

// ─── T6：会话送达（Info 不唤醒 / 字段逐一致）与关闭面隔离 ─────────────────────

#[tokio::test]
async fn t6_session_delivery_is_info_and_closure_skips_subscription() {
    let _env = peri_mcp_common::process_env::lock().expect("process env lock");
    let (dir, cwd) = git_workspace();
    let (owner, spawner) = crate::mcp::McpTaskOwner::new();
    let mut pool = McpClientPool::new_pending_with_spawner(spawner);
    pool.resource_cache = crate::mcp::resource_cache::McpResourceCache::isolated_for_test();
    let pool = Arc::new(pool);

    // 会话 inbox（生产口径：SessionInbox + MessageQueue）。
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    pool.register_inbox("session-1", inbox.handle());

    let link = connect_workspace(&cwd).await;
    let peer = link.peer();
    let handle = Arc::new(McpClientHandle {
        name: "workspace".to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(peer.clone()),
        tools: peer.list_all_tools().await.expect("tools/list 必须成功"),
        resources: peer
            .list_all_resources()
            .await
            .expect("resources/list 必须成功"),
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: Some(peri_acp_types::plugin::ConfigSource::Builtin {
            instance: "workspace".to_string(),
        }),
        url: None,
        skills_capable: false,
    });
    pool.clients.write().insert("workspace".to_string(), handle);
    let subscription = peer.listen(git_filter()).await.expect("listen 必须成功");
    pool.spawn_subscription_loop("workspace", subscription)
        .await;

    // 基线采样 → commit → 再采样 → 通知 → 会话 inbox 收到 Info 提醒。
    call_tool_ok(
        &peer,
        "Read",
        serde_json::json!({ "file_path": "README.md" }),
    )
    .await;
    wait_for_body(&peer, SPIKE_URI, "首采样完成", |body| {
        body.contains("Repository ref snapshot:")
    })
    .await;
    commit_again(dir.path());
    call_tool_ok(
        &peer,
        "Read",
        serde_json::json!({ "file_path": "README.md" }),
    )
    .await;

    let drained = timeout(OBSERVE_TIMEOUT, async {
        loop {
            let messages = queue.drain_all();
            if !messages.is_empty() {
                return messages;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("git ref 变化必须在期限内送达会话 inbox");

    assert_eq!(drained.len(), 1, "一次变化恰注入一条提醒");
    let message = &drained[0];
    assert_eq!(
        message.kind,
        MessageKind::Info,
        "git ref 提醒是 Info（D-4 前的旧契约：不唤醒）"
    );
    assert!(!message.kind.wakes_up(), "Info 不得唤醒会话");
    assert_eq!(
        message.source,
        MessageSource::DynamicMcpNotification,
        "D-4：队列来源标识为 DynamicMcpNotification"
    );
    let reminder = match &message.payload {
        QueuedPayload::SystemReminder(reminder) => reminder.as_reminder(),
        other => panic!("期望 canonical reminder，实际 {other:?}"),
    };
    assert_eq!(reminder.category, ReminderCategory::Diagnostic);
    assert_eq!(reminder.source.0, "git_watch");
    assert_eq!(reminder.kind, "repository_ref_changed");
    assert_eq!(reminder.severity, ReminderSeverity::Info);
    assert_eq!(reminder.delivery, ReminderDelivery::Configurable);
    assert!(reminder.audiences.contains(ReminderAudience::Model));
    assert!(reminder.audiences.contains(ReminderAudience::Tui));
    assert!(reminder.audiences.contains(ReminderAudience::Diagnostics));
    assert_eq!(
        reminder.summary.as_deref(),
        Some("Git branch 或 HEAD 已变化")
    );
    assert_eq!(reminder.metadata, serde_json::json!({}));
    assert!(reminder.body.contains("[Git watch]"), "{}", reminder.body);
    assert!(reminder.body.contains("HEAD:"), "{}", reminder.body);

    // 关闭面隔离：关闭集命中的实例不建立订阅（`subscription_allowed` 的唯一判定）。
    let closed = crate::assembly::builtin_closed_instances(&std::collections::HashSet::from([
        "WorkspaceMiddleware".to_string(),
    ]));
    assert_eq!(
        closed,
        std::collections::BTreeSet::from(["workspace".to_string()]),
        "WorkspaceMiddleware:false 必须映射到 workspace 实例名"
    );
    let closed_pool = Arc::new(McpClientPool::new_empty());
    closed_pool
        .set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new(cwd.clone()).with_closed(closed),
        ))
        .expect("上下文可注入");
    let mut workspace_config: crate::mcp::config::McpServerConfig =
        serde_json::from_str("{}").unwrap();
    workspace_config.source = Some(peri_acp_types::plugin::ConfigSource::Builtin {
        instance: "workspace".to_string(),
    });
    closed_pool
        .configs
        .write()
        .insert("workspace".to_string(), workspace_config.clone());
    assert!(
        !closed_pool.subscription_allowed("workspace"),
        "关闭集命中 ⇒ 不建立订阅（零 git 调用）"
    );
    workspace_config.source = Some(peri_acp_types::plugin::ConfigSource::WorkspaceRemote);
    closed_pool
        .configs
        .write()
        .insert("workspace".to_string(), workspace_config);
    assert!(
        closed_pool.subscription_allowed("workspace"),
        "关闭内置实例不得阻断显式远端 Workspace 的订阅"
    );
    assert!(
        closed_pool.subscription_allowed("web"),
        "非关闭实例不受影响"
    );
    assert!(
        McpClientPool::new_empty().subscription_allowed("workspace"),
        "未注入关闭集（顶层无会话上下文）⇒ 不改变既有行为"
    );

    // 收尾：按生产顺序先停订阅 task（`remove_server` / `set_disabled` 的顺序：
    // `stop_background(Subscription)` → 关 service → 收敛 builtin server task），
    // 再清 `handle.peer`（`shutdown()` 的顺序），最后关连接。
    // 顺序是因果必需：订阅的取消通知必须在连接仍打开时送出，否则服务端的在途
    // `listen` 请求不会被取消，server task 只能等收敛窗口超时后 abort。
    pool.stop_background(&crate::mcp::McpTaskKey::Subscription(
        "workspace".to_string(),
    ))
    .await;
    pool.clients.write().clear();
    drop(peer);
    link.shutdown().await;
    drop(pool);
    let mut owner = owner;
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}

// ─── T7b：回读失败 ⇒ 回退既有通用提醒（事件不丢） ────────────────────────────

/// 资源正文读不回来（server 不支持 `resources/read`）时，git ref 通知必须退化为既有通用
/// 订阅提醒（`Defer` + 唤醒），不得静默丢弃。
///
/// 夹具：父文件的探针 handler（只实现订阅面，`resources/read` 走 rmcp 默认 →
/// `-32601`），以 `workspace` 之名接进 pool 的生产订阅消费循环。
#[tokio::test]
async fn t7b_read_failure_falls_back_to_generic_reminder() {
    let (owner, spawner) = crate::mcp::McpTaskOwner::new();
    let mut pool = McpClientPool::new_pending_with_spawner(spawner);
    pool.resource_cache = crate::mcp::resource_cache::McpResourceCache::isolated_for_test();
    let pool = Arc::new(pool);

    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    pool.register_inbox("session-1", inbox.handle());

    let probe = Arc::new(SubscriptionProbe::default());
    let transport = spawn_builtin_transport_with_handler(
        "workspace",
        SpikeSubscriptionServer {
            probe: Arc::clone(&probe),
        },
    );
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时")
        .expect("builtin 握手不得失败");
    let peer = service.peer().clone();
    pool.clients.write().insert(
        "workspace".to_string(),
        Arc::new(McpClientHandle {
            name: "workspace".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    let subscription = peer.listen(git_filter()).await.expect("listen 必须成功");
    pool.spawn_subscription_loop("workspace", subscription)
        .await;

    wait_until("探针进入 listen 回调", || probe.listen_entered()).await;
    let sink = probe.sink().expect("listen 回调必须登记 sink");
    sink.notify_resource_updated(SPIKE_URI)
        .await
        .expect("filter 内推送必须被接受");

    let drained = timeout(OBSERVE_TIMEOUT, async {
        loop {
            let messages = queue.drain_all();
            if !messages.is_empty() {
                return messages;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("回读失败必须回退通用提醒（事件不丢）");

    assert_eq!(drained.len(), 1);
    let message = &drained[0];
    assert_eq!(
        message.kind,
        MessageKind::Defer,
        "通用订阅提醒是 Defer（唤醒），与 git_watch 的 Info 区分"
    );
    let reminder = match &message.payload {
        QueuedPayload::SystemReminder(reminder) => reminder.as_reminder(),
        other => panic!("期望 canonical reminder，实际 {other:?}"),
    };
    assert_eq!(reminder.kind, "subscription_resource_updated");
    assert_eq!(reminder.source.0, "mcp");
    assert!(
        message.kind.wakes_up(),
        "通用订阅提醒必须可唤醒会话（与 git_watch 的 Info 区分）"
    );

    // 同 T6 的收尾顺序：停订阅 task → 清 handle.peer → drop 本地 peer → 关 service →
    // 收敛 server task（顺序见 T6 的注释）。
    pool.stop_background(&crate::mcp::McpTaskKey::Subscription(
        "workspace".to_string(),
    ))
    .await;
    pool.clients.write().clear();
    drop(peer);
    let mut service = service;
    let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
    let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        !matches!(
            outcome.server,
            crate::mcp::builtin::runtime::BuiltinServerExit::AbortedAfterTimeout
        ),
        "server task 必须收敛，不得 abort：{outcome:?}"
    );
    drop(pool);
    let mut owner = owner;
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}

// ─── T6b：无订阅者 ⇒ 零 git 调用（资源正文保持未采样） ───────────────────────

#[tokio::test]
async fn t6b_no_subscriber_keeps_git_sampling_off() {
    let _env = peri_mcp_common::process_env::lock().expect("process env lock");
    let (dir, cwd) = git_workspace();
    let link = connect_workspace(&cwd).await;
    let peer = link.peer();

    // 不建立任何订阅：成功的工具调用不得触发采样。
    call_tool_ok(
        &peer,
        "Read",
        serde_json::json!({ "file_path": "README.md" }),
    )
    .await;
    commit_again(dir.path());
    call_tool_ok(
        &peer,
        "Read",
        serde_json::json!({ "file_path": "README.md" }),
    )
    .await;
    // 给后台一个可观察窗口：若实现退化为「无条件采样」，正文会变成快照/notice。
    tokio::time::sleep(Duration::from_millis(250)).await;

    assert_eq!(
        read_body(&peer, SPIKE_URI).await,
        UNSAMPLED_RESOURCE_TEXT,
        "无订阅者时不得产生任何 git 采样（正文必须停留在未采样固定文本）"
    );

    link.shutdown().await;
}
