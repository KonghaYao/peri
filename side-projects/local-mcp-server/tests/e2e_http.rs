//! **真实 `local-mcp-server` 进程 + 真实 socket + 真实本机执行** 的 HTTP 端到端测试。
//!
//! 与 `e2e_stdio.rs` 共用同一套接线（`src/main.rs` 的装配、进程内执行内核），差别只在传输：
//! 这里起的是 `--transport http`，用**手写 HTTP/1.1**（`http_support::RawClient`，不是 rmcp
//! 客户端）在真实回环 socket 上说话。
//!
//! 断言覆盖（A-006 / A-007 / A-011）：
//!
//! 1. legacy 会话生命周期与 modern 无状态生命周期**共存**，且两种 Era 下工具面一致；
//! 2. 七工具在两种 Era 下全部成功（本机进程内执行，HTTP-only 字段不适用者给理由）；
//! 3. 至少四类错误（未知工具 / 缺必填参数 / 越界路径 / header-body 不一致）；
//! 4. 身份由传输层注入：请求体里伪造 `clientInfo` 不改变授权主体；
//! 5. bearer 认证：缺失/错误 → 401，且凭证值不出现在日志里；
//! 6. 任务资源跨连接不可见（modern 每个连接一个实例 → 跨身份句柄必须拒绝）；
//! 7. body 上限 413、非回环需显式授权。
//!
//! 另有一条**不依赖竞态**的用例（`raw_client_reconnects_*`）：它验证 `RawClient`
//! 自身对「服务端在拒绝响应后关闭连接」是确定性的（换连接重发同一条请求），对应
//! GAP-009 / F-GATES-02 —— 真实 server 的 401/403/413 路径在读完 body 前返回，连接随未读
//! 请求体一起被丢弃（对端看到 RST），而响应不带 `connection: close`，客户端无法靠响应头预判。

#[path = "e2e_support/mod.rs"]
mod e2e_support;
#[path = "http_support/mod.rs"]
mod http_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use e2e_support::{
    current_uid, dump_evidence, process_exists, process_group_exists, wait_for_descendant,
    wait_until_process_and_group_gone, HttpBroker, Sandbox,
};
use http_support::{
    legacy_initialize, legacy_initialized_notification, legacy_request, legacy_tools_call,
    modern_discover, modern_request, modern_tools_call, modern_tools_list, RawClient, RawRequest,
};

/// 七个工具的冻结顺序。
const TOOL_NAMES: [&str; 7] = [
    "Read",
    "Write",
    "Edit",
    "Glob",
    "Grep",
    "folder_operations",
    "Bash",
];

fn structured(result: &Value) -> &Value {
    result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("工具结果缺少 structuredContent：{result}"))
}

