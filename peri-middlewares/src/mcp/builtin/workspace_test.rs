//! `workspace` builtin MCP handler 的 crate 内证据（owner A2 / W3-A）。
//!
//! **本文件只保留宿主侧接线证据**（工具语义、工具级失败投影、超时/提升/杀组语义、
//! cwd 解析见 `peri-mcp-workspace` 包内测试；load 链路 overlay 见 `builtin_apply_test.rs`）：
//! - **线路级**：server 半边是生产 handler，client 半边是生产 `serve_client_auto`
//!   （Auto lifecycle），链路经**生产** `spawn_builtin_transport_with_handler` 装配；
//!   `tools/list` 与 `tools/call` 两个方向都经真实 wire——不得只调 `list_tools()`：
//!   覆写 `discover` 会让同一连接上的 `tools/list` 被会话层以 `-32602` 拒绝，只有真链路
//!   能证伪该失效模式。
//! - **真实工具、无替身**：本文件不注入 stub，7 条工具全走既有实现，因此每条断言都是
//!   「builtin 包装层 + 既有工具实现」的联合证据（AW3-02：本实例是包装）。
//! - **工具集一致性**：`find("workspace")` 的注册表声明（跨 crate：注册表在
//!   `peri-acp-types`、handler 在 package）与线路上 `tools/list` 的成员、**顺序**逐项比对
//!   （`list_tools_of` 的契约是「声明顺序 = tools 顺序」）；成员集合的冻结字面量断言在
//!   package 侧（`resources/wire_test.rs`），此处不重复。
//! - session 级 TaskManager 输入控制后台登记与取消；真实 MCP 桥的最大前台
//!   期限、日志读取和进程收尾见 workspace_recovery_test.rs。
//! - capability root 未引入（AW3-04）：本文件不宣称 cwd 之外不可达——恰恰相反，
//!   绝对路径可读是当前实现事实，登记为 `UNVERIFIED` 缺口。

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::client::{serve_client_auto, McpServiceWrapper};
use peri_acp_types::builtin_mcp::find;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult},
    service::{Peer, RoleClient},
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use peri_mcp_workspace::{WorkspaceInstanceInput, WorkspaceMcpServer};

// ─── 线路：握手 + tools/list ───────────────────────────────────────────────────

#[tokio::test]
async fn workspace_handler_handshakes_and_lists_seven_tools_over_wire() {
    let (_dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let peer = pair.peer();

    let info = peer
        .peer_info()
        .expect("modern 握手后 peer_info 必须是 Some");
    let implementation = info
        .server_info
        .as_ref()
        .expect("modern 握手必须协商出 server_info（真实 handler 的 get_info）");
    assert_eq!(
        implementation.name, "peri-workspace-mcp",
        "实例的 `Implementation::name`（与注册表 key `workspace` 是两件事）"
    );
    assert!(
        info.capabilities.tools.is_some(),
        "必须声明 tools 能力，否则 tools/list 不可达"
    );

    let tools = peer
        .list_all_tools()
        .await
        .expect("tools/list 必须成功（覆写 discover 会让它在会话层被拒）");

    // 跨 crate 一致性（本文件的宿主职责）：注册表 `find("workspace")` 的声明**顺序**与
    // 线路上的 `tools/list` 逐项一致——`list_tools_of` 的契约是「声明顺序 = tools 顺序」；
    // 成员集合本身也由此被钉住（数量与逐项名字）。成员集合的**冻结字面量**断言在
    // package 侧（`resources/wire_test.rs::wire_declares_resources_and_keeps_seven_tools`），
    // 防「注册表与 handler 一起漂移」的那一半由它承担，此处不重复。
    let declared = find("workspace").expect("`workspace` 必须是已实现实例（注册表 T1）");
    let declared_order: Vec<&str> = declared
        .tools
        .iter()
        .map(|tool| tool.original_name)
        .collect();
    let wire_order: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        wire_order, declared_order,
        "线路上的工具与声明顺序必须与注册表 `WORKSPACE_TOOLS` 逐项一致（漂移即红）"
    );

    // 启动期 `mcp::system_tools::validate_input_schema` 的同源约束：schema 根必须是 object，
    // 且声明了非空 properties（否则工具在启动期被拒、或模型面拿到空 schema）。
    for tool in &tools {
        let schema = tool.input_schema.as_ref();
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{}: input_schema 根必须是 object",
            tool.name
        );
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{}: properties 必须是 object", tool.name));
        assert!(!properties.is_empty(), "{}: properties 不得为空", tool.name);
    }

    pair.shutdown().await;
}

