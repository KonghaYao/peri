//! `workspace` builtin MCP handler 的 crate 内证据（owner A2 / W3-A）。
//!
//! 覆盖口径（主 plan §3.3 T2 / §4 A2 / §5）：
//! - **线路级**：server 半边是生产 handler，client 半边是生产 `serve_client_auto`
//!   （Auto lifecycle），链路经**生产** `spawn_builtin_transport_with_handler` 装配；
//!   `tools/list` 与 `tools/call` 两个方向都经真实 wire——不得只调 `list_tools()`：
//!   覆写 `discover` 会让同一连接上的 `tools/list` 被会话层以 `-32602` 拒绝，只有真链路
//!   能证伪该失效模式。
//! - **真实工具、无替身**：本文件不注入 stub，7 条工具全走既有实现，因此每条断言都是
//!   「builtin 包装层 + 既有工具实现」的联合证据（AW3-02：本实例是包装）。
//! - **cwd 贯通（AW3-05）**：临时目录夹具同时覆盖文件工具的相对路径解析与 `Bash` 的
//!   `current_dir`——两者必须落在**同一个** host cwd。
//! - **IF-D14 映射**：未知工具名返回 invalid_params；执行错误返回 is_error，
//!   已分类原因与工具生成的恢复引用保留，任意路径/命令/凭据错误串不透传。
//! - session 级 TaskManager 输入控制后台登记与取消；真实 MCP 桥的最大前台
//!   期限、日志读取和进程收尾见 workspace_recovery_test.rs。
//!
//! - capability root 未引入（AW3-04）：本文件不宣称 cwd 之外不可达——恰恰相反，
//!   绝对路径可读是当前实现事实，登记为 `UNVERIFIED` 缺口。
//! - **cwd 反例实验的发现**：`invoke_tool_call` 的 `cwd` 参只落进 `ToolContext`，而本波
//!   7 个工具都忽略它——把该参改成任意值，本文件的断言**不会**失败（实测：全绿）；
//!   真正的 cwd 绑定点是 `WorkspaceMcpServer::new` 注入各工具的 `cwd` 字段（反例实验
//!   把构造期的 cwd 改成不存在的目录时，两条 cwd 用例立刻转红）。后续波次若引入
//!   `ToolContext` 消费型工具，需同时复核这条两处 cwd 的口径一致性。
//! - 工具集期望值有**两份独立来源**，两份都断言：①注册表
//!   `find("workspace")` 的声明（`web_test.rs:196-212` 的既有形态，锁「注册表 ↔ handler」
//!   漂移）；②本文件的冻结字面量 `EXPECTED_TOOLS`（锁 AW3-03 的 7 项成员集合，
//!   防「注册表与 handler 一起漂移」）。声明**顺序**另按注册表逐项比对
//!   （`list_tools_of` 的契约是「声明顺序 = tools 顺序」）。

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::client::{serve_client_auto, McpServiceWrapper};
use peri_acp_types::builtin_mcp::find;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_agent::agent::async_tasks::TaskManager as ConcreteTaskManager;
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode},
    service::{Peer, RoleClient},
    ServiceError,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use peri_agent::tools::{BaseTool, ToolContext, ToolExecutionStatus};
use peri_mcp_workspace::{terminal::BashTool, WorkspaceInstanceInput, WorkspaceMcpServer};

/// 期望工具集（**冻结字面量**，AW3-03 的 7 项）。
///
/// 独立于注册表：即使「注册表与 handler 一起漂移」，本字面量仍会失败。成员集合按排序比对
/// （与声明顺序无关）；声明顺序另由 `find("workspace")` 的逐项比对锁定。
const EXPECTED_TOOLS: [&str; 7] = [
    "Bash",
    "Edit",
    "Glob",
    "Grep",
    "Read",
    "Write",
    "folder_operations",
];

/// 夹具文件名与正文：可辨认、无平台依赖。
const FILE_NAME: &str = "wire-marker.txt";
const FILE_CONTENT: &str = "peri-workspace-wire-marker";
/// 路径形状的哨兵串：出现在模型面失败文本里即违规（§9 规则 7）。
const LEAK_PATH: &str = "/tmp/secret-marker/private.txt";

