//! **真实 `local-mcp-server` 进程 + 真实本机执行** 的 stdio 端到端测试。
//!
//! 本文件是 R-002/R-006/A-005/A-007/A-009 的端到端证据：它启动 `src/main.rs` 编出的
//! 二进制，让它按真实启动顺序做「配置校验 → 工作区根 → 进程内执行内核 → stdio 服务
//! 手写 JSON-RPC」。断言覆盖：
//!
//! 1. 七工具**全部成功**，副作用直接落在宿主工作区（本机执行，不是挂载面）；
//! 2. 至少六类错误（未知工具、缺必填参数、越界路径、根内不存在、符号链接逃逸、非零退出码）；
//! 3. 执行形态是**本机单进程**：Bash 以当前 uid 在工作区根 cwd 执行、
//!    工作区根外路径**可达**（根是能力边界，不是安全边界）、服务进程空闲时**没有子进程**；
//! 4. Bash 全生命周期：前台、显式后台、资源查询、日志读取、停止、超时提升、TTL 回收；
//! 5. 关闭语义：EOF → 退出码 0、无残留子进程；
//! 6. 启动 fail closed：工作区根非法即拒绝服务，且 stdout 上不产出任何协议字节；
//! 7. 不依赖 Docker：把含 `docker` 的目录从 PATH 剔除后七工具照常工作。
//!
//! 手写 JSON-RPC 文本（不用 rmcp 客户端）是为了让"断言的是真 wire"由构造保证。

#[path = "e2e_support/mod.rs"]
mod e2e_support;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use e2e_support::{
    children_of, current_uid, dump_evidence, path_has_docker, path_without_docker, process_exists,
    process_group_exists, wait_for_descendant, wait_until_no_children,
    wait_until_process_and_group_gone, Sandbox, StdioBroker,
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

/// 从工具结果里取 `structuredContent`。
fn structured(result: &Value) -> &Value {
    result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("工具结果缺少 structuredContent：{result}"))
}

/// 从工具结果里取文本。
fn text_of(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("工具结果缺少文本块：{result}"))
        .to_string()
}

/// 断言工具成功（`isError` 不为 true）。
fn assert_ok(result: &Value, tool: &str) {
    assert_ne!(
        result.get("isError"),
        Some(&Value::Bool(true)),
        "{tool} 不应失败：{result}"
    );
}

/// 断言工具业务失败。
fn assert_tool_error(result: &Value, tool: &str) {
    assert_eq!(
        result.get("isError"),
        Some(&Value::Bool(true)),
        "{tool} 应为工具业务失败：{result}"
    );
}

/// 读取任务资源载荷（JSON 文本 → Value）。
fn task_snapshot(broker: &mut StdioBroker, id: u64, task_id: &str) -> Value {
    let message = broker.read_resource(id, &format!("sandbox://tasks/{task_id}"));
    let result = message
        .get("result")
        .unwrap_or_else(|| panic!("resources/read 失败：{message}"));
    let text = result
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("资源载荷缺少文本：{result}"));
    serde_json::from_str(text).unwrap_or_else(|err| panic!("资源载荷不是 JSON：{err}\n{text}"))
}

