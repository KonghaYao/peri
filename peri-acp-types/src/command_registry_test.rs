//! `CommandRegistry` 注册表测试（Phase 2 plan Step 3 用例清单全量覆盖）。
//!
//! 覆盖：严格精确（裸名 / 全名 / alias）、前缀拒绝、歧义前缀、大小写不敏感、
//! 冲突裁决（全名 / alias / 裸名，snapshot 不变）、词法校验、域校验、unregister、
//! unregister_namespace、on_change 触发矩阵、resolve 失败路径、snapshot 内容。
//!
//! Phase 6 A2 追加：register_all 部分成功矩阵、project_sources 断连注销 +
//! removed_any 门控、mark_source_started 覆盖矩阵（含 Discovered→Started
//! 撤旧）、mark_source_completed ptr_eq 拒绝旧 handle 回写（无 ABA）+
//! on_change 恰一次、clear_source_started cancel 回退可重试。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::{CommandRegistry, HandleToken, RegisterError};
use crate::command::command_handler::{CommandHandler, CommandOutcome};
use crate::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
};
use crate::command::{CommandContext, CommandResult, PromptStopReason};

// ─── 测试辅助 ──────────────────────────────────────────────────────

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

/// core 域条目快捷构造（第一等级）。
fn core_entry(name: &str, aliases: &[&str]) -> RouteEntry {
    fake_entry(&format!("core:{name}"), CommandSource::Core, aliases)
}

/// 注册一个计数器回调并返回计数句柄。
fn install_counter(reg: &CommandRegistry) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    reg.set_on_change(Some(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    })));
    count
}

// ─── 严格精确匹配（裸名 / 全名 / alias） ─────────────────────────────

#[test]
fn resolve_bare_name_exact() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();
    reg.register(core_entry("rewind", &[])).unwrap();

    let resolved = reg.resolve("/compact").expect("裸名精确命中");
    assert_eq!(resolved.entry.fullname, "core:compact");
    assert_eq!(resolved.args, "");
}

#[test]
fn resolve_fullname_exact() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    let resolved = reg.resolve("/core:compact").expect("全名精确命中");
    assert_eq!(resolved.entry.fullname, "core:compact");
}

#[test]
fn resolve_alias_exact() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c", "compress"]))
        .unwrap();

    let resolved = reg.resolve("/c").expect("alias 精确命中");
    assert_eq!(resolved.entry.fullname, "core:compact");
    assert_eq!(
        reg.resolve("/compress").unwrap().entry.fullname,
        "core:compact"
    );
}

#[test]
fn resolve_without_slash_prefix() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    // 与现状 find 先例一致：无 `/` 前缀也执行同一词法切分。
    assert!(reg.resolve("compact").is_some());
}

#[test]
fn resolve_args_lexing() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    let resolved = reg.resolve("/compact foo bar").expect("带 args 命中");
    assert_eq!(resolved.args, "foo bar");
    // 无 args / 多空格（对齐现状 mod.rs 先例：args 首尾 trim）。
    assert_eq!(reg.resolve("/compact").unwrap().args, "");
    assert_eq!(reg.resolve("/compact   foo ").unwrap().args, "foo");
    // alias 路径同样切分 args。
    assert_eq!(reg.resolve("/core:compact x").unwrap().args, "x");
}

// ─── 前缀匹配废弃（/rew 不解析为 /rewind；歧义前缀一律 None） ────────

#[test]
fn resolve_prefix_not_expanded() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("rewind", &[])).unwrap();

    // 设计 §55：/rew 不解析为 /rewind——唯一前缀也不补全。
    assert!(reg.resolve("/rew").is_none());
    assert!(reg.resolve("/rewi").is_none());
    // 完整名称仍命中。
    assert!(reg.resolve("/rewind").is_some());
}

#[test]
fn resolve_ambiguous_prefix_none() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(core_entry("compose", &[])).unwrap();

    // 多个同前缀名称：任何前缀输入均不解析（模糊只留 UI 搜索层）。
    assert!(reg.resolve("/com").is_none());
    assert!(reg.resolve("/comp").is_none());
}

// ─── 大小写不敏感 ──────────────────────────────────────────────────

