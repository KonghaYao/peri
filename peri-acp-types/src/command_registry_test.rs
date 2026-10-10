use std::sync::Arc;

use async_trait::async_trait;

use super::{CommandRegistry, RegisterError};
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
fn rejected_registration_does_not_reserve_nonconflicting_aliases() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["taken"])).unwrap();
    let rejected = core_entry("rejected", &["free", "taken"]);
    assert_eq!(
        reg.register(rejected),
        Err(RegisterError::Conflict {
            key: "taken".into()
        })
    );
    assert!(reg.resolve("/rejected").is_none());
    assert!(reg.resolve("/free").is_none());
    reg.register(core_entry("replacement", &["free"])).unwrap();
    assert_eq!(
        reg.resolve("/free").unwrap().entry.fullname,
        "core:replacement"
    );
    assert_eq!(
        reg.resolve("/taken").unwrap().entry.fullname,
        "core:compact"
    );
    assert_eq!(reg.snapshot().len(), 2);
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
        "demo:hello",
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

// ─── resolve 全部失败路径 → None（fall through 裁决） ───────────────

#[test]
fn resolve_failure_paths_none() {
    let reg = CommandRegistry::new();
    reg.register(core_entry("compact", &["c"])).unwrap();
    reg.register(fake_entry(
        "demo:hello",
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
    // 决策 1 Mcp：不登记裸名（/hello none）；命令全名 /demo:hello 精确命中；
    // 同 server 未注册 skill（/demo:bye）与未注册 server（/other:hello）miss。
    assert!(reg.resolve("/hello").is_none());
    assert!(reg.resolve("/demo:hello").is_some());
    assert!(reg.resolve("/demo:bye").is_none());
    assert!(reg.resolve("/other:hello").is_none());
}

// ─── 第二等级不登记裸名 / 第一等级 ui 域裸名可用 ────────────────────

#[test]
fn level2_bare_name_not_indexed() {
    let reg = CommandRegistry::new();
    reg.register(fake_entry(
        "demo:hello",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();
    reg.register(fake_entry(
        "demo:world",
        CommandSource::Mcp {
            server: "demo".into(),
        },
        &[],
    ))
    .unwrap();

    // 决策 1：词法地位等同 Level2，不登记裸名（hello/world 不解析）。
    assert!(reg.resolve("/hello").is_none());
    assert!(reg.resolve("/world").is_none());
    // 完整全名正常；旧 3 段 mcp:demo:hello 形态下无此键。
    assert!(reg.resolve("/demo:hello").is_some());
    assert!(reg.resolve("/mcp:demo:hello").is_none());
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
        "demo:hello",
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
        ["core:compact", "demo:hello", "ui:history"]
    );
    // resolve 与 snapshot 返回同一 Arc（单一事实源，无漂移）。
    let resolved = reg.resolve("/compact").unwrap();
    assert!(Arc::ptr_eq(&resolved.entry, &snap[0]));
    assert!(Arc::ptr_eq(
        &reg.resolve("/demo:hello").unwrap().entry,
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
                let fullname = format!("srv{i}:cmd{j}");
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
                let fullname = format!("srv{i}:cmd{j}");
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
            let n = reg.unregister_namespace(&format!("srv{i}"), "");
            assert_eq!(n, PER_THREAD, "线程 {i} 注销数");
        }));
    }
    for h in handles {
        h.join().expect("注销线程无 panic");
    }
    assert!(reg.snapshot().is_empty(), "全部注销后注册表为空");
    assert!(reg.resolve("/srv0:cmd0").is_none());
    assert!(reg.resolve("/s0c0").is_none());
}
