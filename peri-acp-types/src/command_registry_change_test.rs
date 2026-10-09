use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::CommandRegistry;
use crate::command::command_handler::{CommandHandler, CommandOutcome};
use crate::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
};
use crate::command::{CommandContext, CommandResult, PromptStopReason};

/// 假 handler：仅占位（测试断言只关心路由层，不触发执行）。
struct FakeHandler;

#[async_trait]
impl CommandHandler for FakeHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(CommandResult {
            messages: Vec::new(),
            stop_reason: PromptStopReason::EndTurn,
            feedback: None,
        })
    }
}

fn fake_entry(fullname: &str, source: CommandSource, aliases: &[&str]) -> RouteEntry {
    RouteEntry {
        fullname: fullname.to_string(),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        description: "test command".into(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source,
            lifecycle: CommandLifecycle::Connected,
        },
    }
}

fn core_entry(name: &str, aliases: &[&str]) -> RouteEntry {
    fake_entry(&format!("core:{name}"), CommandSource::Core, aliases)
}

fn install_counter(reg: &CommandRegistry) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    reg.set_on_change(Some(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    })));
    count
}
// ─── unregister（命中 + on_change / 未命中不触发） ───────────────────

#[test]
fn unregister_hit_removes_all_indexes() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();

    assert!(reg.unregister("core:compact"));
    // 全名 / 裸名 / alias 三路索引一并清除。
    assert!(reg.resolve("/core:compact").is_none());
    assert!(reg.resolve("/compact").is_none());
    assert!(reg.resolve("/c").is_none());
    assert!(reg.snapshot().is_empty());
}

#[test]
fn unregister_releases_bare_name_and_alias_for_replacement() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();
    assert!(reg.unregister("core:compact"));
    let replacement = fake_entry("ui:compact", CommandSource::Ui, &["c"]);
    reg.register(replacement).unwrap();

    for name in ["/compact", "/c", "/ui:compact"] {
        assert_eq!(reg.resolve(name).unwrap().entry.fullname, "ui:compact");
    }
    assert!(reg.resolve("/core:compact").is_none());
    assert_eq!(reg.snapshot().len(), 1);
}

#[test]
fn unregister_case_insensitive_key() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    assert!(reg.unregister("Core:Compact"), "小写化精确键删除");
    assert!(reg.snapshot().is_empty());
}

#[test]
fn unregister_miss_false() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    assert!(!reg.unregister("core:nonexistent"));
    assert!(!reg.unregister("mcp:demo:x"));
    // 未命中不影响现有条目。
    assert!(reg.resolve("/compact").is_some());
}

// ─── unregister_namespace（前缀批量注销，旁系保留） ──────────────────