#[test]
fn resolve_case_insensitive() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();

    assert_eq!(
        reg.resolve("/COMPACT").unwrap().entry.fullname,
        "core:compact",
        "裸名大写命中"
    );
    assert_eq!(
        reg.resolve("/Core:Compact").unwrap().entry.fullname,
        "core:compact",
        "全名大小写混合命中"
    );
    assert_eq!(
        reg.resolve("/C").unwrap().entry.fullname,
        "core:compact",
        "alias 大写命中"
    );
    // 注册路径同样大小写归一：大写 fullname 与已有键冲突。
    let err = reg.register(fake_entry("CORE:COMPACT", CommandSource::Core, &[]));
    assert_eq!(
        err,
        Err(RegisterError::Conflict {
            key: "core:compact".into()
        })
    );
}

// ─── 冲突裁决（纯拒绝，snapshot 不变） ──────────────────────────────

#[test]
fn register_conflict_fullname_rejects_and_snapshot_unchanged() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    // 同键二次注册：后注册者拒绝 + warn（纯拒绝，无替换分支）。
    let err = reg.register(core_entry("compact", &["c2"]));
    assert_eq!(
        err,
        Err(RegisterError::Conflict {
            key: "core:compact".into()
        })
    );

    // snapshot 内容不变：仍只有第一条目，且 alias 未混入（c2 未被登记）。
    let snap = reg.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].fullname, "core:compact");
    assert!(reg.resolve("/c2").is_none());
    assert!(reg.resolve("/compact").is_some());
}

#[test]
fn register_conflict_alias() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();

    // 新条目 alias 与既有 alias 冲突。
    let err = reg.register(core_entry("rewind", &["c"]));
    assert_eq!(err, Err(RegisterError::Conflict { key: "c".into() }));

    // 拒绝后未写入：rewind 不可解析。
    assert!(reg.resolve("/rewind").is_none());
    assert_eq!(reg.snapshot().len(), 1);
}

#[test]
fn register_conflict_bare_name() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();

    // ui:compact 的裸名 "compact" 已被 core:compact 登记（第一等级裸名冲突）。
    let err = reg.register(fake_entry("ui:compact", CommandSource::Ui, &[]));
    assert_eq!(
        err,
        Err(RegisterError::Conflict {
            key: "compact".into()
        })
    );

    // 旁系裸名（不同 name 段）不受影响。
    reg.register(fake_entry("ui:history", CommandSource::Ui, &[]))
        .unwrap();
    assert!(reg.resolve("/history").is_some());
    assert_eq!(reg.snapshot().len(), 2);
}

#[test]
fn register_conflict_between_alias_and_bare_name() {
    let reg = CommandRegistry::new();
    // 条 1 alias 占 "cc"；条 2 裸名与 alias 交叉冲突（双向防护）。
    reg.register(core_entry("compact", &["cc"])).unwrap();
    let err = reg.register(fake_entry("ui:cc", CommandSource::Ui, &[]));
    assert_eq!(err, Err(RegisterError::Conflict { key: "cc".into() }));
}

/// Phase 6 B2 越权矩阵：同插件两命令同名（同键二次注册）→ Conflict 拒绝，
/// 先出现者保留（插件静态装配 register_all 逐条纯拒绝，不覆盖、不静默）。
#[test]
fn register_conflict_plugin_same_key_second_registration() {
    let reg = CommandRegistry::new();
    let src = CommandSource::Plugin { name: "ecc".into() };
    reg.register(fake_entry("plugin:ecc:deploy", src.clone(), &[]))
        .unwrap();

    // 同插件同名二次注册（如两 skill 同 frontmatter name）：后注册者拒绝。
    let err = reg.register(fake_entry("plugin:ecc:deploy", src, &[]));
    assert_eq!(
        err,
        Err(RegisterError::Conflict {
            key: "plugin:ecc:deploy".into()
        })
    );
    // 注册表保持首条目，无覆盖。
    let snap = reg.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].fullname, "plugin:ecc:deploy");
}

/// Phase 6 B2 越权矩阵补充：`plugin:{plugin}:{cmd}` 合法通过且可解析
/// （第二等级完整 2 层形态，namespace 首段由 provenance 声明）。
#[test]
fn register_plugin_domain_valid_and_resolvable() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry(
        "plugin:ecc:deploy",
        CommandSource::Plugin { name: "ecc".into() },
        &[],
    ))
    .unwrap();
    let resolved = reg.resolve("/plugin:ecc:deploy").expect("全名命中");
    assert_eq!(resolved.entry.fullname, "plugin:ecc:deploy");
    // 第二等级不登记裸名（deploy 不可解析；`mcp:hello` 形态非法同源）。
    assert!(reg.resolve("/deploy").is_none());
    assert_eq!(reg.snapshot().len(), 1);
}