/// 轮询任务资源直到状态满足条件（或超时）。
fn wait_for_status(
    broker: &mut StdioBroker,
    task_id: &str,
    wanted: &[&str],
    timeout: Duration,
) -> Value {
    let deadline = Instant::now() + timeout;
    let mut last = Value::Null;
    let mut id = 900u64;
    while Instant::now() < deadline {
        id += 1;
        last = task_snapshot(broker, id, task_id);
        let status = last
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if wanted.contains(&status.as_str()) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("任务 {task_id} 在 {timeout:?} 内未进入 {wanted:?}；最后快照：{last}");
}

/// 七工具 + 六类错误 + **本机执行事实**（uid / cwd / 根外可见 / 无子进程）。
#[test]
fn real_broker_executes_all_seven_tools_and_error_families_over_stdio() {
    let sandbox = Sandbox::new("local-mcp-e2e-stdio-");
    sandbox.write("notes/hello.txt", "alpha\nbeta\nneedle-in-file\ngamma\n");
    sandbox.write("notes/sub/deep.md", "# deep\nneedle-in-markdown\n");
    // 越界符号链接：根内路径指向根外文件，必须被 capability 拒绝。
    std::os::unix::fs::symlink("/etc/passwd", sandbox.host_path("escape-link")).expect("symlink");

    let mut broker = StdioBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();
    let root = sandbox.canonical_root();
    let parent = sandbox.parent_dir();

    // ① 生命周期：legacy 握手协商到 2025-11-25。
    let init = broker.legacy_handshake();
    let info = &init["result"]["serverInfo"];
    assert_eq!(info["name"], "local-mcp-server");
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        false
    );

    // ② 执行形态：握手后处于空闲，服务进程**没有任何子进程**（不存在 worker 子进程）。
    let idle_children = wait_until_no_children(broker_pid, Duration::from_secs(5));
    assert!(
        idle_children.is_empty(),
        "单进程形态下空闲时不得有子进程（无 worker/无容器运行时）：{idle_children:?}"
    );

    // ③ 工具面：恰好七个，顺序冻结。
    let listed = broker.request(2, "tools/list", json!({}));
    let names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name").to_string())
        .collect();
    assert_eq!(names, TOOL_NAMES.to_vec(), "tools/list 必须恰好七个工具");

    // ④ Read：相对路径与**根内绝对路径**（规范形态）都必须能读到同一文件。
    let read_relative = broker.tool_result(10, "Read", json!({"file_path": "notes/hello.txt"}));
    assert_ok(&read_relative, "Read");
    assert!(text_of(&read_relative).contains("needle-in-file"));
    assert_eq!(structured(&read_relative)["ok"], true);
    assert!(
        text_of(&read_relative).contains("     3\t"),
        "行号必须是 1-based 制表位格式"
    );
    // 绝对路径必须写成**规范根**下的形态：产品启动时把 `--workspace` 解析成真实路径，
    // 授权判定按组件逐段比较（不做 FS 访问），因此别名拼写（如 macOS 的 `/var` →
    // `/private/var`）会被判为根外——拒绝方向是 fail-closed，不是放行。
    let absolute_in_root = root.join("notes/hello.txt");
    let read_absolute = broker.tool_result(
        11,
        "Read",
        json!({"file_path": absolute_in_root.to_string_lossy(), "offset": 2, "limit": 1}),
    );
    assert_ok(&read_absolute, "Read");
    assert!(text_of(&read_absolute).contains("beta"));
    assert!(
        !text_of(&read_absolute).contains("needle-in-file"),
        "limit=1 只读一行"
    );
    // 同一文件经**未解析别名**拼写（非规范根）请求：必须被拒，且不得泄露内容。
    let aliased = sandbox.host_path("notes/hello.txt");
    if aliased != absolute_in_root {
        let denied_alias =
            broker.tool_result(110, "Read", json!({"file_path": aliased.to_string_lossy()}));
        assert_tool_error(&denied_alias, "Read");
        assert_eq!(structured(&denied_alias)["denied"], "outside_workspace");
        assert!(
            !text_of(&denied_alias).contains("needle-in-file"),
            "被拒的别名请求不得回显内容：{denied_alias}"
        );
    }

    // ⑤ Write：副作用直接落在宿主工作区（本机执行）。
    let written = broker.tool_result(
        12,
        "Write",
        json!({"file_path": "out/written.txt", "content": "written-locally\n"}),
    );
    assert_ok(&written, "Write");
    assert_eq!(sandbox.read("out/written.txt"), "written-locally\n");
    assert_eq!(structured(&written)["total_lines"], 1);

    // ⑥ Edit：读-改-写事务，替换计数与宿主内容都要对齐。
    let edited = broker.tool_result(
        13,
        "Edit",
        json!({"file_path": "out/written.txt", "old_string": "locally", "new_string": "in-place"}),
    );
    assert_ok(&edited, "Edit");
    assert_eq!(structured(&edited)["occurrences"], 1);
    assert_eq!(sandbox.read("out/written.txt"), "written-in-place\n");

    // ⑦ Glob：`**` 语义 + 排序稳定。
    let globbed = broker.tool_result(14, "Glob", json!({"pattern": "**/*.md", "path": "."}));
    assert_ok(&globbed, "Glob");
    assert_eq!(structured(&globbed)["count"], 1);
    assert!(text_of(&globbed).ends_with("notes/sub/deep.md"));

    // ⑧ Grep：别名 + 四输出模式之一。
    //    别名优先级按源实现：**语义字段优先**（`show_line_numbers` 存在时忽略 `-n`）。
    let alias_only = broker.tool_result(
        15,
        "Grep",
        json!({"pattern": "needle-in-(file|markdown)", "path": ".",
               "output_mode": "content", "-n": true}),
    );
    assert_ok(&alias_only, "Grep");
    let alias_text = text_of(&alias_only);
    assert!(
        alias_text.contains("notes/hello.txt:3: needle-in-file"),
        "别名 -n 在无语义字段时必须生效：{alias_text}"
    );
    let semantic_wins = broker.tool_result(
        150,
        "Grep",
        json!({"pattern": "needle-in-(file|markdown)", "path": ".",
               "output_mode": "content", "-n": true, "show_line_numbers": false}),
    );
    assert_ok(&semantic_wins, "Grep");
    let semantic_text = text_of(&semantic_wins);
    assert!(
        semantic_text.contains("notes/hello.txt: needle-in-file"),
        "语义字段必须优先于别名（源 `get(\"show_line_numbers\").or_else(|| get(\"-n\"))`）：{semantic_text}"
    );
    assert!(
        semantic_text.contains("needle-in-markdown"),
        "多文件命中必须都返回：{semantic_text}"
    );

    // ⑨ folder_operations：exists + list 两种 operation。
    let folder = broker.tool_result(
        16,
        "folder_operations",
        json!({"operation": "list", "folder_path": "notes"}),
    );
    assert_ok(&folder, "folder_operations");
    assert!(text_of(&folder).contains("hello.txt"));
    let exists = broker.tool_result(
        17,
        "folder_operations",
        json!({"operation": "exists", "folder_path": "notes/sub", "recursive": true,
               "max_depth": 3}),
    );
    assert_ok(&exists, "folder_operations");
    assert_eq!(structured(&exists)["exists"], true);

    // ⑩ Bash 的**本机执行事实**（A-009）：当前 uid、工作区根 cwd、根外可达。
    //    三件事都必须由真实命令回读，而不是由产品自述。
    let parent_str = parent.to_string_lossy().to_string();
    let bash = broker.tool_result(
        18,
        "Bash",
        json!({"command": format!(
            "id -u; pwd -P; ls {parent_str} >/dev/null 2>&1 && echo OUTSIDE-ROOT-VISIBLE; \
             test -e /etc/passwd && echo HOST-DEVICE-VISIBLE"
        ),
        "timeout": 15000,
        "run_in_background": false}),
    );
    assert_ok(&bash, "Bash");
    let bash_text = text_of(&bash);
    assert!(
        bash_text.contains(&format!("\n{}", current_uid()))
            || bash_text.starts_with(&current_uid().to_string()),
        "Bash 必须以**当前用户**（{}）执行：{bash_text}",
        current_uid()
    );
    assert!(
        bash_text.contains(&*root.to_string_lossy()),
        "Bash 的 cwd 必须是工作区根 {}：{bash_text}",
        root.display()
    );
    assert!(
        bash_text.contains("OUTSIDE-ROOT-VISIBLE"),
        "工作区根是**能力边界不是安全边界**：根外路径对 Bash 必须可达（诚实声明的事实面）：{bash_text}\n（被测根外目录：{parent_str}）"
    );
    assert!(
        bash_text.contains("HOST-DEVICE-VISIBLE"),
        "本机执行下宿主设备/系统文件可见：{bash_text}"
    );
    assert_eq!(structured(&bash)["exit_code"], 0);
    assert_eq!(structured(&bash)["status"], "completed");

    // ⑪ 文件类工具受根边界约束：Bash 能读根外，Read 不能（能力边界只作用于文件类工具）。
    let outside_bash = broker.tool_result(
        19,
        "Bash",
        json!({"command": "head -c 1 /etc/hosts >/dev/null && echo HOST-FILE-READABLE",
               "timeout": 15000,
               "run_in_background": false}),
    );
    assert_ok(&outside_bash, "Bash");
    assert!(
        text_of(&outside_bash).contains("HOST-FILE-READABLE"),
        "Bash 不受根边界限制（D-003 的诚实事实）：{outside_bash}"
    );

    // ⑫ 错误族（六类，彼此可区分）。
    //   E1：未知工具 → JSON-RPC -32602，且不进 tool result。
    let unknown = broker.call_tool(20, "DefinitelyNotATool", json!({}));
    assert_eq!(unknown["error"]["code"], -32602);
    assert_eq!(
        unknown["error"]["message"],
        "Unknown tool: DefinitelyNotATool"
    );

    //   E2：缺必填参数 → 工具业务失败，文案来自源实现。
    let missing = broker.tool_result(21, "Read", json!({}));
    assert_tool_error(&missing, "Read");
    assert!(
        text_of(&missing).contains("The 'file_path' parameter is required"),
        "{}",
        text_of(&missing)
    );

    //   E3：越界绝对路径 → 工具业务失败 + 结构化标记，且不回显根外内容。
    let outside = broker.tool_result(22, "Read", json!({"file_path": "/etc/passwd"}));
    assert_tool_error(&outside, "Read");
    assert_eq!(structured(&outside)["denied"], "outside_workspace");
    assert!(
        !text_of(&outside).contains("root:"),
        "不得通过文件类工具泄露根外内容"
    );

    //   E4：根内不存在的文件 → 工具业务失败（与"越界"可区分）。
    let not_found = broker.tool_result(23, "Read", json!({"file_path": "notes/nope.txt"}));
    assert_tool_error(&not_found, "Read");
    assert!(
        text_of(&not_found).contains("File not found at notes/nope.txt"),
        "不存在的文件必须给出源文案：{}",
        text_of(&not_found)
    );

    //   E5：根内符号链接指向根外 → 拒绝，不跟随（文案来自 capability 判定）。
    let symlink = broker.tool_result(24, "Read", json!({"file_path": "escape-link"}));
    assert_tool_error(&symlink, "Read");
    assert!(
        text_of(&symlink).contains("resolves outside the authorized workspace through a symlink"),
        "符号链接逃逸必须被拒绝且原因可诊断：{symlink}"
    );
    assert!(!text_of(&symlink).contains("root:"), "不得泄露链接目标内容");

    //   E6：非零退出码不是 JSON-RPC 错误，而是带 exit_code 的工具结果（源语义）。
    let failing = broker.tool_result(
        25,
        "Bash",
        json!({"command": "echo before-failure; exit 7",
               "timeout": 15000, "run_in_background": false}),
    );
    assert_ok(&failing, "Bash");
    assert_eq!(structured(&failing)["exit_code"], 7);
    assert!(
        text_of(&failing).contains("[Exit code: 7]"),
        "{}",
        text_of(&failing)
    );

    // ⑬ 关闭：EOF → 退出码 0，且**无残留子进程**（前台命令都已回收）。
    assert!(
        wait_until_no_children(broker_pid, Duration::from_secs(10)).is_empty(),
        "前台 Bash 完成后不得留下子进程"
    );
    let exit = broker.shutdown();
    assert_eq!(
        exit.code,
        Some(0),
        "EOF 必须是正常退出；stderr:\n{}",
        exit.stderr
    );
    assert!(
        !exit.stderr.contains("docker"),
        "单进程本机形态的日志不得出现 docker：{}",
        exit.stderr
    );

    dump_evidence(
        "e2e-stdio-seven-tools.json",
        &serde_json::to_string_pretty(&json!({
            "tools_list": names,
            "read_text": text_of(&read_relative),
            "write_host_effect": sandbox.read("out/written.txt"),
            "bash_local_execution_probe": bash_text,
            "bash_outside_root_probe": text_of(&outside_bash),
            "idle_children": Vec::<String>::new(),
            "errors": {
                "unknown_tool": unknown["error"].clone(),
                "missing_param": text_of(&missing),
                "outside_path": structured(&outside),
                "not_found": text_of(&not_found),
                "symlink_escape": structured(&symlink),
                "non_zero_exit": structured(&failing)["exit_code"].clone(),
            },
            "exit_code": exit.code,
        }))
        .expect("serialize evidence"),
    );
}