// ─── 线路：AW3-11 的 session 级输入（有输入 ⇒ 真后台任务；无输入 ⇒ 可见但退化）────────

/// 正向：`WorkspaceInstanceInput` 的两名成员都送达 `BashTool`——注入真 `TaskManager` 后，
/// `run_in_background` 走的是**真后台任务**而不是退化分支。
///
/// 断言强度（每条都能失败）：
/// ① 线路响应不是错误（`task_manager: None` 时 `BashTool::invoke` 在
///    `middleware/terminal.rs:347` 就返回 `Err` ⇒ 本断言失败）；
/// ② 回执文本含 `task_id: shell-`——该 id 由**真 manager** 的 `spawn_shell` 分配并回显；
/// ③ 注入的那一份 manager 上**恰有 1 个活跃任务**（进程内直接证据：证伪「文本对了，
///    但 manager 是另一份 / 只是常量字符串」）；
/// ④ 第二名成员 `on_bg_complete`：用**自然完成**的短命令验证回调被调用（取消路径上
///    `finalize_bg_shell` 先做 `claim_completion`，被取消的任务已认领 ⇒ 回调按设计跳过，
///    故不能用取消路径断言回调，见 `peri-agent/src/agent/async_tasks/shell.rs:588-590`）；
/// ⑤ 收尾 `shutdown()` 后活跃任务归零（不把 `sleep 30` 留给测试进程退出）。
#[tokio::test]
async fn workspace_handler_bash_run_in_background_uses_injected_task_manager_over_wire() {
    let (_dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(peri_mcp_common::create_local_task_manager());
    let completions = Arc::new(AtomicUsize::new(0));
    let on_bg_complete = {
        let completions = Arc::clone(&completions);
        Arc::new(move |_: &BackgroundTaskResult, _: BgTaskKind| {
            completions.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    };
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(Arc::clone(&manager)),
            on_bg_complete: Some(on_bg_complete),
        }),
    )
    .await;
    let peer = pair.peer();

    // 长命令：断言窗口内任务仍在运行（不用短命令赌调度时序）。
    let result = complete(
        peer.call_tool_once(call(
            "Bash",
            json!({"command": "sleep 30", "run_in_background": true}),
        ))
        .await
        .expect("run_in_background 必须成功（session 级输入已注入）"),
    );
    assert_eq!(
        result.is_error,
        Some(false),
        "有 session 级输入时后台发起不得失败"
    );
    let text = first_text(&result).expect("后台结果必须有文本块");
    assert!(
        text.contains("task_id: shell-"),
        "回执必须含真 manager 分配的 task id（缺 manager 时这里报错）：{text}"
    );
    assert_eq!(
        manager.active_count(),
        1,
        "任务必须登记在**注入的那一份** manager 上（文本对了但空转即在此失败）"
    );

    // 第二名成员：自然完成的短命令必须触发注入的那个回调。
    let quick = complete(
        peer.call_tool_once(call(
            "Bash",
            json!({"command": "true", "run_in_background": true}),
        ))
        .await
        .expect("第二次后台发起同样必须成功"),
    );
    assert_eq!(quick.is_error, Some(false));
    for _ in 0..200 {
        if completions.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        completions.load(Ordering::SeqCst) >= 1,
        "自然完成的后台任务必须调用注入的 on_bg_complete（第二名成员送达）"
    );

    pair.shutdown().await;
    // 收尾：取消并等待 owned 执行真正结束（避免 `sleep 30` 悬挂到测试进程退出）。
    let report = manager.shutdown().await;
    assert_eq!(
        manager.active_count(),
        0,
        "shutdown 之后不得残留活跃任务（收尾报告：{report:?}）"
    );
}
/// U6-2（线路级）：前台**正常完成**时 `command &` 残留子进程的登记差。
///
/// 残留子进程必须不持有 stdout/stderr 的写端（`>/dev/null 2>&1`）：否则前台期限内的
/// `drain_output`（`middleware/terminal.rs:481-499`）要等它退出，用例会落到**超时**
/// 分支而不是「正常完成 + 登记」（`:667-744`）——即测错分支。
///
/// 观察面：①线路文本（登记回执含真 manager 分配的 task id）；②注入 manager 的登记表
/// （`active_count`）。两者都指向同一个事实：残留子进程是否被接管。
///
/// 区分力（破坏后必红）：把 `:682-715` 的 `Some` 侧登记回执去掉 ⇒ 文本断言红；
/// 把 `:716-719` 的 `None` 侧补上同一条回执 ⇒ 反向断言红；把 `:716-719` 改成杀组
/// ⇒ 下面 `orphan_none` 的哨兵不落盘 ⇒ 断言红。
#[cfg(unix)]
#[tokio::test]
async fn workspace_handler_lingering_child_is_registered_only_with_injected_task_manager_over_wire()
{
    let (dir, cwd) = workspace_dir();
    /// 两条变体各自一个哨兵文件（命令骨架相同，只有文件名不同；文件名不参与分支选择，
    /// 分支只看 `self.task_manager` 的有无）。
    const ORPHAN_NONE: &str = "orphan-left-behind-without-input.txt";
    const ORPHAN_SOME: &str = "orphan-left-behind-with-input.txt";
    /// 前台 shell 立刻退出；同进程组的子进程继续跑 3s 后落哨兵。
    const STARTED: &str = "started";
    /// 登记回执的固定前缀（`middleware/terminal.rs:712-715`）。
    const REGISTERED: &str = "Remaining processes continue as background task";
    fn command(marker: &str) -> String {
        format!("(sleep 3; touch {marker}) >/dev/null 2>&1 & echo {STARTED}")
    }

    // ① `None`：残留子进程无法登记 ⇒ 线路文本里没有登记回执。
    let pair = connect(&cwd, None).await;
    let none = complete(
        pair.peer()
            .call_tool_once(call("Bash", json!({"command": command(ORPHAN_NONE)})))
            .await
            .expect("前台正常完成必须成功（无 TaskManager 只影响登记 / 提升路径）"),
    );
    assert_eq!(
        none.is_error,
        Some(false),
        "有残留子进程也仍是成功结果（状态改判为 Unknown，不进 Err）"
    );
    let none_text = first_text(&none).expect("结果必须有文本块");
    assert!(
        none_text.contains(STARTED),
        "前台命令自身的输出必须照常回显：{none_text}"
    );
    assert!(
        !none_text.contains(REGISTERED),
        "无 manager 时残留进程无法登记：线路文本不得出现登记回执：{none_text}"
    );
    pair.shutdown().await;

    // ② `Some(真 TaskManager)`：同一形态的命令 ⇒ 残留进程组在返回前登记，回执带 task id。
    let manager: Arc<dyn TaskManager> = Arc::new(peri_mcp_common::create_local_task_manager());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(Arc::clone(&manager)),
            on_bg_complete: None,
        }),
    )
    .await;
    let some = complete(
        pair.peer()
            .call_tool_once(call("Bash", json!({"command": command(ORPHAN_SOME)})))
            .await
            .expect("有 session 级输入时登记不得失败"),
    );
    assert_eq!(some.is_error, Some(false));
    let some_text = first_text(&some).expect("结果必须有文本块");
    assert!(
        some_text.contains(REGISTERED),
        "有 manager 时残留进程组必须登记并回执：{some_text}"
    );
    assert!(
        some_text.contains("shell-"),
        "登记回执必须含真 manager 分配的 task id：{some_text}"
    );
    assert!(
        !none_text.contains("shell-"),
        "反向对照：退化路径的文本里不得出现任何 task id：{none_text}"
    );
    assert_eq!(
        manager.active_count(),
        1,
        "残留进程组必须登记在**注入的那一份** manager 上，且此刻仍存活（`sleep 3` ≫ 响应时延）"
    );
    pair.shutdown().await;
    let report = manager.shutdown().await;
    assert_eq!(
        manager.active_count(),
        0,
        "shutdown 必须回收登记的残留进程组（报告：{report:?}）"
    );

    // ③ 收尾观察（进程外事实）：`None` 侧的残留子进程**既未被登记、也未随前台调用被
    //    连坐杀掉**，它自行跑完并落哨兵——`release_unmanaged` 只解除 guard 的所有权
    //    （`shell.rs:97-106`），不杀进程。等待窗口 3s+ 覆盖命令自身的 3s。
    let mut orphan_none = false;
    for _ in 0..80 {
        orphan_none = dir.path().join(ORPHAN_NONE).exists();
        if orphan_none {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        orphan_none,
        "`None` 侧的残留子进程必须自由跑完（改成杀组即在此失败）"
    );
    // `Some` 侧同形子进程已被上面的 shutdown 回收（`Complete` 意味着进程组确已消失），
    // 因此它的哨兵不得落盘——这一条同时证明「shutdown 真的回收了进程，而不只是从登记表
    // 里移除条目」。
    assert!(
        !dir.path().join(ORPHAN_SOME).exists(),
        "被 shutdown 回收的残留进程组不得再产出任何副作用"
    );
}
/// client 侧握手上界（`serve_client_auto` 内建）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（夹具收尾）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 一条已握手的 builtin 链路（client service + 本代关闭所有权）。
struct Pair {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：关闭 client（释放 duplex 写半边）→ 本代监督者按冻结顺序有界收敛。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let _ = self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    }
}