#[test]
fn unregister_namespace_batch_keeps_others() {
    let reg = CommandRegistry::new();
    // 带 alias 的第一条（P2-3 审查跟进：批量注销路径须同步清 alias 索引）。
    reg.register(fake_entry(
        "demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &["da"],
    ))
    .unwrap();
    reg.register(fake_entry(
        "demo:b",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "other:c",
        CommandSource::Mcp {
            server: "other".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "demo2:d",
        CommandSource::Mcp {
            server: "demo2".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(core_entry("compact", &[])).unwrap();

    // 决策 1：Mcp 无独立 namespace——前缀 = server 名（domain 段即 server，
    // namespace 参数传空，`{domain}:` 结算前缀命中 `demo:*`）。
    let n = reg.unregister_namespace("demo", "");
    assert_eq!(n, 2, "仅 demo: 前缀两条");

    // 前缀边界：demo2 前缀相似但不同，保留。
    assert!(reg.resolve("/demo:a").is_none());
    assert!(reg.resolve("/demo:b").is_none());
    assert!(
        reg.resolve("/da").is_none(),
        "alias 随 namespace 批量注销同步清理"
    );
    assert!(reg.resolve("/other:c").is_some());
    assert!(reg.resolve("/demo2:d").is_some());
    assert!(reg.resolve("/compact").is_some());
    assert_eq!(reg.snapshot().len(), 3);
}

#[test]
fn unregister_namespace_miss_zero() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry(
        "demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();

    assert_eq!(reg.unregister_namespace("demo", ""), 1);
    reg.register(fake_entry(
        "demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    assert_eq!(reg.unregister_namespace("nope", ""), 0);
    assert_eq!(reg.unregister_namespace("plugin", "ecc"), 0);
    assert_eq!(reg.snapshot().len(), 1);
}

// ─── on_change 触发矩阵 ─────────────────────────────────────────────

#[test]
fn on_change_fires_on_register_ok() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);

    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(fake_entry(
        "demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn on_change_not_fired_on_register_error() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);

    reg.register(core_entry("compact", &[])).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // 三种 Err 均不触发。
    assert!(reg.register(core_entry("compact", &[])).is_err()); // Conflict
    assert!(reg
        .register(fake_entry("a:b:c:d", CommandSource::Core, &[]))
        .is_err()); // MalformedName
    assert!(reg
        .register(fake_entry(
            "mcp:demo:x",
            CommandSource::Plugin { name: "p".into() },
            &[]
        ))
        .is_err()); // ProvenanceMismatch
    assert_eq!(count.load(Ordering::SeqCst), 1, "Err 不触发 on_change");
}

#[test]
fn on_change_unregister_trigger_matrix() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(fake_entry(
        "demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();

    assert!(reg.unregister("core:compact"));
    assert_eq!(count.load(Ordering::SeqCst), 3, "unregister 命中触发");

    assert!(!reg.unregister("core:compact"), "二次删除未命中");
    assert_eq!(count.load(Ordering::SeqCst), 3, "unregister 未命中不触发");

    assert_eq!(reg.unregister_namespace("demo", ""), 1);
    assert_eq!(count.load(Ordering::SeqCst), 4, "namespace 移除 n>0 触发");

    assert_eq!(reg.unregister_namespace("demo", ""), 0);
    assert_eq!(count.load(Ordering::SeqCst), 4, "namespace 未命中不触发");
}

#[test]
fn on_change_cleared_callback_not_fired() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    reg.set_on_change(None);

    reg.register(core_entry("compact", &[])).unwrap();
    reg.unregister("core:compact");
    assert_eq!(count.load(Ordering::SeqCst), 0, "回调清空后不触发");
}

#[test]
fn on_change_callback_can_snapshot() {
    // 投影闭环语义：回调内 snapshot 可重建投影（真实投影函数在 peri-acp 组合根，
    // 此处以等价闭包断言「snapshot 数据源可重建投影列表」）。
    let reg = Arc::new(CommandRegistry::new());
    let reg_ref = reg.clone();
    let seen_lens = Arc::new(std::sync::Mutex::new(Vec::new()));
    let lens_ref = seen_lens.clone();
    reg.set_on_change(Some(Arc::new(move || {
        let snap = reg_ref.snapshot();
        // 等价投影：fullname + description（wire 形态与 peri-acp
        // available_command_from_entry 的 name/description 一致）。
        let projection: Vec<(String, String)> = snap
            .iter()
            .map(|e| (e.fullname.clone(), e.description.clone()))
            .collect();
        assert!(!projection.is_empty());
        assert_eq!(
            projection[0].0, "core:compact",
            "按 fullname 排序，首条为 core:compact"
        );
        lens_ref.lock().unwrap().push(snap.len());
    })));

    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(fake_entry("ui:history", CommandSource::Ui, &[]))
        .unwrap();
    // 每次注册触发一次回调，且回调内 snapshot 已包含最新条目（内容变化先落盘、后通知）。
    assert_eq!(*seen_lens.lock().unwrap(), vec![1, 2]);
}

/// reconcile：批量注销 + 注册在单次写锁内完成，on_change 合并为**单次**触发
/// （P1-1 联动：sync_mcp_entries 对账不再逐条触发 N 次投影重发）；内容无
/// 变化（注销 0 且注册 0）不触发。
#[test]
fn reconcile_fires_single_on_change() {
    let reg = Arc::new(CommandRegistry::new());
    let mcp_a = || {
        fake_entry(
            "demo:a",
            CommandSource::Mcp {
                server: "demo".into(),
            },
            &[],
        )
    };
    let mcp_b = || {
        fake_entry(
            "demo:b",
            CommandSource::Mcp {
                server: "demo".into(),
            },
            &[],
        )
    };

    // 未挂载回调：注册成功、不触发
    let (removed, added) = reg.reconcile(&[], vec![mcp_a(), mcp_b()]);
    assert_eq!((removed, added), (0, 2));

    // 挂载回调：注销 + 注册（对账形态）→ 单次触发，且回调见终态快照
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_ref = seen.clone();
    let reg_ref = Arc::clone(&reg);
    reg.set_on_change(Some(Arc::new(move || {
        let n = reg_ref.snapshot().len();
        seen_ref.lock().unwrap().push(n);
    })));
    let (removed, added) = reg.reconcile(&["demo:a".to_string()], vec![mcp_b()]);
    assert_eq!(
        (removed, added),
        (1, 0),
        "b 与已注册条目同键冲突被纯拒绝（不覆盖）"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![1],
        "批量对账应合并为单次 on_change，且回调见终态（仅剩 1 条）"
    );

    // 内容无变化（注销 0 且注册 0）→ 不触发
    let (removed, added) = reg.reconcile(&["nope".to_string()], vec![mcp_b()]);
    assert_eq!((removed, added), (0, 0));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![1],
        "内容无变化的 reconcile 不得触发 on_change"
    );
}

// ─── on_change → 投影数据源闭环（Step 6） ────────────────────────────

/// 测试内自建等价投影闭包（契约层不依赖 peri-acp 的
/// `available_command_from_entry`——pub(crate) 且依赖
/// agent-client-protocol-schema）；断言「snapshot 数据源可重建投影列表」
/// 这一闭环语义，wire 形态与生产投影一致（name = fullname / description）。
fn entry_to_name_desc(entry: &RouteEntry) -> (String, String) {
    (entry.fullname.clone(), entry.description.clone())
}

#[test]
fn on_change_projection_loop_register_after_builtins() {
    // register_builtins 等价物：手工注册 core: 条目（装配顺序即优先级）。
    let reg = Arc::new(CommandRegistry::new());
    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(core_entry("loop", &[])).unwrap();
    reg.register(core_entry("rewind", &[])).unwrap();

    // set_on_change 之后才计数：builtins 注册（回调未装）不触发。
    let calls = Arc::new(AtomicUsize::new(0));
    let projected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reg_ref = reg.clone();
    let c_calls = calls.clone();
    let c_projected = projected.clone();
    reg.set_on_change(Some(Arc::new(move || {
        c_calls.fetch_add(1, Ordering::SeqCst);
        // 回调内 snapshot() + 等价投影闭包重建投影列表
        // （内容变化先落盘、后通知，投影必然已含新条目）。
        let proj: Vec<(String, String)> = reg_ref
            .snapshot()
            .iter()
            .map(|e| entry_to_name_desc(e))
            .collect();
        *c_projected.lock().unwrap() = proj;
    })));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "set_on_change 前注册不触发"
    );

    // register 新条目 → 回调触发，回调内 snapshot 重建投影（条目数 +1）。
    reg.register(core_entry("status", &[])).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "新注册触发一次 on_change");

    let proj = projected.lock().unwrap();
    assert_eq!(proj.len(), 4, "投影条目数 = 3 builtins + 1 新注册");
    // 按 fullname 排序（snapshot 确定性），新条目在列。
    assert_eq!(
        proj.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["core:compact", "core:loop", "core:rewind", "core:status"]
    );
    // name/description 与注册内容一致（投影闭包闭环）。
    assert!(proj.contains(&("core:status".to_string(), "test command".to_string())));
}