/// Bash 全生命周期：前台/后台/查询/日志/停止/超时提升/TTL，全部在**本机进程内**。
#[test]
fn real_broker_runs_full_bash_lifecycle_over_stdio() {
    let sandbox = Sandbox::new("local-mcp-e2e-bash-");
    let mut broker = StdioBroker::start(
        sandbox.root(),
        // TTL 压到 3 秒，才能在测试里观测到终态任务的回收（生产默认 1h）。
        &["--task-ttl-secs", "3"],
    );
    let broker_pid = broker.pid();
    let root = sandbox.canonical_root();
    broker.legacy_handshake();

    // ① 前台命令：立即完成，不注册任务。
    let foreground = broker.tool_result(
        10,
        "Bash",
        json!({"command": "echo foreground-ok", "timeout": 15000, "run_in_background": false}),
    );
    assert_ok(&foreground, "Bash");
    assert!(text_of(&foreground).contains("foreground-ok"));
    assert_eq!(structured(&foreground)["status"], "completed");
    assert_eq!(structured(&foreground)["exit_code"], 0);
    assert!(
        structured(&foreground).get("task_id").is_none()
            || structured(&foreground)["task_id"].is_null(),
        "前台完成不应注册任务：{}",
        structured(&foreground)
    );

    // ② 显式后台：返回 task_id/pid/日志路径。
    let background = broker.tool_result(
        11,
        "Bash",
        json!({"command": "for i in 1 2 3 4 5 6 7 8 9 10; do echo tick-$i; sleep 1; done",
               "run_in_background": true}),
    );
    assert_ok(&background, "Bash");
    let task_id = structured(&background)["task_id"]
        .as_str()
        .expect("后台任务必须有 task_id")
        .to_string();
    let pid = structured(&background)["pid"].as_i64().expect("pid");
    let stdout_log = structured(&background)["stdout_log"]
        .as_str()
        .expect("日志路径")
        .to_string();
    assert!(
        text_of(&background).contains(&format!("kill {pid}")),
        "返回文本必须给出可执行的控制路径：{}",
        text_of(&background)
    );

    // ③ 真实进程事实：后台任务真的是**服务进程的子进程**（进程内执行、无 worker 中转）。
    let task_process = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("tick-")
    })
    .unwrap_or_else(|| {
        panic!(
            "后台任务进程必须是 server({broker_pid}) 的后代；当前后代：{:?}",
            e2e_support::descendants_of(broker_pid)
        )
    });
    assert_ne!(
        task_process.pid as i64, 0,
        "任务 pid 必须是真实进程：{task_process:?}"
    );
    assert_eq!(
        task_process.pid as i64, pid,
        "上报的 pid 必须就是进程表里的那个 bash 进程（无中间进程）：{task_process:?}"
    );

    // ④ 资源查询：状态为 running，且句柄属于本连接。
    let running = wait_for_status(&mut broker, &task_id, &["running"], Duration::from_secs(20));
    assert_eq!(running["task_id"], task_id.as_str());
    assert_eq!(running["pid"], pid);

    // ⑤ 日志：两条可访问路径都必须真的可用，且都是**宿主路径单表示**——
    //    (a) Bash 结果里的路径；(b) 任务资源里的路径（必须与 (a) 同形）。
    let resource_log = running["stdout_log"]
        .as_str()
        .expect("任务资源必须给出日志路径")
        .to_string();
    assert_eq!(
        resource_log, stdout_log,
        "宿主路径单表示：Bash 结果与任务资源必须给出同一条日志路径"
    );
    assert!(
        Path::new(&resource_log).starts_with(&root),
        "日志路径必须在工作区根内（宿主路径）：{resource_log}"
    );
    assert!(
        resource_log.contains(".local-mcp/logs"),
        "日志落在本产品私有目录内：{resource_log}"
    );
    let mut log_text = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let read = broker.tool_result(12, "Read", json!({"file_path": stdout_log}));
        if read.get("isError") != Some(&Value::Bool(true)) {
            log_text = text_of(&read);
            if log_text.contains("tick-2") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(
        log_text.contains("tick-"),
        "日志必须可读且持续追加：{log_text}"
    );
    let read_via_resource = broker.tool_result(120, "Read", json!({"file_path": resource_log}));
    assert_ne!(
        read_via_resource.get("isError"),
        Some(&Value::Bool(true)),
        "资源里的路径必须能直接喂给 Read：{read_via_resource}"
    );
    assert!(text_of(&read_via_resource).contains("tick-"));

    // ⑥ 停止：不改 Bash schema，用返回文本给出的进程组信号。
    //    注意状态语义：外部 `kill` 让进程被**信号**终止，因此终态是 `failed`
    //    （`killed` 只出现在注册表按 owner 意图取消的路径上）。
    let stop = broker.tool_result(
        13,
        "Bash",
        json!({"command": format!("kill -- -{pid} && echo STOP-SENT"),
               "timeout": 15000, "run_in_background": false}),
    );
    assert_ok(&stop, "Bash");
    assert!(text_of(&stop).contains("STOP-SENT"), "{}", text_of(&stop));
    let terminal = wait_for_status(
        &mut broker,
        &task_id,
        &["killed", "completed", "failed", "timed_out"],
        Duration::from_secs(30),
    );
    assert!(
        matches!(
            terminal["status"].as_str(),
            Some("failed") | Some("killed") | Some("completed")
        ),
        "被信号停止的任务必须进入终态：{terminal}"
    );
    // 停止后进程组不得留下 `sleep` 子孙（TERM→KILL 清理真的发生了）。
    let leftovers = wait_until_no_children(broker_pid, Duration::from_secs(10))
        .into_iter()
        .filter(|entry| entry.command.contains("tick-") || entry.command.contains("sleep"))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "停止后不得残留任务进程：{leftovers:?}"
    );

    // ⑦ 前台超时 → 自动提升为后台任务（源语义：进程继续存活，响应是工具错误）。
    let promoted = broker.tool_result(
        14,
        "Bash",
        json!({"command": "sleep 30", "timeout": 1200, "run_in_background": false}),
    );
    assert_tool_error(&promoted, "Bash");
    let promoted_text = text_of(&promoted);
    assert!(
        promoted_text.contains("promoted to a background task"),
        "超时必须提升为后台任务：{promoted_text}"
    );
    let promoted_id = structured(&promoted)["task_id"]
        .as_str()
        .expect("提升后必须有 task_id")
        .to_string();
    let promoted_pid = structured(&promoted)["pid"].as_i64().expect("pid");
    let promoted_snapshot = wait_for_status(
        &mut broker,
        &promoted_id,
        &["running", "completed", "killed", "failed", "timed_out"],
        Duration::from_secs(20),
    );
    assert_eq!(
        promoted_snapshot["pid"], promoted_pid,
        "提升后的任务资源必须指向同一个进程：{promoted_snapshot}"
    );
    assert_eq!(
        promoted_snapshot["status"], "running",
        "提升出的进程在未停止前必须保持运行：{promoted_snapshot}"
    );

    // 清理提升出来的进程（否则它会一直占着任务槽到测试结束）。
    let cleanup = broker.tool_result(
        15,
        "Bash",
        json!({"command": format!("kill -- -{promoted_pid} 2>/dev/null; echo CLEANUP"),
               "timeout": 15000, "run_in_background": false}),
    );
    assert_ok(&cleanup, "Bash");

    // ⑧ 终态任务在 TTL 到期后被回收：资源不再可读（fail closed，不泄露"曾经存在"）。
    let short_task = broker.tool_result(
        16,
        "Bash",
        json!({"command": "echo short-lived", "run_in_background": true}),
    );
    let short_id = structured(&short_task)["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();
    let completed = wait_for_status(
        &mut broker,
        &short_id,
        &["completed", "failed", "killed", "timed_out"],
        Duration::from_secs(20),
    );
    assert_eq!(completed["status"], "completed");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut reaped = false;
    let mut reaper_id = 700u64;
    while Instant::now() < deadline {
        reaper_id += 1;
        let message = broker.read_resource(reaper_id, &format!("sandbox://tasks/{short_id}"));
        if message.get("error").is_some() {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(reaped, "终态任务在 TTL（3s）后必须不再可读（回收即不可见）");

    let exit = broker.shutdown();
    assert_eq!(exit.code, Some(0), "stderr:\n{}", exit.stderr);

    dump_evidence(
        "e2e-stdio-bash-lifecycle.json",
        &serde_json::to_string_pretty(&json!({
            "foreground": structured(&foreground),
            "background": structured(&background),
            "task_process": task_process.command,
            "running_snapshot": running,
            "log_path_shared_form": resource_log,
            "log_excerpt": log_text,
            "stop": text_of(&stop),
            "terminal_snapshot": terminal,
            "promotion": structured(&promoted),
            "ttl_reaped": reaped,
        }))
        .expect("serialize evidence"),
    );
}

/// 启动 fail closed（单进程形态）：工作区根非法即拒绝服务，stdout 上不产出任何协议字节。
///
/// 本用例取代容器期的"镜像身份不符即拒绝服务"：那一条随 D-003 失去对象（没有镜像），
/// 但"启动期拒绝必须发生、且不得出现半截协议响应"这一要求在本机形态下依然成立，
/// 因此以**真实进程 + 真实退出码 + 空 stdout** 取证，而不是删除覆盖。
#[test]
fn real_broker_fails_closed_on_invalid_workspace_root_over_stdio() {
    let sandbox = Sandbox::new("local-mcp-e2e-failclosed-");

    // ① 根不存在。
    let missing_root = sandbox.host_path("does-not-exist");
    let mut broker = StdioBroker::start(&missing_root, &[]);
    broker.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "fail-closed-probe", "version": "1.0.0"}}
    }));
    let stdout_lines = broker.drain_stdout_lines();
    let exit = broker.shutdown();
    assert_eq!(
        exit.code,
        Some(2),
        "工作区根不存在必须以 USAGE(2) 拒绝服务：{}",
        exit.stderr
    );
    assert!(
        exit.stderr.contains("invalid configuration")
            && exit
                .stderr
                .contains("workspace root is not an existing directory"),
        "拒绝原因必须可诊断（配置校验层）：{}",
        exit.stderr
    );
    assert!(
        stdout_lines.is_empty(),
        "拒绝服务时协议通道必须是空的（不得出现半截响应）：{stdout_lines:?}"
    );

    // ② 根存在但不是目录（文件）。
    let file_root = sandbox.write("not-a-dir.txt", "x\n");
    let mut broker = StdioBroker::start(&file_root, &[]);
    broker.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "fail-closed-probe", "version": "1.0.0"}}
    }));
    let stdout_lines = broker.drain_stdout_lines();
    let exit = broker.shutdown();
    assert_eq!(
        exit.code,
        Some(2),
        "工作区根不是目录必须以 USAGE(2) 拒绝服务：{}",
        exit.stderr
    );
    assert!(
        exit.stderr
            .contains("workspace root is not an existing directory"),
        "拒绝原因必须可诊断：{}",
        exit.stderr
    );
    assert!(
        stdout_lines.is_empty(),
        "拒绝服务时协议通道必须是空的：{stdout_lines:?}"
    );

    // ③ 根是指向不存在目标的**符号链接**（canonicalize 必然失败）同样必须拒绝。
    let broken_link = sandbox.host_path("broken-root");
    std::os::unix::fs::symlink(sandbox.host_path("nowhere"), &broken_link).expect("symlink");
    let mut broker = StdioBroker::start(&broken_link, &[]);
    broker.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "fail-closed-probe", "version": "1.0.0"}}
    }));
    let stdout_lines = broker.drain_stdout_lines();
    let exit = broker.shutdown();
    assert_eq!(
        exit.code,
        Some(2),
        "悬空符号链接作为根必须以 USAGE(2) 拒绝服务：{}",
        exit.stderr
    );
    assert!(
        exit.stderr.contains("invalid configuration"),
        "拒绝原因必须可诊断：{}",
        exit.stderr
    );
    assert!(
        stdout_lines.is_empty(),
        "拒绝服务时协议通道必须是空的：{stdout_lines:?}"
    );
}