// ─── 夹具 ─────────────────────────────────────────────────────────────────────

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
    assert_eq!(
        tools.len(),
        EXPECTED_TOOLS.len(),
        "workspace 实例的工具面恰为 7 项（AW3-03）"
    );

    // ① 注册表派生（`find("workspace")`）：声明**顺序**与成员集合逐项一致——`list_tools_of`
    // 的契约是「声明顺序 = tools 顺序」。
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

    // ② 冻结字面量（AW3-03 的 7 项）：注册表与 handler 一起漂移时仍能失败。
    let mut actual = wire_order;
    actual.sort_unstable();
    let mut expected = EXPECTED_TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(
        actual, expected,
        "线路上的工具集必须与 AW3-03 冻结的 7 项字面量一致"
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

// ─── 线路：真实工具调用（Write → Read 往返；Bash 同 cwd） ─────────────────────

#[tokio::test]
async fn workspace_handler_write_then_read_round_trips_in_host_cwd_over_wire() {
    let (dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let peer = pair.peer();

    let written = complete(
        peer.call_tool_once(call(
            "Write",
            json!({"file_path": FILE_NAME, "content": FILE_CONTENT}),
        ))
        .await
        .expect("Write 必须成功"),
    );
    assert_eq!(written.is_error, Some(false));
    assert_eq!(
        first_text(&written).as_deref(),
        Some(format!("Wrote 1 line {FILE_NAME}").as_str()),
        "写回文本由既有实现生成（相对路径 + 行数），本层不改写"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(FILE_NAME)).expect("夹具文件必须已落盘"),
        FILE_CONTENT,
        "相对路径必须落在实例的 host cwd 上（AW3-05 的 cwd 贯通）"
    );

    let read = complete(
        peer.call_tool_once(call("Read", json!({"file_path": FILE_NAME})))
            .await
            .expect("Read 必须成功"),
    );
    assert_eq!(read.is_error, Some(false));
    assert_eq!(
        first_text(&read).as_deref(),
        Some(format!("     1\t{FILE_CONTENT}").as_str()),
        "Read 返回既有格式（1-based 行号 + 制表符 + 正文），往返文本必须与写入一致"
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn workspace_handler_bash_runs_in_the_same_host_cwd_over_wire() {
    let (dir, cwd) = workspace_dir();
    std::fs::write(dir.path().join(FILE_NAME), FILE_CONTENT).expect("夹具文件必须已落盘");
    let pair = connect(&cwd, None).await;
    let peer = pair.peer();

    let result = complete(
        peer.call_tool_once(call("Bash", json!({"command": format!("cat {FILE_NAME}")})))
            .await
            .expect("前台 Bash 必须成功（无 TaskManager 只影响后台 / 提升路径）"),
    );
    assert_eq!(result.is_error, Some(false));
    let text = first_text(&result).expect("Bash 结果必须有文本块");
    assert!(
        text.contains(FILE_CONTENT),
        "Bash 必须在实例的 host cwd 下执行（`current_dir` 贯通，AW3-05）：{text}"
    );

    pair.shutdown().await;
}

// ─── 线路：错误路径（typed 协议错误 + 脱敏的失败结果） ────────────────────────

#[tokio::test]
async fn workspace_handler_unknown_tool_is_invalid_params_over_wire() {
    let (_dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let peer = pair.peer();

    let error = peer
        .call_tool_once(call("Nope", json!({})))
        .await
        .expect_err("未知工具名必须回 -32602");
    match error {
        ServiceError::McpError(data) => {
            assert_eq!(data.code.0, ErrorCode::INVALID_PARAMS.0);
            assert!(
                data.message.contains("unknown tool: Nope"),
                "错误文本应含工具名；实际：{}",
                data.message
            );
        }
        other => panic!("期望 McpError(invalid_params)，实际：{other:?}"),
    }

    pair.shutdown().await;
}

#[tokio::test]
async fn workspace_handler_tool_failures_return_sanitized_error_result_over_wire() {
    let (_dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let peer = pair.peer();

    // ① 缺必填参数（`file_path`）② 路径不存在且**含路径形状哨兵**：两种原始错误都不得
    // 进入模型面文本，且必须给出不同的安全原因（不泄漏输入值）。
    let missing = complete(
        peer.call_tool_once(call("Read", json!({})))
            .await
            .expect("工具级失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(
        missing.is_error,
        Some(true),
        "失败必须进 CallToolResult::error"
    );
    let text = first_text(&missing).expect("错误结果必须有文本块");
    assert!(
        text.contains("tool `Read` failed to execute"),
        "固定规则文本必须含工具名：{text}"
    );
    assert!(text.contains("file_path") && text.contains("required"));

    let leaky = complete(
        peer.call_tool_once(call("Read", json!({"file_path": LEAK_PATH})))
            .await
            .expect("工具级失败仍走协议成功"),
    );
    assert_eq!(leaky.is_error, Some(true));
    let leaky_text = first_text(&leaky).expect("错误结果必须有文本块");
    assert_ne!(leaky_text, text);
    assert!(leaky_text.contains("File not found"));
    assert!(
        !leaky_text.contains(LEAK_PATH),
        "线路上的错误文本不得含路径形状串（§9 规则 7）：{leaky_text}"
    );

    // Bash 的失败路径同样脱敏（含命令文本的原始错误不得外泄）。
    let bash = complete(
        peer.call_tool_once(call("Bash", json!({})))
            .await
            .expect("工具级失败仍走协议成功"),
    );
    assert_eq!(bash.is_error, Some(true));
    let bash_text = first_text(&bash).expect("错误结果必须有文本块");
    assert!(
        bash_text.contains("tool `Bash` failed to execute"),
        "固定规则文本必须含工具名：{bash_text}"
    );

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
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    let completions = Arc::new(AtomicUsize::new(0));
    let on_bg_complete = {
        let completions = Arc::clone(&completions);
        Arc::new(move |_: &BackgroundTaskResult, _: BgTaskKind| {
            completions.fetch_add(1, Ordering::SeqCst);
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

/// 退化（`input == None`）：AW3-11 的「可见但退化」——实例照常装配、`Bash` 拒绝后台请求。
///
/// 两个层次各断言一次：本无 manager 分支仍是未分类错误，`invoke_tool_call` 回
/// 固定脱敏文本（IF-D14 规则 7，`web.rs:110-113` 的 `Err(_error)`），所以线路文本里不会、
/// 也不得出现 `run_in_background` 字样；原因短语只在工具层可见。本用例同时锁定这两件事实
/// ——「模型面只得到 is_error + 固定文本」与「同一 `None` 形态的 `BashTool` 报的确是
/// `run_in_background` 不可用」——不得只锁一半（否则「Bash 因别的原因失败」也会绿）。
#[tokio::test]
async fn workspace_handler_without_session_input_rejects_run_in_background_over_wire() {
    let (_dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let peer = pair.peer();

    let result = complete(
        peer.call_tool_once(call(
            "Bash",
            json!({"command": "sleep 1", "run_in_background": true}),
        ))
        .await
        .expect("工具级失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(
        result.is_error,
        Some(true),
        "无 session 级输入时 run_in_background 必须失败（可见但退化，AW3-11）"
    );
    let text = first_text(&result).expect("错误结果必须有文本块");
    assert!(
        text.contains("tool `Bash` failed to execute"),
        "固定规则文本必须含工具名：{text}"
    );
    assert!(
        !text.contains("run_in_background"),
        "线路文本不得含原因短语（IF-D14 规则 7 脱敏；原因只在工具层可见）：{text}"
    );

    // 工具层：同一 `None` 形态（`WorkspaceMcpServer::new(cwd, None)` 里 `BashTool::new` 的
    // 缺省）下，原始错误逐字为冻结的退化分支文本。
    let bash = BashTool::new(cwd.as_str());
    let error = bash
        .invoke(
            json!({"command": "sleep 1", "run_in_background": true}),
            ToolContext::new(&[], cwd.as_str()),
        )
        .await
        .expect_err("无 manager 的 BashTool 必须拒绝 run_in_background");
    let error = error.to_string();
    assert!(
        error.contains("run_in_background is not available"),
        "退化分支的原始错误必须点明 run_in_background：{error}"
    );
    assert!(
        error.contains("no background task manager configured"),
        "退化分支的原始错误必须点明缺 manager：{error}"
    );

    pair.shutdown().await;
}

// ─── 线路：U6（`None` 退化两条路径的成对对照） ─────────────────────────────────
//
// 本节四例共用一条纪律：**同一条命令、同一个 `timeout`**，只换 AW3-11 的 session 级
// 输入（`None` / `Some(真 TaskManager)`），断言两边的可观察差异。
//
// **缩放纪律（不改分支、只改时长）**：超时分支的选择条件是
// `self.task_manager.as_ref()` 的有无（`middleware/terminal.rs:525` 的 `if let` /
// `:637` 的 `else`），`timeout` 只决定期限 `Duration` 与回执里的秒数
// （`parse_foreground_timeout` → `foreground_timeout_ms`，`:411-412`），因此
// `timeout: 300ms` 与 `timeout: 150000ms` 走**同一条**分支。**不跑 150s** 的理由：
// 前台期限被 `FOREGROUND_MAX_TIMEOUT_MS = 120000` 界住（`:409-412` 注释与
// `parse_foreground_timeout` 的 `ms > FOREGROUND_MAX_TIMEOUT_MS` 分支），>120s 的请求
// 与 120s 请求在实现里被拉齐（U2 探针的边界竞争问题），对本节的「提升 vs 杀组」
// 判定不增加任何信息量；真跑 150s 只延长测试、不改变分支。

/// U6-1（线路级，进程级对照）：前台超时后**进程组是否存活**——`None` 变体杀组、
/// `Some` 变体提升为后台任务。
///
/// 同时检查安全回执和进程证据：有 manager 时给出任务 ID/PID，无 manager 时
/// 说明终止。`sleep 1; touch <哨兵>` 进一步证明超时后进程是否继续执行，
/// 防止仅回执文本改变却没有真实提升/终止行为。
///
/// 区分力（破坏后必红）：把 `:637-661` 的 `None` 分支改成不杀进程组 ⇒ `MARKER_NONE`
/// 会落盘 ⇒ 断言红；把 `:525-616` 的 `Some` 分支改成不提升（杀组）⇒ `MARKER_SOME`
/// 不落盘 ⇒ 断言红。
#[cfg(unix)]
#[tokio::test]
async fn workspace_handler_foreground_timeout_kills_the_group_without_input_and_promotes_with_input_over_wire(
) {
    let (dir, cwd) = workspace_dir();
    /// 每条变体一个哨兵文件名（命令在超时**之后**才执行 `touch`）。
    const MARKER_NONE: &str = "ran-after-timeout-without-input.txt";
    const MARKER_SOME: &str = "ran-after-timeout-with-input.txt";
    /// 同一条命令骨架：先睡 1s（跨过 300ms 前台期限），再落哨兵。
    const COMMAND_SKELETON: &str = "sleep 1; touch";
    /// 前台期限：短缩放（见本节头部的缩放纪律）。
    const TIMEOUT_MS: u64 = 300;

    // ① `None`（无 session 级输入）：超时分支杀进程组 ⇒ 命令不可能跑到 `touch`。
    let pair = connect(&cwd, None).await;
    let none = complete(
        pair.peer()
            .call_tool_once(call(
                "Bash",
                json!({"command": format!("{COMMAND_SKELETON} {MARKER_NONE}"), "timeout": TIMEOUT_MS}),
            ))
            .await
            .expect("工具级失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(
        none.is_error,
        Some(true),
        "超时被 `invoke` 转成 Err（terminal.rs:756-774）⇒ 线路报错，与「哪条分支」无关"
    );
    let none_text = first_text(&none).expect("错误结果必须有文本块");
    pair.shutdown().await;

    // ② `Some(真 TaskManager)`：超时分支不杀、register + promote ⇒ 同一个 shell 继续
    //    跑到 `touch`。
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
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
            .call_tool_once(call(
                "Bash",
                json!({"command": format!("{COMMAND_SKELETON} {MARKER_SOME}"), "timeout": TIMEOUT_MS}),
            ))
            .await
            .expect("工具级失败仍走协议成功"),
    );
    assert_eq!(
        some.is_error,
        Some(true),
        "提升同样先回 Err（两条路径此处同形）"
    );
    let some_text = first_text(&some).expect("错误结果必须有文本块");
    assert!(some_text.contains("background task"));
    assert!(some_text.contains("task_id: shell-"));
    assert!(some_text.contains("pid: "));
    assert!(none_text.contains("terminated"));
    assert!(!none_text.contains("task_id:"));
    pair.shutdown().await;

    // 同一窗口内看两条哨兵：窗口 3s ≫ 命令自身的 1s，且窗口起点已在两条命令启动之后，
    // 所以「`None` 侧不落盘」不是「还没跑到」的假绿（这个等待是断言区分力的前提：
    // 不等满命令时长时，杀不杀都看不到文件）。
    let mut some_marker = false;
    for _ in 0..60 {
        some_marker = dir.path().join(MARKER_SOME).exists();
        if some_marker {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        some_marker,
        "`Some` 变体必须提升：超时后同一个 shell 必须继续执行到命令结束（改成杀组即在此失败）"
    );
    assert!(
        !dir.path().join(MARKER_NONE).exists(),
        "`None` 变体必须杀进程组：超时后命令的任何后续步骤都不得执行"
    );

    let report = manager.shutdown().await;
    assert_eq!(
        manager.active_count(),
        0,
        "收尾后不得残留后台任务（提升的进程组必须被回收；报告：{report:?}）"
    );
}

/// U6-1（工具层，文本 + typed evidence 对照）：同一命令 / 同一 `timeout` 打到
/// `BashTool` 的两条能力源形态，回执文本与 typed evidence 必须**互斥**。
///
/// 本用例补的是线路层看不到的那一面：`invoke_output`（`middleware/terminal.rs:331`）
/// 带 typed evidence，两条分支的措辞也不同（`:644-653` 的「有界同步路径」 vs
/// `:607-616` 的「已提升为后台任务 + task_id」）。线路层看不到它的原因见上一个用例的
/// 两条路径均为 error 结果，但安全回执区分后台运行与已终止。
///
/// 区分力（破坏后必红）：把 `:644-661`（`None` 分支）的状态改成 `RunningAfterTimeout`
/// 或把文本换成提升措辞 ⇒ 本用例红；把 `:607-616` 的 task_id 去掉同理。
#[cfg(unix)]
#[tokio::test]
async fn foreground_timeout_text_and_evidence_differ_without_and_with_task_manager() {
    let (_dir, cwd) = workspace_dir();
    let input = json!({"command": "sleep 5", "timeout": 300});

    // ① 无 manager：杀组 + `TimedOut` + 无 task_id；文本必须点明同步路径有界，
    //    且**不得**出现提升措辞。
    let none = BashTool::new(cwd.as_str())
        .invoke_output(input.clone(), ToolContext::new(&[], cwd.as_str()))
        .await
        .expect("工具层超时不是 Err（转 Err 的是 `invoke`：terminal.rs:756-774）");
    assert!(
        none.text.contains("The synchronous path is always bounded"),
        "无 manager 的超时回执必须落在「有界同步路径」分支：{}",
        none.text
    );
    assert!(
        !none.text.contains("promoted to a background task"),
        "无 manager 时不得声称已提升为后台任务：{}",
        none.text
    );
    let none_evidence = none.execution.expect("BashTool 必须带 typed evidence");
    assert_eq!(
        none_evidence.status,
        ToolExecutionStatus::TimedOut,
        "无 manager 的超时必须报 TimedOut（而非 RunningAfterTimeout）"
    );
    assert_eq!(none_evidence.task_id, None, "杀组路径没有 task_id");

    // ② 有 manager：提升 + `RunningAfterTimeout` + task_id；同一批断言取反。
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    let some = BashTool::new(cwd.as_str())
        .with_task_manager(Arc::clone(&manager))
        .invoke_output(input, ToolContext::new(&[], cwd.as_str()))
        .await
        .expect("提升路径同样不是 Err（`invoke_output` 保留 typed evidence）");
    assert!(
        some.text.contains("has been promoted to a background task"),
        "有 manager 的超时回执必须落在提升分支：{}",
        some.text
    );
    assert!(
        some.text.contains("task_id: shell-"),
        "提升回执必须回显真 manager 分配的 task id：{}",
        some.text
    );
    assert!(
        !some.text.contains("The synchronous path is always bounded"),
        "提升分支不得回「已杀组」的同步路径文本：{}",
        some.text
    );
    let some_evidence = some.execution.expect("BashTool 必须带 typed evidence");
    assert_eq!(
        some_evidence.status,
        ToolExecutionStatus::RunningAfterTimeout,
        "提升路径的 typed status 必须是 RunningAfterTimeout"
    );
    assert!(
        some_evidence
            .task_id
            .as_deref()
            .is_some_and(|id| id.starts_with("shell-")),
        "提升路径必须带 `shell-` 前缀的 task_id：{:?}",
        some_evidence.task_id
    );
    assert_eq!(
        manager.active_count(),
        1,
        "提升后的任务必须登记在**注入的那一份** manager 上，且此刻仍在跑（`sleep 5` ≫ 300ms）"
    );

    let report = manager.shutdown().await;
    assert_eq!(
        manager.active_count(),
        0,
        "shutdown 必须回收提升的进程组（报告：{report:?}）"
    );
}

/// U6-1 的第三条分支（同时澄清一处引用口径）：**提升被注册表拒绝**时的回落路径
/// （`middleware/terminal.rs:618-635`，文本含 `could not be promoted to a background task`
/// 与 `The process group has been terminated.`）。
///
/// 该分支**不是** `None` 退化分支：`None` 分支（`:637-661`）根本不尝试 `register`，
/// 文本是「有界同步路径」（见上一条用例）。本用例把「有 manager 但 register 被拒」
/// 这条独立路径也钉住：回落为 `TimedOut` + 杀组 + 无 task_id，与成功提升
/// （`RunningAfterTimeout` + task_id）互斥。
///
/// 区分力（破坏后必红）：把 `:626` 的措辞换成提升措辞、或把 `:629` 的状态改成
/// `RunningAfterTimeout` ⇒ 本用例红。
#[cfg(unix)]
#[tokio::test]
async fn foreground_timeout_with_rejected_promotion_falls_back_to_killing_the_group() {
    use peri_acp_types::tasks::BgTaskRegistration;
    use peri_agent::agent::async_tasks::BackgroundTaskRegistry;

    let (_dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    // 占满 Shell 类并发位（纯登记表条目，不产生真进程）⇒ 提升时 `register` 必被拒。
    for index in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        manager
            .register(BgTaskRegistration {
                task_id: format!("occupied-{index}"),
                kind: BgTaskKind::Shell,
                summary: "capacity fixture".into(),
                pid: None,
                kill: Some(Box::new(|| {})),
            })
            .expect("占位登记必须成功");
    }
    assert_eq!(manager.active_count(), BackgroundTaskRegistry::SHELL_LIMIT);

    let out = BashTool::new(cwd.as_str())
        .with_task_manager(Arc::clone(&manager))
        .invoke_output(
            json!({"command": "sleep 5", "timeout": 300}),
            ToolContext::new(&[], cwd.as_str()),
        )
        .await
        .expect("工具层超时不是 Err（转 Err 的是 `invoke`）");
    assert!(
        out.text
            .contains("could not be promoted to a background task"),
        "提升被拒的回执必须点明「无法提升」：{}",
        out.text
    );
    assert!(
        out.text.contains("The process group has been terminated"),
        "提升被拒必须回落到杀进程组：{}",
        out.text
    );
    assert!(
        !out.text.contains("has been promoted to a background task"),
        "被拒路径不得出现成功提升的措辞：{}",
        out.text
    );
    let evidence = out.execution.expect("BashTool 必须带 typed evidence");
    assert_eq!(
        evidence.status,
        ToolExecutionStatus::TimedOut,
        "提升被拒 ⇒ 回落 TimedOut（成功提升是 RunningAfterTimeout）"
    );
    assert_eq!(evidence.task_id, None, "提升被拒 ⇒ 没有 task_id");

    // 收尾：占位条目按既有夹具的口径撤销（cancel + confirm），再把 manager 关干净。
    for index in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        let id = format!("occupied-{index}");
        manager.cancel(&id).expect("占位条目可取消");
        manager.confirm_external_execution_stopped(&id);
    }
    assert_eq!(manager.active_count(), 0, "占位条目必须全部撤销");
    let report = manager.shutdown().await;
    assert_eq!(
        manager.active_count(),
        0,
        "shutdown 之后不得残留活跃任务（报告：{report:?}）"
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
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
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
    let service = serve_client_auto(
        io,
        None,
        None,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await
    .expect("builtin 握手不得超时（同进程链路）")
    .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
    }
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

// ─── 线路：握手 + tools/list ───────────────────────────────────────────────────