// ─── 词法校验（register 严格路径） ──────────────────────────────────

#[test]
fn register_malformed_name_cases() {
    let cases: &[(&str, CommandSource)] = &[
        ("mcp__demo__hello", CommandSource::Core), // mcp__ 遗留形态
        ("a:b:c:d", CommandSource::Core),          // 冒号段数超限
        ("core:foo:bar", CommandSource::Core),     // 第一等级双层
        (
            "mcp:hello",
            CommandSource::Mcp {
                server: "demo".into(),
            },
        ), // 第二等级单层
        ("core::x", CommandSource::Core),          // 空段
        (":leading", CommandSource::Core),         // 空段
        ("core:co mpact", CommandSource::Core),    // 段含空白
    ];

    for (fullname, source) in cases {
        let reg = CommandRegistry::new();
        let err = reg.register(fake_entry(fullname, source.clone(), &[]));
        assert_eq!(
            err,
            Err(RegisterError::MalformedName),
            "fullname = {fullname}"
        );
        // 拒绝后注册表为空（Err 不改变内容）。
        assert!(reg.snapshot().is_empty(), "fullname = {fullname}");
    }
}

#[test]
fn register_malformed_alias_cases() {
    // alias 词法校验（P1-1 审查跟进）：alias 必须为 Bare 形态——含 `__` /
    // 冒号 / 空白 / 空串一律 MalformedName，register 严格校验不可经 alias
    // 旁路（设计 §59/§78：「解析即失败」裁决对 alias_index 同样生效）。
    let cases: &[&str] = &[
        "mcp__x",   // 双下划线遗留形态
        "a:b",      // 冒号（非 Bare 形态）
        "core:x",   // 显式第一等级（非 Bare 形态）
        "my alias", // 含空白（split_once(' ') 后永不可解析的静默失效条目）
        "",         // 空 alias
    ];

    for alias in cases {
        let reg = CommandRegistry::new();
        let err = reg.register(core_entry("compact", &[alias]));
        assert_eq!(err, Err(RegisterError::MalformedName), "alias = {alias:?}");
        // 拒绝后注册表为空，alias 不可解析（未被登记）。
        assert!(reg.snapshot().is_empty(), "alias = {alias:?}");
        assert!(reg.resolve(alias).is_none(), "alias = {alias:?}");
    }
}

// ─── 域校验（namespace 首段不可伪造） ───────────────────────────────

#[test]
fn register_provenance_mismatch_cases() {
    let cases: &[(&str, CommandSource)] = &[
        // plugin 条目注册 mcp:* → ProvenanceMismatch。
        (
            "mcp:demo:hello",
            CommandSource::Plugin { name: "ecc".into() },
        ),
        // core 条目注册 mcp:* → ProvenanceMismatch。
        ("mcp:demo:hello", CommandSource::Core),
        // mcp 条目注册 core:* → ProvenanceMismatch。
        (
            "core:compact",
            CommandSource::Mcp {
                server: "demo".into(),
            },
        ),
        // ui 条目注册 plugin:* → ProvenanceMismatch。
        ("plugin:ecc:deploy", CommandSource::Ui),
        // 裸名（Bare 无域）→ ProvenanceMismatch：注册键禁止裸名（设计 §86）。
        ("compact", CommandSource::Core),
        // namespace 段伪造（P1-2 审查跟进，设计 §58）：来源域内标识之外的
        // namespace 一律拒绝——Mcp/Plugin/User 三来源各一例。
        (
            "mcp:other:hello",
            CommandSource::Mcp {
                server: "demo".into(),
            },
        ),
        (
            "plugin:ecc2:deploy",
            CommandSource::Plugin { name: "ecc".into() },
        ),
        (
            "user:other:custom",
            CommandSource::User { name: "me".into() },
        ),
    ];

    for (fullname, source) in cases {
        let reg = CommandRegistry::new();
        let err = reg.register(fake_entry(fullname, source.clone(), &[]));
        assert_eq!(
            err,
            Err(RegisterError::ProvenanceMismatch),
            "fullname = {fullname}"
        );
        assert!(reg.snapshot().is_empty(), "fullname = {fullname}");
    }
}