/// 终态任务的**条数**保留上限同样来自配置并在真实 server 上生效。
#[test]
fn real_broker_reaps_terminal_tasks_by_retention_over_stdio() {
    let sandbox = Sandbox::new("local-mcp-e2e-retention-");
    // TTL 拉长到 1h：本用例只观察条数上限，不观察时间上限（两者在同一回收器里）。
    let mut broker = StdioBroker::start(
        sandbox.root(),
        &["--task-ttl-secs", "3600", "--task-retention", "1"],
    );
    broker.legacy_handshake();

    let mut task_ids = Vec::new();
    for (offset, marker) in ["retain-1", "retain-2", "retain-3"].iter().enumerate() {
        let started = broker.tool_result(
            10 + offset as u64,
            "Bash",
            json!({"command": format!("echo {marker}"), "run_in_background": true}),
        );
        assert_ok(&started, "Bash");
        let task_id = structured(&started)["task_id"]
            .as_str()
            .expect("task_id")
            .to_string();
        wait_for_status(
            &mut broker,
            &task_id,
            &["completed"],
            Duration::from_secs(20),
        );
        task_ids.push(task_id);
    }

    // retention=1：只剩最后一个终态任务可读，之前的必须已被回收（不可读且不泄露存在性）。
    let newest = task_ids.last().expect("至少一个任务").clone();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut oldest_reaped = false;
    let mut id = 100u64;
    while Instant::now() < deadline {
        id += 1;
        if broker
            .read_resource(id, &format!("sandbox://tasks/{}", task_ids[0]))
            .get("error")
            .is_some()
        {
            oldest_reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(oldest_reaped, "最早的任务必须被条数上限回收");
    let newest_message = broker.read_resource(id + 1, &format!("sandbox://tasks/{newest}"));
    assert!(
        newest_message.get("result").is_some(),
        "最新终态任务必须仍然可读：{newest_message}"
    );

    let exit = broker.shutdown();
    assert_eq!(exit.code, Some(0), "stderr:\n{}", exit.stderr);
}

/// A-009：**不依赖 Docker** —— 把含 `docker` 的目录从 PATH 剔除后，七工具照常工作。
///
/// 这不是"声明式"检查：PATH 真的被换掉（子进程环境不含 docker），七工具真的跑一遍。
#[test]
fn real_broker_executes_seven_tools_without_docker_on_path() {
    let sandbox = Sandbox::new("local-mcp-e2e-nodocker-");
    sandbox.write("notes/hello.txt", "alpha\nneedle-in-file\n");

    let path = path_without_docker();
    assert!(
        !path_has_docker(&path),
        "本用例的前置条件：构造出的 PATH 上不得有 docker（{path}）"
    );

    let mut broker = StdioBroker::start_with_env(sandbox.root(), &[], &[("PATH", path.as_str())]);
    let broker_pid = broker.pid();
    broker.legacy_handshake();

    // 让 Bash 自己确认 PATH 上确实没有 docker（命令回读，而不是测试进程自述）。
    let probe = broker.tool_result(
        10,
        "Bash",
        json!({"command": "command -v docker || echo NO-DOCKER-ON-PATH",
               "timeout": 15000, "run_in_background": false}),
    );
    assert_ok(&probe, "Bash");
    assert!(
        text_of(&probe).contains("NO-DOCKER-ON-PATH"),
        "子进程 PATH 上不得有 docker：{}",
        text_of(&probe)
    );
    assert_eq!(structured(&probe)["exit_code"], 0);

    // 七工具在该环境下全部可用（Read/Write/Edit/Glob/Grep/folder/Bash）。
    let write = broker.tool_result(
        11,
        "Write",
        json!({"file_path": "nodocker/out.txt", "content": "without-docker\n"}),
    );
    assert_ok(&write, "Write");
    let read = broker.tool_result(12, "Read", json!({"file_path": "nodocker/out.txt"}));
    assert_ok(&read, "Read");
    assert!(text_of(&read).contains("without-docker"));
    let edit = broker.tool_result(
        13,
        "Edit",
        json!({"file_path": "nodocker/out.txt", "old_string": "without", "new_string": "no"}),
    );
    assert_ok(&edit, "Edit");
    assert_eq!(sandbox.read("nodocker/out.txt"), "no-docker\n");
    let glob = broker.tool_result(14, "Glob", json!({"pattern": "**/*.txt", "path": "."}));
    assert_ok(&glob, "Glob");
    assert_eq!(structured(&glob)["count"], 2);
    let grep = broker.tool_result(
        15,
        "Grep",
        json!({"pattern": "no-docker", "path": ".", "output_mode": "content"}),
    );
    assert_ok(&grep, "Grep");
    assert!(text_of(&grep).contains("nodocker/out.txt"));
    let folder = broker.tool_result(
        16,
        "folder_operations",
        json!({"operation": "exists", "folder_path": "nodocker"}),
    );
    assert_ok(&folder, "folder_operations");
    assert_eq!(structured(&folder)["exists"], true);

    // 忙完之后服务进程仍无残留子进程。
    assert!(
        wait_until_no_children(broker_pid, Duration::from_secs(10)).is_empty(),
        "无 docker 环境下同样不得留下子进程：{:?}",
        children_of(broker_pid)
    );

    let exit = broker.shutdown();
    assert_eq!(exit.code, Some(0), "stderr:\n{}", exit.stderr);

    dump_evidence(
        "e2e-stdio-no-docker.json",
        &serde_json::to_string_pretty(&json!({
            "path_without_docker": path,
            "bash_probe": text_of(&probe),
            "read_text": text_of(&read),
            "glob": structured(&glob),
            "grep_text": text_of(&grep),
            "exit_code": exit.code,
        }))
        .expect("serialize evidence"),
    );
}

/// SIGINT must cancel stdio and let the registry reap the background process group.
#[test]
fn real_broker_sigint_reaps_background_process_group() {
    let sandbox = Sandbox::new("local-mcp-e2e-stdio-sigint-");
    let mut broker = StdioBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();
    broker.legacy_handshake();
    let started = broker.tool_result(
        1,
        "Bash",
        json!({"command": "echo stdio-int; sleep 30", "run_in_background": true}),
    );
    assert_ok(&started, "Bash");
    let task = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("stdio-int")
    })
    .expect("后台任务必须在 SIGINT 前真实运行");
    broker.track_process_group(task.pgid);
    assert!(process_exists(task.pid));
    assert!(process_group_exists(task.pgid));
    let exit = broker.shutdown_with_signal(libc::SIGINT);
    assert_eq!(exit.code, Some(0), "SIGINT stderr:\n{}", exit.stderr);
    wait_until_process_and_group_gone(task.pid, task.pgid, Duration::from_secs(10));
}