/// 经**生产**装配函数握手一条 `workspace` 链路（handler 是真 handler，工具是真工具）。
///
/// `input` 即 AW3-11 的 session 级输入：`None` 是顶层三路径 / 1:N 形态的形态（可见但退化），
/// 既有五条线路用例都走该形态；后台任务两条用例显式传 `Some` / `None` 各一遍。
async fn connect(cwd: &str, input: Option<WorkspaceInstanceInput>) -> Pair {
    let transport =
        spawn_builtin_transport_with_handler("workspace", WorkspaceMcpServer::new(cwd, input));
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时（同进程链路）")
        .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
    }
}

#[tokio::test]
async fn bridge_accepts_workspace_owned_task_handle() {
    use peri_acp_types::mcp::McpSubscriptionPort;
    use peri_acp_types::session::{
        MessageKind, MessageQueue, MessageSource, QueuedPayload, SessionInbox,
    };
    use peri_agent::tools::{BaseTool, ToolContext};
    let (mut owner, spawner) = crate::mcp::McpTaskOwner::new();
    let pool = Arc::new(crate::mcp::McpClientPool::new_pending_with_spawner(spawner));

    let (_dir, cwd) = workspace_dir();
    let context = crate::mcp::builtin::context::BuiltinInstanceContext::new(cwd.clone());
    context
        .task_scope_authority
        .set(pool.task_scope_authority.clone())
        .ok();
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_context(
        "workspace",
        &context,
        &std::collections::HashMap::new(),
    )
    .expect("builtin Workspace dispatch");
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("handshake deadline")
        .expect("handshake");
    let pair = Pair {
        service,
        supervisor,
    };
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    pool.register_inbox("task-session", inbox.handle());
    let peer = pair.peer();
    let tools = peer.list_all_tools().await.expect("tool list");
    let bash = tools.iter().find(|tool| tool.name == "Bash").expect("Bash");
    let handle = Arc::new(crate::mcp::client::McpClientHandle {
        name: "workspace".into(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(peer.clone()),
        tools: tools.clone(),
        resources: vec![],
        status: crate::mcp::client::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: Some(crate::mcp::config::ConfigSource::Builtin {
            instance: "workspace".into(),
        }),
        url: None,
        skills_capable: false,
    });
    pool.clients
        .write()
        .insert("workspace".into(), Arc::clone(&handle));
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("task-session", &manager);
    let bridge = crate::mcp::tool_bridge::McpToolBridge::new("workspace", bash, handle)
        .with_output_store(&pool, Some("task-session"));
    let receipt = bridge
        .invoke(
            json!({"command":"sleep 0.2; printf bridge-ok", "run_in_background":true}),
            ToolContext::new(&[], &cwd).with_session_identity("task-session", "task-turn"),
        )
        .await
        .expect("bridge accepts Tasks response");
    let task_id = receipt
        .split("Background task started: ")
        .nth(1)
        .expect("task receipt")
        .split('.')
        .next()
        .expect("task id");
    assert!(task_id.starts_with("mcp-"));
    assert!(manager
        .snapshot()
        .tasks
        .iter()
        .any(|task| task.task_id == task_id));
    tokio::time::timeout(Duration::from_secs(5), inbox.await_wake())
        .await
        .expect("task completion wakes its session");
    let notifications = queue.drain_all();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, MessageKind::Defer);
    assert_eq!(notifications[0].source, MessageSource::ShellComplete);
    let QueuedPayload::SystemReminder(reminder) = &notifications[0].payload else {
        panic!("expected task reminder")
    };
    let owner_task_id = reminder.as_reminder().metadata["task_id"]
        .as_str()
        .expect("completion carries the actionable Workspace task identity");
    assert!(owner_task_id.starts_with("shell-"));
    assert!(reminder
        .as_reminder()
        .body
        .contains(&owner_task_id.chars().take(8).collect::<String>()));
    let mut params = rmcp::model::GetTaskParams::new(owner_task_id);
    params.meta = pool.task_scope_meta_for("workspace", "task-session");
    let task = peer.get_task(params).await.expect("query owned task").task;
    assert_eq!(task.task.task_id, owner_task_id);
    let rmcp::model::TaskPayload::Completed { result } = task.payload else {
        panic!("completion reminder must identify a completed task")
    };
    let result: BackgroundTaskResult =
        serde_json::from_value(result["structuredContent"].clone()).expect("shell result");
    assert_eq!(result.task_id, owner_task_id);
    assert!(result.success);
    let output_path = result
        .shell_output
        .expect("shell output evidence")
        .stdout_path
        .expect("readable stdout path");
    let mut read = call("Read", json!({"file_path":output_path}));
    read.meta = pool.task_scope_meta_for("workspace", "task-session");
    let output = complete(peer.call_tool_once(read).await.expect("read task stdout"));
    assert_eq!(output.is_error, Some(false));
    assert!(first_text(&output)
        .expect("stdout text")
        .contains("bridge-ok"));
    assert!(reminder.as_reminder().body.contains("stdout 输出文件"));
    assert!(!reminder.as_reminder().body.contains("Task details:"));
    pair.shutdown().await;
    let _ = owner.shutdown().await;
}