#[test]
fn register_valid_domains_ok() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(fake_entry("ui:history", CommandSource::Ui, &[]))
        .unwrap();
    reg.register(fake_entry(
        "mcp:demo:hello",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "plugin:ecc:deploy",
        CommandSource::Plugin { name: "ecc".into() },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "user:me:custom",
        CommandSource::User { name: "me".into() },
        &[],
    ))
    .unwrap();
    assert_eq!(reg.snapshot().len(), 5);
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
        "mcp:demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &["da"],
    ))
    .unwrap();
    reg.register(fake_entry(
        "mcp:demo:b",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "mcp:other:c",
        CommandSource::Mcp {
            server: "other".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "mcp:demo2:d",
        CommandSource::Mcp {
            server: "demo2".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(core_entry("compact", &[])).unwrap();

    let n = reg.unregister_namespace("mcp", "demo");
    assert_eq!(n, 2, "仅 mcp:demo: 前缀两条");

    // 前缀边界：demo2 前缀相似但不同，保留。
    assert!(reg.resolve("/mcp:demo:a").is_none());
    assert!(reg.resolve("/mcp:demo:b").is_none());
    assert!(
        reg.resolve("/da").is_none(),
        "alias 随 namespace 批量注销同步清理"
    );
    assert!(reg.resolve("/mcp:other:c").is_some());
    assert!(reg.resolve("/mcp:demo2:d").is_some());
    assert!(reg.resolve("/compact").is_some());
    assert_eq!(reg.snapshot().len(), 3);
}

#[test]
fn unregister_namespace_miss_zero() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry(
        "mcp:demo:a",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();

    assert_eq!(reg.unregister_namespace("mcp", "nope"), 0);
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
        "mcp:demo:a",
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
        "mcp:demo:a",
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

    assert_eq!(reg.unregister_namespace("mcp", "demo"), 1);
    assert_eq!(count.load(Ordering::SeqCst), 4, "namespace 移除 n>0 触发");

    assert_eq!(reg.unregister_namespace("mcp", "demo"), 0);
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
            "mcp:demo:a",
            CommandSource::Mcp {
                server: "demo".into(),
            },
            &[],
        )
    };
    let mcp_b = || {
        fake_entry(
            "mcp:demo:b",
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
    let (removed, added) = reg.reconcile(&["mcp:demo:a".to_string()], vec![mcp_b()]);
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
    let (removed, added) = reg.reconcile(&["mcp:nope".to_string()], vec![mcp_b()]);
    assert_eq!((removed, added), (0, 0));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![1],
        "内容无变化的 reconcile 不得触发 on_change"
    );
}

// ─── resolve 全部失败路径 → None（fall through 裁决） ───────────────

#[test]
fn resolve_failure_paths_none() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();
    reg.register(fake_entry(
        "mcp:demo:hello",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();

    // 空输入 / 纯斜杠。
    assert!(reg.resolve("").is_none());
    assert!(reg.resolve("/").is_none());
    assert!(reg.resolve("   ").is_none());
    // mcp__ 遗留形态：不属任何合法词法，lookup 未命中 → None（不报错）。
    assert!(reg.resolve("/mcp__demo__hello").is_none());
    // 词法非法形态（层数超限 / 未知域 / 第二等级单层）：一律 None。
    assert!(reg.resolve("/a:b:c:d").is_none());
    assert!(reg.resolve("/unknown:x").is_none());
    assert!(reg.resolve("/mcp:hello").is_none());
    // 未注册名称 / 域不对。
    assert!(reg.resolve("/nonexistent").is_none());
    assert!(reg.resolve("/mcp:compact").is_none());
    assert!(reg.resolve("/core:demo:hello").is_none());
    // 第二等级不支持裸名 / 省略 namespace 形态。
    assert!(reg.resolve("/hello").is_none());
    assert!(reg.resolve("/demo:hello").is_none());
}

// ─── 第二等级不登记裸名 / 第一等级 ui 域裸名可用 ────────────────────

#[test]
fn level2_bare_name_not_indexed() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry(
        "mcp:demo:hello",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "mcp:demo:world",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();

    // 第二等级条目不登记裸名（设计 §54：mcp:hello 形态非法），裸名不解析。
    assert!(reg.resolve("/hello").is_none());
    assert!(reg.resolve("/world").is_none());
    assert!(reg.resolve("/demo:hello").is_none());
    // 完整全名正常。
    assert!(reg.resolve("/mcp:demo:hello").is_some());
}

#[test]
fn level1_ui_bare_name_works() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry("ui:history", CommandSource::Ui, &["h"]))
        .unwrap();

    // ui 域同属第一等级：裸名 + alias 均可解析。
    assert_eq!(
        reg.resolve("/history").unwrap().entry.fullname,
        "ui:history"
    );
    assert_eq!(reg.resolve("/h").unwrap().entry.fullname, "ui:history");
}

// ─── snapshot 内容 ─────────────────────────────────────────────────

#[test]
fn snapshot_sorted_contents_and_arc_identity() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry(
        "mcp:demo:hello",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(fake_entry("ui:history", CommandSource::Ui, &[]))
        .unwrap();

    let snap = reg.snapshot();
    assert_eq!(snap.len(), 3);
    // 按 fullname 排序（确定性输出）。
    assert_eq!(
        snap.iter().map(|e| e.fullname.as_str()).collect::<Vec<_>>(),
        ["core:compact", "mcp:demo:hello", "ui:history"]
    );
    // resolve 与 snapshot 返回同一 Arc（单一事实源，无漂移）。
    let resolved = reg.resolve("/compact").unwrap();
    assert!(Arc::ptr_eq(&resolved.entry, &snap[0]));
    assert!(Arc::ptr_eq(
        &reg.resolve("/mcp:demo:hello").unwrap().entry,
        &snap[1]
    ));
}

#[test]
fn snapshot_reflects_register_unregister() {
    let reg = CommandRegistry::new();
    assert!(reg.snapshot().is_empty());

    reg.register(core_entry("compact", &[])).unwrap();
    reg.register(core_entry("rewind", &[])).unwrap();
    assert_eq!(reg.snapshot().len(), 2);

    reg.unregister("core:compact");
    assert_eq!(reg.snapshot().len(), 1);
    assert_eq!(reg.snapshot()[0].fullname, "core:rewind");
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

// ─── 并发 smoke（P2-4 审查跟进：RwLock 锁序 / 两锁原子写入回归护栏） ──
//
// 按审查建议原文实现：多线程并发 register + resolve + unregister_namespace，
// 断言结束后 snapshot 与索引一致、无 panic。三个操作分阶段执行（每阶段
// join 后统一断言），注册循环内不做 resolve 自检——避免与注册写锁竞争的
// 瞬时读与断言耦合（结束态断言覆盖索引一致性即可）。

#[test]
fn concurrent_register_resolve_unregister_smoke() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    let reg = Arc::new(CommandRegistry::new());
    const THREADS: usize = 8;
    const PER_THREAD: usize = 16;

    // 阶段 1：并发注册（每线程独立 namespace，fullname / alias 全局唯一，
    // 键不重叠无冲突）。写锁内原子写入（entries + alias_index 两锁），
    // 锁序 entries → alias_index 一致，不应死锁。
    let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
    for i in 0..THREADS {
        let reg = reg.clone();
        handles.push(thread::spawn(move || {
            let source = CommandSource::Mcp {
                server: format!("srv{i}"),
            };
            for j in 0..PER_THREAD {
                let fullname = format!("mcp:srv{i}:cmd{j}");
                let alias = format!("s{i}c{j}");
                let entry = fake_entry(&fullname, source.clone(), &[alias.as_str()]);
                reg.register(entry)
                    .unwrap_or_else(|e| panic!("并发注册失败 {fullname}: {e:?}"));
            }
        }));
    }
    for h in handles {
        h.join().expect("注册线程无 panic");
    }
    assert_eq!(reg.snapshot().len(), THREADS * PER_THREAD);

    // 阶段 2：并发 resolve（读压；读锁与写锁互斥）。失败仅置标志，
    // 结束后统一断言（不在线程内 panic，避免 unwinding 干扰并发路径）。
    let resolve_failed = Arc::new(AtomicBool::new(false));
    let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
    for i in 0..THREADS {
        let reg = reg.clone();
        let failed = resolve_failed.clone();
        handles.push(thread::spawn(move || {
            for j in 0..PER_THREAD {
                let fullname = format!("mcp:srv{i}:cmd{j}");
                let alias = format!("s{i}c{j}");
                if reg.resolve(&fullname).is_none() || reg.resolve(&alias).is_none() {
                    failed.store(true, Ordering::SeqCst);
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("resolve 线程无 panic");
    }
    assert!(
        !resolve_failed.load(Ordering::SeqCst),
        "并发 resolve 阶段全名 / alias 全部可解析"
    );

    // 快照与索引一致：每个条目按全名 / alias 两路可解析（并发写入无丢失）。
    for entry in reg.snapshot() {
        assert!(reg.resolve(&entry.fullname).is_some());
        for alias in &entry.aliases {
            assert!(reg.resolve(alias).is_some());
        }
    }

    // 并发 namespace 批量注销（各自前缀，键不重叠），结束后注册表为空。
    let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
    for i in 0..THREADS {
        let reg = reg.clone();
        handles.push(thread::spawn(move || {
            let n = reg.unregister_namespace("mcp", &format!("srv{i}"));
            assert_eq!(n, PER_THREAD, "线程 {i} 注销数");
        }));
    }
    for h in handles {
        h.join().expect("注销线程无 panic");
    }
    assert!(reg.snapshot().is_empty(), "全部注销后注册表为空");
    assert!(reg.resolve("/mcp:srv0:cmd0").is_none());
    assert!(reg.resolve("/s0c0").is_none());
}

// ─── Phase 6 A2：来源生命周期状态机 ─────────────────────────────────
// （register_all / project_sources / mark_source_* / clear_source_started；
// 语义逐条对齐 mcp_skills_test.rs 的 McpSkillRegistry 先例）

/// mcp 域条目快捷构造（第二等级，`mcp:{server}:{name}`）。
fn mcp_entry(server: &str, name: &str) -> RouteEntry {
    fake_entry(
        &format!("mcp:{server}:{name}"),
        CommandSource::Mcp {
            server: server.to_string(),
        },
        &[],
    )
}

/// 连接身份 token（type-erased Arc；不同 u32 → 不同指针，ptr_eq 可区分）。
fn token(v: u32) -> HandleToken {
    Arc::new(v)
}

// ─── register_all：部分成功矩阵 + on_change 门控 ──────────────────────

#[test]
fn register_all_partial_success_matrix() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);

    let entries = vec![
        mcp_entry("demo", "hello"),                          // 合法 → 成功
        mcp_entry("demo", "hello"),                          // 同键冲突 → Conflict
        fake_entry("mcp:demo:hi", CommandSource::Core, &[]), // 越权 → ProvenanceMismatch
        fake_entry(
            "mcp__demo__x",
            CommandSource::Mcp {
                server: "demo".into(),
            },
            &[],
        ), // 词法非法 → MalformedName
        mcp_entry("demo", "world"),                          // 合法 → 成功
    ];
    let (ok, errors) = reg.register_all(entries);

    assert_eq!(ok, 2, "部分成功：2 条注册");
    assert_eq!(
        errors,
        vec![
            RegisterError::Conflict {
                key: "mcp:demo:hello".into()
            },
            RegisterError::ProvenanceMismatch,
            RegisterError::MalformedName,
        ],
        "失败错误按输入顺序返回"
    );
    // 成功条目已注册（fullname 排序）；失败条目不占位。
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["mcp:demo:hello", "mcp:demo:world"]);
    // 批量注册合并为单次变更事件。
    assert_eq!(count.load(Ordering::SeqCst), 1, "on_change 恰一次");
}

#[test]
fn register_all_all_failed_no_on_change() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);

    let entries = vec![
        fake_entry("mcp:demo:hi", CommandSource::Core, &[]), // 越权
        fake_entry(
            "mcp__demo__x",
            CommandSource::Mcp {
                server: "demo".into(),
            },
            &[],
        ), // 词法非法
    ];
    let (ok, errors) = reg.register_all(entries);

    assert_eq!(ok, 0);
    assert_eq!(errors.len(), 2);
    assert!(reg.snapshot().is_empty());
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "全部失败内容无变化，不触发"
    );
}

