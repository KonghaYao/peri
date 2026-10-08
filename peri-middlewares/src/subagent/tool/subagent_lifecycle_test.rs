//! M10 子 agent 生命周期 hook（红→绿）：SubagentStart/Stop 必须复用 dispatcher 的
//! 匹配语义（matcher / if 条件 / once）与执行通道（async spawn、超时、取消、进程树
//! owner），而不是旁路裸执行；生命周期事件默认非阻断。
//!
//! 红阶段：现行 `fire_subagent_lifecycle_hooks_static` 只按事件过滤，
//! matcher/if/once 都被忽略，以下用例会失败。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::hooks::types::{HookEvent, HookType, RegisteredHook};

fn subagent_hook(
    event: HookEvent,
    command: &str,
    matcher: Option<&str>,
    condition: Option<&str>,
    once: bool,
) -> RegisteredHook {
    RegisteredHook {
        hook: HookType::Command {
            command: command.to_string(),
            shell: None,
            timeout: Some(1000),
            status_message: None,
            once,
            async_run: false,
            async_rewake: false,
            matcher: matcher.map(str::to_string),
            condition: condition.map(str::to_string),
        },
        event,
        matcher: matcher.map(str::to_string),
        plugin_name: "subagent-test-plugin".to_string(),
        plugin_id: "subagent-test-plugin-id".to_string(),
        plugin_source: None,
        plugin_root: PathBuf::from("/tmp/subagent-test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/subagent-test-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn unique_marker(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peri-subagent-hook-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("fired")
}

fn lifecycle_dispatcher(
    hooks: Vec<RegisteredHook>,
    cwd: &str,
) -> crate::hooks::dispatcher::HookDispatcher {
    crate::hooks::dispatcher::HookDispatcher::new_without_llm(
        hooks,
        std::sync::Arc::new(crate::hooks::once_tracker::OnceTracker::new()),
        cwd.to_string(),
    )
}

async fn fire_with(
    dispatcher: &crate::hooks::dispatcher::HookDispatcher,
    event: HookEvent,
    cwd: &str,
    name: &str,
    result: Option<&str>,
) {
    use crate::hooks::types::HookInput;
    let input = match event {
        HookEvent::SubagentStart => HookInput::subagent_start("", "", cwd, name),
        HookEvent::SubagentStop => {
            HookInput::subagent_stop("", "", cwd, name, result.unwrap_or(""))
        }
        _ => unreachable!("lifecycle test only fires subagent events"),
    };
    // 默认非阻断：action 仅诊断，不阻断子 agent。
    let _action = dispatcher
        .fire_subagent_lifecycle(event, &input, name)
        .await;
}

async fn fire(
    hooks: &[RegisteredHook],
    event: HookEvent,
    cwd: &str,
    name: &str,
    result: Option<&str>,
) {
    let dispatcher = lifecycle_dispatcher(hooks.to_vec(), cwd);
    fire_with(&dispatcher, event, cwd, name, result).await;
}

/// matcher 不匹配的 SubagentStart hook 不得执行（今天会被忽略并执行 → 红）。
#[tokio::test]
async fn subagent_start_hook_matcher_mismatch_is_skipped() {
    let marker = unique_marker("matcher");
    let command = format!("touch {}", marker.display());
    let hooks = vec![subagent_hook(
        HookEvent::SubagentStart,
        &command,
        Some("other-agent"),
        None,
        false,
    )];

    fire(&hooks, HookEvent::SubagentStart, "/tmp", "explore", None).await;

    assert!(!marker.exists(), "matcher 不匹配的生命周期 hook 不得执行");
}

/// matcher 命中的 SubagentStart hook 必须执行（对照，避免"全跳过"假绿）。
#[tokio::test]
async fn subagent_start_hook_matcher_match_executes() {
    let marker = unique_marker("match-ok");
    let command = format!("touch {}", marker.display());
    let hooks = vec![subagent_hook(
        HookEvent::SubagentStart,
        &command,
        Some("explore"),
        None,
        false,
    )];

    fire(&hooks, HookEvent::SubagentStart, "/tmp", "explore", None).await;

    assert!(marker.exists(), "matcher 命中的生命周期 hook 必须执行");
}

/// if 条件不满足（非工具事件没有 tool_name 可匹配）时不得执行。
#[tokio::test]
async fn subagent_start_hook_condition_mismatch_is_skipped() {
    let marker = unique_marker("condition");
    let command = format!("touch {}", marker.display());
    let hooks = vec![subagent_hook(
        HookEvent::SubagentStart,
        &command,
        None,
        Some("Bash(git *)"),
        false,
    )];

    fire(&hooks, HookEvent::SubagentStart, "/tmp", "explore", None).await;

    assert!(!marker.exists(), "if 条件不满足的生命周期 hook 不得执行");
}

/// once hook 两次生命周期触发只能执行一次（共享 once tracker）。
#[tokio::test]
async fn subagent_start_hook_once_fires_only_once() {
    let marker = unique_marker("once");
    let command = format!(
        "touch {}; echo run >> {}.log",
        marker.display(),
        marker.display()
    );
    let hooks = vec![subagent_hook(
        HookEvent::SubagentStart,
        &command,
        None,
        None,
        true,
    )];

    let dispatcher = lifecycle_dispatcher(hooks, "/tmp");
    fire_with(
        &dispatcher,
        HookEvent::SubagentStart,
        "/tmp",
        "explore",
        None,
    )
    .await;
    fire_with(
        &dispatcher,
        HookEvent::SubagentStart,
        "/tmp",
        "explore",
        None,
    )
    .await;

    let log = std::fs::read_to_string(format!("{}.log", marker.display())).unwrap_or_default();
    assert_eq!(log.lines().count(), 1, "once hook 只能在首次触发执行一次");
}

/// 跨 spawn 的 once 语义：同一工具（同一会话）多次 `lifecycle_closures()` 必须
/// 共享同一个 dispatcher/OnceTracker；每次 spawn 新建会让 once:true 随每个子触发。
#[tokio::test]
async fn subagent_start_once_hook_is_shared_across_spawns() {
    let marker = unique_marker("once-across-spawns");
    let command = format!(
        "touch {}; echo run >> {}.log",
        marker.display(),
        marker.display()
    );
    let hooks = vec![subagent_hook(
        HookEvent::SubagentStart,
        &command,
        None,
        None,
        true,
    )];

    let tool = super::SubAgentTool::new(
        std::sync::Arc::new(Vec::new()),
        None,
        std::sync::Arc::new(|_: Option<&str>| unimplemented!("no child is spawned in this test")),
        "/tmp".to_string(),
    )
    .with_registered_hooks(hooks);

    let log_path = format!("{}.log", marker.display());
    let fired_lines = |path: &str| {
        std::fs::read_to_string(path)
            .map(|log| log.lines().count())
            .unwrap_or(0)
    };

    // 第一次 spawn：等 hook 命令真正落盘（模拟一次真实的子 agent 启动完成）。
    let (on_start, _) = tool.lifecycle_closures();
    on_start.expect("非空 hook 列表必须构造 SubagentStart 闭包")("explore", "/tmp");
    for _ in 0..200 {
        if fired_lines(&log_path) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(fired_lines(&log_path), 1, "首次 spawn 必须触发一次");

    // 第二次 spawn（同一会话、同一工具）：共享 once tracker 必须让 hook 不再触发。
    let (on_start, _) = tool.lifecycle_closures();
    on_start.expect("非空 hook 列表必须构造 SubagentStart 闭包")("explore", "/tmp");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        fired_lines(&log_path),
        1,
        "once:true 的 SubagentStart 必须跨 spawn 只触发一次"
    );
}

/// 并发触发（两个子 agent 背靠背启动、互不等待）：once hook 必须在"预留"阶段
/// 原子去重，只能执行一次。
///
/// [TRAP] 该用例不得用"等第一个执行完再触发第二个"的方式串行化：那会把
/// `was_fired`（查）与执行后 `mark_fired`（标记）之间的竞态掩盖成必过。
/// 两个闭包各自经 `tokio::spawn` 分离触发，第二次触发在第一次的命令尚未返回
/// 时进入 dispatcher——这正是生命周期路径的真实并发窗口。
#[tokio::test]
async fn subagent_start_once_hook_is_atomic_under_concurrent_triggers() {
    let marker = unique_marker("once-concurrent");
    let command = format!(
        "touch {}; echo run >> {}.log",
        marker.display(),
        marker.display()
    );
    let hooks = vec![subagent_hook(
        HookEvent::SubagentStart,
        &command,
        None,
        None,
        true,
    )];

    let tool = super::SubAgentTool::new(
        std::sync::Arc::new(Vec::new()),
        None,
        std::sync::Arc::new(|_: Option<&str>| unimplemented!("no child is spawned in this test")),
        "/tmp".to_string(),
    )
    .with_registered_hooks(hooks);

    let (on_start_a, _) = tool.lifecycle_closures();
    let (on_start_b, _) = tool.lifecycle_closures();
    on_start_a.expect("非空 hook 列表必须构造 SubagentStart 闭包")("explore", "/tmp");
    on_start_b.expect("非空 hook 列表必须构造 SubagentStart 闭包")("explore", "/tmp");

    let log_path = format!("{}.log", marker.display());
    let fired_lines = || {
        std::fs::read_to_string(&log_path)
            .map(|log| log.lines().count())
            .unwrap_or(0)
    };

    // 有界观察窗口：一出现第二次执行就立即结束等待（不掩盖竞态）；否则等
    // 单次执行的状态稳定 300ms（命令已落盘且无后续写入）后收口。
    let mut stable_since: Option<std::time::Instant> = None;
    for _ in 0..300 {
        if fired_lines() >= 2 {
            break;
        }
        if fired_lines() == 1 {
            match stable_since {
                Some(since) if since.elapsed() >= std::time::Duration::from_millis(300) => break,
                Some(_) => {}
                None => stable_since = Some(std::time::Instant::now()),
            }
        } else {
            stable_since = None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(
        fired_lines(),
        1,
        "并发触发的 once:true SubagentStart 只能执行一次（原子预留）"
    );
}