#[test]
fn real_broker_sigterm_reaps_background_process_group() {
    let sandbox = Sandbox::new("local-mcp-e2e-stdio-sigterm-");
    let mut broker = StdioBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();
    broker.legacy_handshake();
    let started = broker.tool_result(
        1,
        "Bash",
        json!({"command": "echo stdio-term; sleep 30", "run_in_background": true}),
    );
    assert_ok(&started, "Bash");
    let task = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("stdio-term")
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
fn stdio_broker_drop_reaps_registered_task_group() {
    let sandbox = Sandbox::new("local-mcp-e2e-stdio-drop-");
    let mut broker = StdioBroker::start(sandbox.root(), &[]);
    let broker_pid = broker.pid();
    broker.legacy_handshake();
    let started = broker.tool_result(
        1,
        "Bash",
        json!({"command": "echo stdio-drop; sleep 30", "run_in_background": true}),
    );
    assert_ok(&started, "Bash");
    let task = wait_for_descendant(broker_pid, Duration::from_secs(10), |entry| {
        entry.command.contains("stdio-drop")
    })
    .expect("Drop cleanup task must be running");
    broker.track_process_group(task.pgid);
    drop(broker);
    wait_until_process_and_group_gone(task.pid, task.pgid, Duration::from_secs(10));
}

/// Positive control: the identity probes must report a deliberately live process/group.
#[cfg(unix)]
#[test]
fn process_identity_probe_detects_live_process() {
    let mut child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn positive control");
    let pid = child.id() as i32;
    // SAFETY: query the process group of the child just spawned by this test.
    let pgid = unsafe { libc::getpgid(pid) };
    assert!(process_exists(pid), "live positive-control pid was missed");
    assert!(
        process_group_exists(pgid),
        "live positive-control pgid was missed"
    );
    // SAFETY: terminate only the process created by this test.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    let _ = child.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        assert!(
            Instant::now() < deadline,
            "positive-control pid did not disappear"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // The child inherited this test's group, so the group itself intentionally remains.
    assert!(process_group_exists(pgid));
}