// ─── project_sources：断连注销 + removed_any 门控 ────────────────────

#[test]
fn project_sources_removed_triggers_unregister() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    // 两个已发现来源（demo：2 条；other：1 条）。
    reg.mark_source_started("mcp:demo", h1.clone());
    reg.mark_source_completed(
        "mcp:demo",
        h1.clone(),
        vec![mcp_entry("demo", "hello"), mcp_entry("demo", "world")],
    );
    reg.mark_source_started("mcp:other", h1.clone());
    reg.mark_source_completed("mcp:other", h1, vec![mcp_entry("other", "skill")]);
    assert_eq!(count.load(Ordering::SeqCst), 2, "两次完成回写各触发一次");

    // 断连：other 不在 connected；demo handle 变化（重连）→ 重扫。
    let h2 = token(2);
    let proj = reg.project_sources(&[("mcp:demo".to_string(), h2.clone())]);

    assert!(proj.removed_any, "other 被移除");
    assert_eq!(proj.to_discover.len(), 1, "demo handle 变化 → 重扫");
    assert_eq!(proj.to_discover[0].0, "mcp:demo");
    assert!(
        Arc::ptr_eq(&proj.to_discover[0].1, &h2),
        "重扫携带新 handle"
    );
    // 仅断连来源（other）前缀条目被批量注销；demo 条目保留。
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(
        names,
        vec!["mcp:demo:hello", "mcp:demo:world"],
        "断连按 mcp:other: 前缀批量注销"
    );
    assert_eq!(count.load(Ordering::SeqCst), 3, "断连清理触发恰一次");
}