#[tokio::test]
async fn closing_workspace_scope_reconciles_without_live_agent_manager() {
    let (mut owner, spawner) = crate::mcp::McpTaskOwner::new();
    let pool = Arc::new(crate::mcp::McpClientPool::new_pending_with_spawner(spawner));
    let (_dir, cwd) = workspace_dir();
    let context = crate::mcp::builtin::context::BuiltinInstanceContext::new(cwd.clone());
    context
        .task_scope_authority
        .set(pool.task_scope_authority.clone())
        .ok();
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_context(
        "workspace",
        &context,
        &std::collections::HashMap::new(),
    )
    .expect("builtin Workspace dispatch");
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("handshake deadline")
        .expect("handshake");
    let pair = Pair {
        service,
        supervisor,
    };
    let peer = pair.peer();
    let mut config: crate::mcp::config::McpServerConfig =
        serde_json::from_value(json!({})).expect("empty MCP config");
    config.source = Some(crate::mcp::config::ConfigSource::Builtin {
        instance: "workspace".into(),
    });
    pool.configs.write().insert("workspace".into(), config);
    pool.clients.write().insert(
        "workspace".into(),
        Arc::new(crate::mcp::client::McpClientHandle {
            name: "workspace".into(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools: vec![],
            resources: vec![],
            status: crate::mcp::client::ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            }),
            url: None,
            skills_capable: false,
        }),
    );
    let mut first_call = call(
        "Bash",
        json!({"command":"sleep 30", "run_in_background":true}),
    );
    first_call.meta = pool.task_scope_meta_for("workspace", "closing-session");
    let scope = first_call.meta.clone().expect("trusted scope");
    let before = peer
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            rmcp::model::CustomRequest::new("workspace/taskSnapshot", Some(json!({"_meta":scope}))),
        ))
        .await
        .expect("snapshot before close");
    let rmcp::model::ServerResult::CustomResult(before) = before else {
        panic!("scope snapshot result")
    };
    let close_epoch = before.0["epoch"].as_u64().expect("scope epoch");
    let response = peer
        .call_tool_once(first_call)
        .await
        .expect("start scoped Bash");
    assert!(matches!(response, CallToolResponse::Task(_)));
    pool.reconcile_closing_workspace_scope("closing-session")
        .await
        .expect("owner confirms all tasks terminal without Agent manager");
    pool.open_workspace_task_scope("closing-session")
        .await
        .expect("same session scope opens after close settles");
    let mut reopened = call(
        "Bash",
        json!({"command":"printf reopened", "run_in_background":true}),
    );
    reopened.meta = Some(scope.clone());
    let response = peer
        .call_tool_once(reopened)
        .await
        .expect("Bash accepted after reopen");
    assert!(matches!(response, CallToolResponse::Task(_)));
    assert!(
        peer.send_request(rmcp::model::ClientRequest::CustomRequest(
            rmcp::model::CustomRequest::new(
                "workspace/taskClose",
                Some(json!({"_meta":scope,"epoch":close_epoch})),
            ),
        ))
        .await
        .is_err(),
        "stale taskClose must not close reopened scope"
    );
    pair.shutdown().await;
    let _ = owner.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_close_cleans_up_workspace_owned_bash() {
    let (dir, cwd) = workspace_dir();
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_context(
        "workspace",
        &crate::mcp::builtin::context::BuiltinInstanceContext::new(cwd),
        &std::collections::HashMap::new(),
    )
    .expect("builtin Workspace dispatch");
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("handshake deadline")
        .expect("handshake");
    let pair = Pair {
        service,
        supervisor,
    };
    let response = pair
        .peer()
        .call_tool_once(call(
            "Bash",
            json!({"command":"echo $$ > running.pid.tmp; mv running.pid.tmp running.pid; exec sleep 30", "run_in_background":true}),
        ))
        .await
        .expect("start background Bash");
    assert!(matches!(response, CallToolResponse::Task(_)));
    let marker = dir.path().join("running.pid");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Bash process marker");
    let pid: i32 = std::fs::read_to_string(marker)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid");
    pair.shutdown().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::process::Command::new("kill")
            .args(["-0", "--", &format!("-{pid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("check process group")
            .success()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Workspace Bash process group must exit with builtin");
}

/// 临时工作目录（**已 canonicalize**）。
///
/// macOS 的 `/var` 是 `/private/var` 的符号链接：不规范化时工具内部的
/// `resolve_path`（`tools/filesystem/mod.rs:27-44` 会 canonicalize）与夹具的
/// `strip_prefix(cwd)` 口径不一致，写回文本会变成绝对路径。规范化后夹具与工具同源。
fn workspace_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("临时目录夹具必须可创建");
    let cwd = dir.path().canonicalize().expect("夹具目录必须可规范化");
    (dir, cwd.to_string_lossy().to_string())
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块。
fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// 断言响应是 `Complete` 并取出结果（IF-D14 的失败形态**不是** `Err`）。
fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

// ─── beta flag `full-async-tools`（装配面 → 生产 Bash 路径）──────────────────

/// `BuiltinInstanceContext` 注入的有效缺省同时驱动 schema `default` 与省略字段的调用：
/// ① `tools/list` 的 `run_in_background.default` 随缺省（flag 开启时 true）；
/// ② 省略字段的调用走 **owned 后台任务**（生产 `standalone` 路径）；
/// ③ 显式 `false` 仍走前台（flag 只改缺省）。
#[tokio::test]
async fn workspace_dispatch_propagates_bash_default_run_in_background() {
    let (_dir, cwd) = workspace_dir();
    let context = crate::mcp::builtin::context::BuiltinInstanceContext::new(cwd.clone())
        .with_workspace_bash_default_run_in_background(true);
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_context(
        "workspace",
        &context,
        &std::collections::HashMap::new(),
    )
    .expect("内置 Workspace 分派必须成功");
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("内置链路握手不得超时")
        .expect("内置链路握手不得失败");
    let pair = Pair {
        service,
        supervisor,
    };
    let peer = pair.peer();

    let tools = peer.list_all_tools().await.expect("tools/list");
    let bash = tools
        .iter()
        .find(|tool| tool.name == "Bash")
        .expect("Bash 必须声明");
    assert_eq!(
        bash.input_schema["properties"]["run_in_background"]["default"],
        json!(true),
        "装配输入的有效缺省必须到达工具 schema"
    );

    // 省略字段：owned 后台任务（客户端支持 tasks 时回 Task 句柄，否则回执文本携带
    // 任务启动信息——两条形态都只可能来自后台分支）。命令立刻结束，测试退出前不留
    // 存活进程。
    let omitted = peer
        .call_tool_once(call("Bash", json!({"command": "printf bg-by-default"})))
        .await
        .expect("省略 run_in_background 且缺省为 true 时必须走后台");
    let omitted_is_background = match &omitted {
        CallToolResponse::Task(_) => true,
        CallToolResponse::Complete(result) => {
            first_text(result).is_some_and(|text| text.contains("Background shell task started"))
        }
        other => panic!("未知响应形态：{other:?}"),
    };
    assert!(
        omitted_is_background,
        "省略字段必须走 owned 后台任务：{omitted:?}"
    );

    // 显式 false：前台路径（同一链路、同一命令，仍是 Complete 结果）。
    let explicit = complete(
        peer.call_tool_once(call(
            "Bash",
            json!({"command": "printf explicit-foreground", "run_in_background": false}),
        ))
        .await
        .expect("显式 false 必须走前台"),
    );
    assert_eq!(explicit.is_error, Some(false));
    let text = first_text(&explicit).expect("前台结果必须有文本块");
    assert!(
        text.contains("explicit-foreground"),
        "显式 false 必须拿到前台输出：{text}"
    );
    assert!(
        !text.contains("Background shell task started"),
        "显式 false 不得走后台：{text}"
    );

    pair.shutdown().await;
}