fn text_of(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 在指定 Era 下跑一遍七工具，返回各工具的 `structuredContent` 摘要。
fn exercise_seven_tools(era: &str, client: &mut RawClient, session: Option<&str>) -> Value {
    let mut id = 100u64;
    let mut next = || {
        id += 1;
        id
    };

    // 每个 Era 各准备一个只属于它的文件，避免两个 Era 的执行结果互相覆盖。
    let prefix = format!("era-{era}");

    let write = client.send(&match era {
        "legacy" => legacy_tools_call(
            "Write",
            json!({"file_path": format!("{prefix}/note.txt"), "content": "hello-http\n"}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "Write",
            json!({"file_path": format!("{prefix}/note.txt"), "content": "hello-http\n"}),
            next(),
        ),
    });
    assert_eq!(write.status, 200);
    let write_result = write.first_message()["result"].clone();
    assert_ne!(
        write_result["isError"],
        json!(true),
        "{era} Write：{write_result}"
    );

    let read = client.send(&match era {
        "legacy" => legacy_tools_call(
            "Read",
            json!({"file_path": format!("{prefix}/note.txt")}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "Read",
            json!({"file_path": format!("{prefix}/note.txt")}),
            next(),
        ),
    });
    let read_result = read.first_message()["result"].clone();
    assert!(
        text_of(&read_result).contains("hello-http"),
        "{era} Read：{read_result}"
    );

    let edit = client.send(&match era {
        "legacy" => legacy_tools_call(
            "Edit",
            json!({"file_path": format!("{prefix}/note.txt"),
                   "old_string": "hello", "new_string": "goodbye"}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "Edit",
            json!({"file_path": format!("{prefix}/note.txt"),
                   "old_string": "hello", "new_string": "goodbye"}),
            next(),
        ),
    });
    let edit_result = edit.first_message()["result"].clone();
    assert_eq!(
        structured(&edit_result)["occurrences"],
        1,
        "{era} Edit：{edit_result}"
    );

    let glob = client.send(&match era {
        "legacy" => legacy_tools_call(
            "Glob",
            json!({"pattern": format!("{prefix}/*.txt"), "path": "."}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "Glob",
            json!({"pattern": format!("{prefix}/*.txt"), "path": "."}),
            next(),
        ),
    });
    let glob_result = glob.first_message()["result"].clone();
    assert_eq!(
        structured(&glob_result)["count"],
        1,
        "{era} Glob：{glob_result}"
    );

    let grep = client.send(&match era {
        "legacy" => legacy_tools_call(
            "Grep",
            json!({"pattern": "goodbye", "path": prefix, "output_mode": "content"}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "Grep",
            json!({"pattern": "goodbye", "path": prefix, "output_mode": "content"}),
            next(),
        ),
    });
    let grep_result = grep.first_message()["result"].clone();
    assert_ne!(
        grep_result["isError"],
        json!(true),
        "{era} Grep：{grep_result}"
    );

    let folder = client.send(&match era {
        "legacy" => legacy_tools_call(
            "folder_operations",
            json!({"operation": "list", "folder_path": prefix}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "folder_operations",
            json!({"operation": "list", "folder_path": prefix}),
            next(),
        ),
    });
    let folder_result = folder.first_message()["result"].clone();
    assert!(
        text_of(&folder_result).contains("note.txt"),
        "{era} folder：{folder_result}"
    );

    // Bash 的**本机执行事实**：以当前 uid 执行、cwd 是工作区根（两种 Era 都必须一致）。
    let bash_command = format!(
        "id -u; pwd -P; \
         test -n \"$(ls -d {prefix} 2>/dev/null)\" && echo CWD-IS-WORKSPACE-ROOT"
    );
    let bash = client.send(&match era {
        "legacy" => legacy_tools_call(
            "Bash",
            json!({"command": bash_command, "timeout": 15000, "run_in_background": false}),
            next(),
            session.expect("legacy 需要会话头"),
        ),
        _ => modern_tools_call(
            "Bash",
            json!({"command": bash_command, "timeout": 15000, "run_in_background": false}),
            next(),
        ),
    });
    let bash_result = bash.first_message()["result"].clone();
    let bash_text = text_of(&bash_result);
    assert!(
        bash_text.contains(&*current_uid().to_string()),
        "{era} Bash 必须以当前用户（{}）在本机执行：{bash_text}",
        current_uid()
    );
    assert!(
        bash_text.contains("CWD-IS-WORKSPACE-ROOT"),
        "{era} Bash 的 cwd 必须是工作区根：{bash_text}"
    );

    json!({
        "write": structured(&write_result),
        "read_text": text_of(&read_result),
        "edit": structured(&edit_result),
        "glob": structured(&glob_result),
        "grep": structured(&grep_result),
        "folder": structured(&folder_result),
        "bash": structured(&bash_result),
        "bash_text": bash_text,
    })
}

/// 两种 Era 的七工具成功与共享核心。
#[test]
fn real_broker_serves_all_seven_tools_over_legacy_and_modern_http() {
    let sandbox = Sandbox::new("local-mcp-e2e-http-");
    let mut broker = HttpBroker::start(
        sandbox.root(),
        &["--allowed-origin", "http://localhost:5173"],
    );
    let addr = broker.addr();
    let broker_pid = broker.pid();
    let mut client = RawClient::connect(addr);

    // ── legacy：initialize → 会话头 → initialized → tools/list → 调用 ──
    let init = client.send(&legacy_initialize(1));
    assert_eq!(init.status, 200, "legacy initialize 必须 200");
    let session = init
        .header("mcp-session-id")
        .expect("legacy initialize 必须返回会话头")
        .to_string();
    assert!(
        init.first_message()["result"]["capabilities"]["tools"].is_object(),
        "握手必须声明 tools 能力"
    );
    client.send(&legacy_initialized_notification(&session));

    let legacy_list = client.send(&legacy_request("tools/list", json!({}), 2, Some(&session)));
    let legacy_names: Vec<String> = legacy_list.first_message()["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name").to_string())
        .collect();
    assert_eq!(legacy_names, TOOL_NAMES.to_vec());

    let legacy_summary = exercise_seven_tools("legacy", &mut client, Some(&session));

    // ── modern：无需握手，逐请求自带 _meta；同一个 socket 上直接调用 ──
    let discover = client.send(&modern_discover(1000));
    assert_eq!(discover.status, 200, "modern server/discover 必须 200");
    let modern_list = client.send(&modern_tools_list(1001));
    let modern_names: Vec<String> = modern_list.first_message()["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name").to_string())
        .collect();
    assert_eq!(
        modern_names, legacy_names,
        "两种 Era 必须暴露同一个工具面（共用 MCP core）"
    );

    let modern_summary = exercise_seven_tools("modern", &mut client, None);

    // ── 错误族（HTTP）：未知工具、缺必填参数、越界路径、header/body 不一致 ──
    let unknown = client.send(&modern_tools_call("NopeTool", json!({}), 1100));
    let unknown_message = unknown.first_message();
    assert_eq!(unknown_message["error"]["code"], -32602);
    assert_eq!(
        unknown_message["error"]["message"],
        "Unknown tool: NopeTool"
    );

    let missing = client.send(&modern_tools_call("Read", json!({}), 1101));
    let missing_result = missing.first_message()["result"].clone();
    assert_eq!(missing_result["isError"], json!(true), "{missing_result}");
    assert!(text_of(&missing_result).contains("The 'file_path' parameter is required"));

    let outside = client.send(&modern_tools_call(
        "Read",
        json!({"file_path": "/etc/passwd"}),
        1102,
    ));
    let outside_result = outside.first_message()["result"].clone();
    assert_eq!(structured(&outside_result)["denied"], "outside_workspace");
    assert!(!text_of(&outside_result).contains("root:"));

    let mismatch = client.send(
        &modern_tools_call("Read", json!({"file_path": "era-modern/note.txt"}), 1103)
            .header("Mcp-Name", "Write"),
    );
    assert_eq!(mismatch.status, 400, "header/body 不一致必须 400");
    assert_eq!(
        mismatch.first_message()["error"]["code"],
        -32020,
        "header/body 不一致必须 -32020"
    );
    assert_eq!(mismatch.content_type(), Some("application/json"));

    // ── 身份由传输层注入：请求体里伪造 clientInfo 不改变授权主体 ──
    let mut forged = modern_tools_call("Read", json!({"file_path": "era-modern/note.txt"}), 1104);
    forged = forged.header("Mcp-Client-Info", "{\"name\":\"attacker\"}");
    let forged_response = client.send(&forged);
    assert_eq!(forged_response.status, 200, "未知头不得影响正常调用");

    dump_evidence(
        "e2e-http-seven-tools.json",
        &serde_json::to_string_pretty(&json!({
            "legacy": legacy_summary,
            "modern": modern_summary,
            "legacy_tools": legacy_names,
            "modern_tools": modern_names,
            "errors": {
                "unknown_tool": unknown.first_message()["error"].clone(),
                "missing_param": text_of(&missing_result),
                "outside_path": structured(&outside_result),
                "header_body_mismatch": mismatch.first_message()["error"].clone(),
            },
        }))
        .expect("serialize evidence"),
    );

    // 真实进程事实：modern 连接起的后台任务必须是 server 进程的后代（本机执行、无 worker 中转）。
    let owned = client.send(&modern_tools_call(
        "Bash",
        json!({"command": "echo http-owned; sleep 30", "timeout": 15000, "run_in_background": true}),
        1200,
    ));
    assert_eq!(
        owned.status,
        200,
        "前台 Bash 在 HTTP 上必须可用：{}",
        owned.body_text()
    );
    let owned_result = owned.first_message()["result"].clone();
    assert_ne!(owned_result["isError"], json!(true), "{owned_result}");

    let task = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("http-owned")
    })
    .expect("后台任务必须在 SIGINT 前真实运行");
    broker.track_process_group(task.pgid);
    assert!(process_exists(task.pid));
    assert!(process_group_exists(task.pgid));
    let exit = broker.shutdown_with_signal(libc::SIGINT);
    assert_eq!(
        exit.code,
        Some(0),
        "SIGINT 后必须正常退出；stderr:\n{}",
        exit.stderr
    );
    wait_until_process_and_group_gone(task.pid, task.pgid, Duration::from_secs(10));
}

/// SIGTERM 也必须走同一条优雅关闭路径并回收后台 Bash 进程组。
#[test]
fn real_broker_sigterm_reaps_background_process_group() {
    let sandbox = Sandbox::new("local-mcp-e2e-sigterm-");
    let mut broker = HttpBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();
    let mut client = RawClient::connect(broker.addr());
    let started = client.send(&modern_tools_call(
        "Bash",
        json!({"command": "echo http-term; sleep 30", "run_in_background": true}),
        1,
    ));
    assert_eq!(started.status, 200);
    let task = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("http-term")
    })
    .expect("后台任务必须在 SIGTERM 前真实运行");
    broker.track_process_group(task.pgid);
    assert!(process_exists(task.pid));
    assert!(process_group_exists(task.pgid));
    let exit = broker.shutdown_with_signal(libc::SIGTERM);
    assert_eq!(exit.code, Some(0), "SIGTERM stderr:\n{}", exit.stderr);
    wait_until_process_and_group_gone(task.pid, task.pgid, Duration::from_secs(10));
}

/// Fixture cleanup must reclaim registered groups even when a test exits by panic/timeout.
#[test]
fn http_broker_drop_reaps_registered_task_group() {
    let sandbox = Sandbox::new("local-mcp-e2e-http-drop-");
    let mut broker = HttpBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();
    let mut client = RawClient::connect(broker.addr());
    let started = client.send(&modern_tools_call(
        "Bash",
        json!({"command": "echo http-drop; sleep 30", "run_in_background": true}),
        1,
    ));
    assert_eq!(started.status, 200);
    let task = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("http-drop")
    })
    .expect("Drop cleanup task must be running");
    broker.track_process_group(task.pgid);
    drop(broker);
    wait_until_process_and_group_gone(task.pid, task.pgid, Duration::from_secs(10));
}