#[test]
fn project_sources_same_handle_no_rescan_no_fire() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    reg.mark_source_started("mcp:demo", h1.clone());
    reg.mark_source_completed("mcp:demo", h1.clone(), vec![mcp_entry("demo", "hello")]);

    // 同 handle Discovered：不重扫、无移除、不触发。
    let proj = reg.project_sources(&[("mcp:demo".to_string(), h1.clone())]);
    assert!(!proj.removed_any);
    assert!(
        proj.to_discover.is_empty(),
        "同 handle 已 Discovered 不重扫"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1, "无移除不触发");

    // 空 connected：demo 被移除 → 前缀条目注销 + 触发。
    let proj = reg.project_sources(&[]);
    assert!(proj.removed_any);
    assert!(reg.snapshot().is_empty(), "空 connected 全量注销");
    assert_eq!(count.load(Ordering::SeqCst), 2, "空 connected 断连触发一次");
}

// ─── mark_source_started：覆盖矩阵（含 Discovered→Started 撤旧） ─────

#[test]
fn mark_source_started_overwrite_matrix() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    // ① 首次 Started（无状态）→ 不触发。
    reg.mark_source_started("mcp:demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // ② Started → Started（重复 spawn，同 handle）→ 不触发。
    reg.mark_source_started("mcp:demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // ③ Started → Discovered（完成，注册 1 条）→ 触发一次。
    reg.mark_source_completed("mcp:demo", h1.clone(), vec![mcp_entry("demo", "hello")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // ④ Discovered（有条目）→ Started（重连撤旧）→ 先批量注销 + 触发一次。
    reg.mark_source_started("mcp:demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 2, "撤旧触发恰一次");
    assert!(reg.snapshot().is_empty(), "重连撤旧：前缀条目已注销");

    // ⑤ Discovered（无条目）→ Started → 不触发。
    reg.mark_source_completed("mcp:demo", h1.clone(), vec![]);
    assert_eq!(count.load(Ordering::SeqCst), 2, "空完成回写不触发");
    reg.mark_source_started("mcp:demo", h1);
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "Discovered 无条目 → Started 不触发"
    );
}

// ─── mark_source_completed：ptr_eq 防 ABA + 清旧 + on_change 恰一次 ──

#[test]
fn mark_source_completed_without_started_ignored() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);

    // 无 Started 状态（发现任务从未 spawn）→ 回写丢弃，不注册、不触发。
    let n = reg.mark_source_completed("mcp:demo", token(1), vec![mcp_entry("demo", "hello")]);
    assert_eq!(n, 0, "无来源状态不回写");
    assert!(reg.snapshot().is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn mark_source_completed_old_handle_writeback_discarded() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);
    let h2 = token(2);

    reg.mark_source_started("mcp:demo", h1.clone());
    reg.mark_source_completed("mcp:demo", h1.clone(), vec![mcp_entry("demo", "hello")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // 重连：覆盖为 handle 2 的 Started（撤旧触发一次）。
    reg.mark_source_started("mcp:demo", h2.clone());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(reg.snapshot().is_empty());

    // 旧任务（handle 1）回写 → ptr_eq 拒绝，丢弃（无 ABA）。
    let n = reg.mark_source_completed("mcp:demo", h1, vec![mcp_entry("demo", "old")]);
    assert_eq!(n, 0, "旧 handle 回写返回 0");
    assert_eq!(count.load(Ordering::SeqCst), 2, "拒绝回写不触发 on_change");
    assert!(reg.snapshot().is_empty(), "旧任务条目不入主表");

    // 新任务（handle 2）回写 → 应用 + 触发恰一次。
    let n = reg.mark_source_completed("mcp:demo", h2.clone(), vec![mcp_entry("demo", "new")]);
    assert_eq!(n, 1);
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["mcp:demo:new"], "仅新任务条目");
    assert_eq!(count.load(Ordering::SeqCst), 3, "新任务回写触发恰一次");
}

#[test]
fn mark_source_completed_success_fires_once() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    // 首次发现：注册 2 条 → 触发一次（批量回写合并）。
    reg.mark_source_started("mcp:demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0, "Started 置位不触发");
    let n = reg.mark_source_completed(
        "mcp:demo",
        h1.clone(),
        vec![mcp_entry("demo", "hello"), mcp_entry("demo", "world")],
    );
    assert_eq!(n, 2);
    assert_eq!(count.load(Ordering::SeqCst), 1, "批量回写 on_change 恰一次");

    // 重复完成（同 handle，重扫结果收缩）：清旧 2 + 注册 1 → 触发一次。
    let n = reg.mark_source_completed("mcp:demo", h1.clone(), vec![mcp_entry("demo", "hello")]);
    assert_eq!(n, 1);
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "清旧+注册合并为单次变更事件"
    );
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["mcp:demo:hello"], "重扫结果收缩：world 已撤");

    // 部分失败：越权条目跳过 + 告警，不整体回滚。
    let n = reg.mark_source_completed(
        "mcp:demo",
        h1.clone(),
        vec![
            mcp_entry("demo", "hi"),
            fake_entry("mcp:demo:hack", CommandSource::Core, &[]),
        ],
    );
    assert_eq!(n, 1, "越权条目跳过，其余注册");
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["mcp:demo:hi"], "冲突/越权条目不占位");
    assert_eq!(count.load(Ordering::SeqCst), 3, "部分失败仍触发恰一次");
}

