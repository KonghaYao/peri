//! 生产装配面的 once 作用域判定（缺陷复审次要项，运行时证据）。
//!
//! 复审疑问：`SubAgentTool` 的 once cell 挂在"每轮重建"的工具实例上，同一
//! session 跨 prompt 再 spawn 时 once 是否被重置、是否违反契约。
//!
//! 本文件用**生产装配器**（`ProductionChainAssembler` + `production_blueprint`，
//! 即 `build_agent` 每轮 prompt 调用的唯一入口）给出运行时结论：
//! - 每次 `assemble` 都新建 `MiddlewareChain` / `SubAgentMiddleware` /
//!   `SubAgentTool`，once 状态随装配实例重置（本文件断言两次装配各触发一次）；
//! - 主 hook 路径的 `HookMiddleware` 同样"每轮新建实例"，作用域完全一致
//!   （对照断言，证明重置不是子 agent 生命周期路径特有）。
//!
//! 因此把 cell 上移到 `SubAgentMiddleware` 不能跨轮存活——它同样每轮重建；
//! 要跨 prompt 存活必须是 session 级 once 状态（跨 ACP 装配面），且需同时
//! 覆盖主 hook 路径，属于本轮范围外的语义扩容。

use super::*;

use crate::hooks::{HookInput, HookMiddleware};
use crate::subagent::SubAgentMiddleware;
use peri_agent::agent::react::ReactLLM;

/// 构造 once 命令行 hook（`echo run >> <log>`）。
fn once_command_hook(event: HookEvent, log: &std::path::Path) -> RegisteredHook {
    let log_path = log.to_string_lossy();
    let command = if cfg!(windows) {
        format!(
            "Add-Content -LiteralPath '{}' -Value run -Encoding UTF8",
            log_path.replace('\'', "''")
        )
    } else {
        format!("echo run >> '{}'", log_path.replace('\'', "'\\''"))
    };
    RegisteredHook {
        hook: HookType::Command {
            command,
            shell: None,
            timeout: Some(1000),
            status_message: None,
            once: true,
            async_run: false,
            async_rewake: false,
            matcher: None,
            condition: None,
        },
        event,
        matcher: None,
        plugin_name: "once-scope-plugin".to_string(),
        plugin_id: "once-scope-plugin-id".to_string(),
        plugin_source: None,
        plugin_root: PathBuf::from("/tmp/once-scope-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/once-scope-plugin-data"),
        plugin_options: std::collections::HashMap::new(),
    }
}

fn fired_lines(path: &std::path::Path) -> usize {
    match std::fs::read_to_string(path) {
        Ok(log) => log.lines().count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("failed to read hook log {}: {error}", path.display()),
    }
}