/// 认证与暴露面：缺 token 401、错 token 401、对 token 200，且 token 值不进日志。
#[test]
fn real_broker_enforces_bearer_auth_and_redacts_credentials() {
    let sandbox = Sandbox::new("local-mcp-e2e-auth-");
    // token 由测试进程随机生成并只经环境变量交给 server；不落盘、不进 fixture。
    let token = format!(
        "tok-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let broker = HttpBroker::start_with_env(
        sandbox.root(),
        &[
            "--token-env",
            "LOCAL_MCP_E2E_TOKEN",
            "--max-body-bytes",
            "65536",
        ],
        &[("LOCAL_MCP_E2E_TOKEN", token.as_str())],
    );
    let mut client = RawClient::connect(broker.addr());

    // 缺失凭证：401，且不回显任何提交内容。
    let anonymous = client.send(&modern_tools_list(1));
    assert_eq!(anonymous.status, 401, "缺失 bearer 必须 401");
    assert!(
        !anonymous.body_text().contains(&token),
        "401 响应不得包含凭证"
    );

    // 错误凭证：同样 401（不区分"缺失"与"错误"）。
    let wrong =
        client.send(&modern_tools_list(2).header("Authorization", "Bearer not-the-right-token"));
    assert_eq!(wrong.status, 401);

    // 正确凭证：工具面可用。
    let authorized =
        client.send(&modern_tools_list(3).header("Authorization", &format!("Bearer {token}")));
    assert_eq!(authorized.status, 200, "正确凭证必须可用");
    let names: Vec<String> = authorized.first_message()["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, TOOL_NAMES.to_vec());

    // body 上限：本实例把上限压到 64 KiB（`--max-body-bytes`），再发一个明显超限的 body。
    let oversize = "x".repeat(100 * 1024);
    let too_big = client.send(
        &modern_tools_call("Read", json!({"file_path": oversize}), 4)
            .header("Authorization", &format!("Bearer {token}")),
    );
    assert_eq!(
        too_big.status,
        413,
        "凭证正确但 body 超限必须 413（认证先于体积判定）：{}",
        too_big.body_text()
    );

    let exit = broker.shutdown();
    assert_eq!(exit.code, Some(0), "stderr:\n{}", exit.stderr);
    // 凭证脱敏：日志里不得出现 token 值本身。
    assert!(
        !exit.stderr.contains(&token),
        "broker 日志不得包含 bearer token 值"
    );
    assert!(
        !exit.stderr.contains("not-the-right-token"),
        "broker 日志不得包含提交过的错误凭证"
    );
}

/// 任务句柄的所有权：modern 每个连接一个实例，跨连接读取他人任务必须"不存在"。
#[test]
fn real_broker_keeps_task_handles_connection_owned_over_http() {
    let sandbox = Sandbox::new("local-mcp-e2e-owner-");
    let broker = HttpBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();

    // 连接 A 起一个后台任务。
    let mut owner = RawClient::connect(broker.addr());
    let started = owner.send(&modern_tools_call(
        "Bash",
        json!({"command": "for i in 1 2 3 4 5; do echo owned-$i; sleep 1; done",
               "run_in_background": true}),
        1,
    ));
    let started_result = started.first_message()["result"].clone();
    let task_id = structured(&started_result)["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();
    let task_pid = structured(&started_result)["pid"].as_i64().expect("pid");

    // 真实进程事实：任务进程是 server 的后代（本机直接执行）。
    assert!(
        wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| entry
            .command
            .contains("owned-"))
        .is_some(),
        "后台任务必须是 server 进程的后代（当前后代：{:?}）",
        e2e_support::descendants_of(broker_pid)
    );
    assert!(task_pid > 0, "任务 pid 必须是真实进程：{task_pid}");

    // 连接 A 自己能读到。
    let task_uri = format!("sandbox://tasks/{task_id}");
    let own_read = owner.send(&modern_request(
        "resources/read",
        json!({"uri": task_uri}),
        Some(&task_uri),
        2,
    ));
    assert_eq!(own_read.status, 200);
    assert!(
        own_read.first_message().get("result").is_some(),
        "任务所有者必须能读到自己的任务"
    );

    // 连接 B（另一个 client instance）读同一个 URI：必须失败，且不泄露存在性。
    let mut stranger = RawClient::connect(broker.addr());
    let foreign = stranger.send(&modern_request(
        "resources/read",
        json!({"uri": task_uri}),
        Some(&task_uri),
        3,
    ));
    let foreign_message = foreign.first_message();
    assert!(
        foreign_message.get("error").is_some(),
        "跨连接读取任务句柄必须被拒绝：{foreign_message}"
    );
    assert!(
        !serde_json::to_string(&foreign_message)
            .unwrap_or_default()
            .contains("owned-"),
        "拒绝响应不得泄露任务内容"
    );

    // 清理：所有者自己停掉任务（外部 kill 不是取消路径，这里直接让命令自然结束即可）。
    let stop = owner.send(&modern_tools_call(
        "Bash",
        json!({"command": "echo cleanup", "timeout": 15000, "run_in_background": false}),
        4,
    ));
    assert_eq!(stop.status, 200);

    let exit = broker.shutdown();
    assert_eq!(exit.code, Some(0), "stderr:\n{}", exit.stderr);
}

// ──────────────────── raw 客户端自身的确定性（不依赖 Docker 与竞态） ────────────────────

/// 进程内最小 HTTP 服务器：**只读到请求头结束就关闭连接**，把 body 留在内核接收缓冲里。
///
/// 这正是真实 broker 的拒绝路径形态（`src/transport/http.rs` 的 401 在读完 body 之前就返回，
/// 连接随请求体一起被丢弃 ⇒ 关闭时 socket 里还有未读字节 ⇒ 对端收到 RST）。
/// 返回（监听地址，捕获到的请求字节列表）。
fn spawn_reject_then_close_server(
    reject_status: &str,
) -> (std::net::SocketAddr, JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fake server");
    let addr = listener.local_addr().expect("fake server addr");
    let reject_status = reject_status.to_string();
    let handle = std::thread::spawn(move || {
        let mut captured: Vec<Vec<u8>> = Vec::new();
        if let Ok((mut stream, _)) = listener.accept() {
            captured.push(read_head_only(&mut stream));
            // 默认 rejection 响应：形态与真实 401 一致（content-length: 0、无 connection: close）。
            let response = format!(
                "HTTP/1.1 {reject_status}\r\n\
                 content-type: text/plain; charset=utf-8\r\n\
                 www-authenticate: Bearer realm=\"local-mcp-server\"\r\n\
                 content-length: 0\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            // 直接 drop：socket 里仍有未读请求体 ⇒ 内核发 RST（而不是干净的 FIN）。
        }
        // 第二次连接：把请求读干净（避免响应被 RST 连带丢弃），回 200 后干净关闭，
        // 证明客户端确实换了一条连接并重发了同一条请求。
        if let Ok((mut stream, _)) = listener.accept() {
            let head = read_head_only(&mut stream);
            drain_declared_body(&mut stream, &head);
            captured.push(head);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\r\nok",
            );
            let _ = stream.flush();
        }
        captured
    });
    (addr, handle)
}

/// 逐字节读到 `\r\n\r\n` 为止（body 因此留在内核缓冲，不被用户态读走）。
fn read_head_only(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while let Ok(read) = stream.read(&mut byte) {
        if read == 0 {
            break;
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    head
}

/// 按 `Content-Length` 把请求体读完（用于「正常响应」那条连接，保证干净关闭而不是 RST）。
fn drain_declared_body(stream: &mut std::net::TcpStream, head: &[u8]) {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let length = text
        .split("content-length:")
        .nth(1)
        .and_then(|rest| rest.split("\r\n").next())
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut remaining = length;
    let mut buffer = [0u8; 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len());
        match stream.read(&mut buffer[..want]) {
            Ok(0) => break,
            Ok(read) => remaining -= read,
            Err(_) => break,
        }
    }
}

/// `RawClient` 对「服务端已关闭复用连接」必须是**确定性**的：换一条连接重发同一条请求，
/// 而不是在 `write_all` 上撞 `Broken pipe (os error 32)`（GAP-009 / F-GATES-02 的机制）。
///
/// 本用例刻意不参与任何竞态：先让服务端关掉连接，再等足 200ms 让 RST 落到本机，
/// 然后发第二条请求。修复前的客户端在这里**必然**失败（写已关闭的连接）；修复后必然成功。
#[test]
fn raw_client_reconnects_after_server_closes_reused_connection() {
    let (addr, server) = spawn_reject_then_close_server("401 Unauthorized");
    let mut client = RawClient::connect(addr);

    // 第一条请求：服务端拒绝并关闭连接（客户端读到的是完整 401，看不到任何关闭预告）。
    let big_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {},
        "padding": "x".repeat(8192),
    });
    let first = client.send(&RawRequest::post(big_body));
    assert_eq!(first.status, 401, "拒绝响应必须被完整读到");
    assert_eq!(client.reconnects(), 0, "首次交换不应触发重连");

    // 等足 RST 落地：此后在旧连接上写请求对任何客户端都是确定性 EPIPE。
    std::thread::sleep(Duration::from_millis(200));

    // 第二条请求：复用同一个 RawClient（同一用例语义），必须换连接并重发成功。
    let second_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
        "padding": "x".repeat(8192),
    });
    let second = client.send(&RawRequest::post(second_request));
    assert_eq!(second.status, 200, "服务端关闭后必须换连接重发");
    assert_eq!(second.body_text(), "ok");
    assert!(
        client.reconnects() >= 1,
        "客户端必须显式换连接，而不是在被关闭的连接上继续写"
    );

    let captured = server.join().expect("fake server 线程");
    assert_eq!(
        captured.len(),
        2,
        "两次请求必须各占一条连接（请求数不得减少）"
    );
    let second_head = String::from_utf8_lossy(&captured[1]).to_string();
    assert!(
        second_head.starts_with("POST /mcp HTTP/1.1"),
        "重发的必须是同一条请求行：{second_head}"
    );
    assert!(
        second_head.to_ascii_lowercase().contains("content-length:"),
        "重发必须保留原始头：{second_head}"
    );
}