// ─── clear_source_started：cancel 回退可重试 ─────────────────────────

#[test]
fn clear_source_started_retryable() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    // 发现任务 spawn → Started（不触发）。
    reg.mark_source_started("mcp:demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0, "Started 置位不触发");

    // cancel：同 handle 回退 Started（不触发）。
    reg.clear_source_started("mcp:demo", h1.clone());
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "cancel 回退不触发 on_change"
    );

    // 下轮可重试：重新 Started → 完成 → 注册成功。
    reg.mark_source_started("mcp:demo", h1.clone());
    let n = reg.mark_source_completed("mcp:demo", h1, vec![mcp_entry("demo", "hello")]);
    assert_eq!(n, 1, "回退后重试注册成功");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn clear_source_started_handle_mismatch_keeps_state() {
    let reg = CommandRegistry::new();
    let h1 = token(1);
    reg.mark_source_started("mcp:demo", h1.clone());

    // 旧 handle 的 cancel → 不移除；状态仍在 → 同 handle 投影不重扫。
    reg.clear_source_started("mcp:demo", token(99));
    let proj = reg.project_sources(&[("mcp:demo".to_string(), h1.clone())]);
    assert!(
        proj.to_discover.is_empty(),
        "handle 不匹配不移除，同 handle 不重扫"
    );

    // 正确 handle 的 cancel → 移除；下轮投影重新进入 to_discover（可重试）。
    reg.clear_source_started("mcp:demo", h1);
    let proj = reg.project_sources(&[("mcp:demo".to_string(), token(2))]);
    assert_eq!(
        proj.to_discover.len(),
        1,
        "回退后同来源重新进入 to_discover"
    );
}