/// 生产装配器每轮（prompt）重建链/中间件/工具，因此 once 作用域 = 单次装配；
/// 主 hook 路径（每轮新建 HookMiddleware）与之一致。
#[tokio::test]
async fn once_scope_is_per_assembled_chain_not_session() {
    let dir = tempfile::Builder::new()
        .prefix("peri once scope ")
        .tempdir()
        .unwrap();
    let subagent_log = dir.path().join("subagent-once.log");
    let pretooluse_log = dir.path().join("pretooluse-once.log");

    let subagent_hook = once_command_hook(HookEvent::SubagentStart, &subagent_log);
    let pretooluse_hook = once_command_hook(HookEvent::PreToolUse, &pretooluse_log);

    let mut ctx = base_context();
    // hook 命令以 ctx.cwd 为工作目录执行：目录必须真实存在，否则 shell 起不来。
    ctx.cwd = dir.path().to_str().unwrap().to_owned();
    ctx.hook_groups = vec![vec![subagent_hook.clone(), pretooluse_hook.clone()]];

    // 两次"prompt"：生产路径每轮 prompt 调 `build_agent` → `assemble` 一次。
    for prompt in 0..2 {
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        assert!(
            out.chain.names().contains(&"SubAgentMiddleware"),
            "第 {prompt} 轮装配链上必须有 SubAgentMiddleware"
        );

        // 子 agent 生命周期路径：链上的 SubAgentMiddleware → build_tool（= collect_tools）。
        let subagent_mw = Arc::clone(
            out.subagent_mw
                .as_ref()
                .expect("非空 hook_groups 必须装配 SubAgentMiddleware"),
        )
        .downcast_arc::<SubAgentMiddleware>()
        .ok()
        .expect("链上的具体 SubAgentMiddleware");
        let tool = subagent_mw.build_tool(&ctx.cwd);
        let (on_start, _) = tool.lifecycle_closures();
        on_start.expect("非空 hook 列表必须构造 SubagentStart 闭包")(
            "fixture-child-thread",
            "explore",
            &ctx.cwd,
        );

        // 主 hook 路径：装配点每轮同样新建 HookMiddleware 实例（assembly::hooks）。
        let hook_llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
            Arc::new({
                let factory = ctx.llm_factory.clone();
                move || factory(None).into_plain_bridge()
            });
        let hook_mw = HookMiddleware::new(
            vec![pretooluse_hook.clone()],
            hook_llm_factory,
            &ctx.cwd,
            "once-scope-session",
            "/test/transcript.json",
            ctx.permission_mode.clone(),
            &ctx.provider_name,
        );
        let action = hook_mw
            .fire_event(
                HookEvent::PreToolUse,
                &HookInput::tool_call(
                    "once-scope-session",
                    "/test/transcript.json",
                    &ctx.cwd,
                    "Default",
                    "Bash",
                    &serde_json::json!({"command": "ls"}),
                    "c1",
                ),
                Some("Bash"),
                Some(&serde_json::json!({"command": "ls"})),
            )
            .await;
        assert!(
            matches!(action, crate::hooks::HookAction::Allow),
            "once PreToolUse hook 不得阻断本次调用: {action:?}"
        );
    }

    // 命令 hook 经子进程落盘：有界等待两条日志出现。
    for _ in 0..200 {
        if fired_lines(&subagent_log) >= 2 && fired_lines(&pretooluse_log) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(
        fired_lines(&subagent_log),
        2,
        "每次装配各触发一次：once 作用域 = 单次装配（生产=每轮 prompt），非 session"
    );
    assert_eq!(
        fired_lines(&pretooluse_log),
        2,
        "主 hook 路径作用域相同（HookMiddleware 每轮新建实例）"
    );
}

/// 同一次装配内（同一 prompt）once 只触发一次——与上一条对照，锁定作用域边界。
#[tokio::test]
async fn once_scope_is_stable_within_one_assembled_chain() {
    let dir = tempfile::Builder::new()
        .prefix("peri once scope single ")
        .tempdir()
        .unwrap();
    let subagent_log = dir.path().join("subagent-once.log");

    let mut ctx = base_context();
    // hook 命令以 ctx.cwd 为工作目录执行：目录必须真实存在，否则 shell 起不来。
    ctx.cwd = dir.path().to_str().unwrap().to_owned();
    ctx.hook_groups = vec![vec![once_command_hook(
        HookEvent::SubagentStart,
        &subagent_log,
    )]];

    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let subagent_mw = Arc::clone(out.subagent_mw.as_ref().expect("SubAgentMiddleware"))
        .downcast_arc::<SubAgentMiddleware>()
        .ok()
        .expect("链上的具体 SubAgentMiddleware");
    let tool = subagent_mw.build_tool(&ctx.cwd);

    // 同一次装配内两次 spawn（各取一组闭包）只允许触发一次。
    for _ in 0..2 {
        let (on_start, _) = tool.lifecycle_closures();
        on_start.expect("非空 hook 列表必须构造闭包")(
            "fixture-child-thread",
            "explore",
            &ctx.cwd,
        );
    }

    for _ in 0..200 {
        if fired_lines(&subagent_log) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        fired_lines(&subagent_log),
        1,
        "同一次装配内 once 必须只触发一次（原子预留）"
    );
}
