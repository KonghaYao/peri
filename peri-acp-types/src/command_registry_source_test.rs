use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::{CommandRegistry, HandleToken, RegisterError};
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

fn install_counter(reg: &CommandRegistry) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    reg.set_on_change(Some(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    })));
    count
}
/// mcp 域条目快捷构造（决策 1：`{server}:{name}`，server 名即词法首段域）。
fn mcp_entry(server: &str, name: &str) -> RouteEntry {
    fake_entry(
        &format!("{server}:{name}"),
        CommandSource::Mcp {
            server: server.to_string(),
        },
        &[],
    )
}

/// 审查 B1 防线 2：Mcp provenance 强制 Level2Short——server 名恰为保留域
/// 时 fullname 被解析为 Level1/Level2，注册必须拒绝（防裸名路由污染与
/// 断连误删内置域条目；源头跳过见 skill_discovery::mcp_namespace_reserved）。
#[test]
fn mcp_entry_reserved_domain_rejected() {
    let reg = CommandRegistry::new();
    for reserved in ["core", "ui", "plugin", "user", "mcp"] {
        let result = reg.register(mcp_entry(reserved, "hello"));
        // core/ui：parse 为 Level1 → 防线 2 ProvenanceMismatch；plugin/user/
        // mcp：第二等级域缺 namespace 段 → 词法层 MalformedName。均为拒绝。
        assert!(
            matches!(
                result,
                Err(RegisterError::ProvenanceMismatch) | Err(RegisterError::MalformedName)
            ),
            "server 名 {reserved} 应拒绝（Level1/Level2 形态）: {result:?}"
        );
        // 拒绝即不残留：裸名/全名均不可路由。
        assert!(reg.resolve(reserved).is_none());
        assert!(reg.resolve(&format!("{reserved}:hello")).is_none());
    }
    // 非保留域正常注册（对照）。
    assert!(reg.register(mcp_entry("demo", "hello")).is_ok());
    assert!(reg.resolve("demo:hello").is_some());
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
                key: "demo:hello".into()
            },
            RegisterError::ProvenanceMismatch,
            RegisterError::MalformedName,
        ],
        "失败错误按输入顺序返回"
    );
    // 成功条目已注册（fullname 排序）；失败条目不占位。
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["demo:hello", "demo:world"]);
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
    reg.mark_source_started("demo", h1.clone());
    reg.mark_source_completed(
        "demo",
        h1.clone(),
        vec![mcp_entry("demo", "hello"), mcp_entry("demo", "world")],
    );
    reg.mark_source_started("other", h1.clone());
    reg.mark_source_completed("other", h1, vec![mcp_entry("other", "skill")]);
    assert_eq!(count.load(Ordering::SeqCst), 2, "两次完成回写各触发一次");

    // 断连：other 不在 connected；demo handle 变化（重连）→ 重扫。
    let h2 = token(2);
    let proj = reg.project_sources(&[("demo".to_string(), h2.clone())]);

    assert!(proj.removed_any, "other 被移除");
    assert_eq!(proj.to_discover.len(), 1, "demo handle 变化 → 重扫");
    assert_eq!(proj.to_discover[0].0, "demo");
    assert!(
        Arc::ptr_eq(&proj.to_discover[0].1, &h2),
        "重扫携带新 handle"
    );
    // 仅断连来源（other）前缀条目被批量注销；demo 条目保留。
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(
        names,
        vec!["demo:hello", "demo:world"],
        "断连按 other: 前缀批量注销"
    );
    assert_eq!(count.load(Ordering::SeqCst), 3, "断连清理触发恰一次");
}

#[test]
fn project_sources_same_handle_no_rescan_no_fire() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    reg.mark_source_started("demo", h1.clone());
    reg.mark_source_completed("demo", h1.clone(), vec![mcp_entry("demo", "hello")]);

    // 同 handle Discovered：不重扫、无移除、不触发。
    let proj = reg.project_sources(&[("demo".to_string(), h1.clone())]);
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
    reg.mark_source_started("demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // ② Started → Started（重复 spawn，同 handle）→ 不触发。
    reg.mark_source_started("demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // ③ Started → Discovered（完成，注册 1 条）→ 触发一次。
    reg.mark_source_completed("demo", h1.clone(), vec![mcp_entry("demo", "hello")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // ④ Discovered（有条目）→ Started（重连撤旧）→ 先批量注销 + 触发一次。
    reg.mark_source_started("demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 2, "撤旧触发恰一次");
    assert!(reg.snapshot().is_empty(), "重连撤旧：前缀条目已注销");

    // ⑤ Discovered（无条目）→ Started → 不触发。
    reg.mark_source_completed("demo", h1.clone(), vec![]);
    assert_eq!(count.load(Ordering::SeqCst), 2, "空完成回写不触发");
    reg.mark_source_started("demo", h1);
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
    let n = reg.mark_source_completed("demo", token(1), vec![mcp_entry("demo", "hello")]);
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

    reg.mark_source_started("demo", h1.clone());
    reg.mark_source_completed("demo", h1.clone(), vec![mcp_entry("demo", "hello")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // 重连：覆盖为 handle 2 的 Started（撤旧触发一次）。
    reg.mark_source_started("demo", h2.clone());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(reg.snapshot().is_empty());

    // 旧任务（handle 1）回写 → ptr_eq 拒绝，丢弃（无 ABA）。
    let n = reg.mark_source_completed("demo", h1, vec![mcp_entry("demo", "old")]);
    assert_eq!(n, 0, "旧 handle 回写返回 0");
    assert_eq!(count.load(Ordering::SeqCst), 2, "拒绝回写不触发 on_change");
    assert!(reg.snapshot().is_empty(), "旧任务条目不入主表");

    // 新任务（handle 2）回写 → 应用 + 触发恰一次。
    let n = reg.mark_source_completed("demo", h2.clone(), vec![mcp_entry("demo", "new")]);
    assert_eq!(n, 1);
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["demo:new"], "仅新任务条目");
    assert_eq!(count.load(Ordering::SeqCst), 3, "新任务回写触发恰一次");
}

#[test]
fn mark_source_completed_success_fires_once() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    // 首次发现：注册 2 条 → 触发一次（批量回写合并）。
    reg.mark_source_started("demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0, "Started 置位不触发");
    let n = reg.mark_source_completed(
        "demo",
        h1.clone(),
        vec![mcp_entry("demo", "hello"), mcp_entry("demo", "world")],
    );
    assert_eq!(n, 2);
    assert_eq!(count.load(Ordering::SeqCst), 1, "批量回写 on_change 恰一次");

    // 重复完成（同 handle，重扫结果收缩）：清旧 2 + 注册 1 → 触发一次。
    let n = reg.mark_source_completed("demo", h1.clone(), vec![mcp_entry("demo", "hello")]);
    assert_eq!(n, 1);
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "清旧+注册合并为单次变更事件"
    );
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["demo:hello"], "重扫结果收缩：world 已撤");

    // 部分失败：越权条目跳过 + 告警，不整体回滚。
    let n = reg.mark_source_completed(
        "demo",
        h1.clone(),
        vec![
            mcp_entry("demo", "hi"),
            fake_entry("mcp:demo:hack", CommandSource::Core, &[]),
        ],
    );
    assert_eq!(n, 1, "越权条目跳过，其余注册");
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert_eq!(names, vec!["demo:hi"], "冲突/越权条目不占位");
    assert_eq!(count.load(Ordering::SeqCst), 3, "部分失败仍触发恰一次");
}

// ─── clear_source_started：cancel 回退可重试 ─────────────────────────

#[test]
fn clear_source_started_retryable() {
    let reg = CommandRegistry::new();
    let count = install_counter(&reg);
    let h1 = token(1);

    // 发现任务 spawn → Started（不触发）。
    reg.mark_source_started("demo", h1.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0, "Started 置位不触发");

    // cancel：同 handle 回退 Started（不触发）。
    reg.clear_source_started("demo", h1.clone());
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "cancel 回退不触发 on_change"
    );

    // 下轮可重试：重新 Started → 完成 → 注册成功。
    reg.mark_source_started("demo", h1.clone());
    let n = reg.mark_source_completed("demo", h1, vec![mcp_entry("demo", "hello")]);
    assert_eq!(n, 1, "回退后重试注册成功");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn clear_source_started_handle_mismatch_keeps_state() {
    let reg = CommandRegistry::new();
    let h1 = token(1);
    reg.mark_source_started("demo", h1.clone());

    // 旧 handle 的 cancel → 不移除；状态仍在 → 同 handle 投影不重扫。
    reg.clear_source_started("demo", token(99));
    let proj = reg.project_sources(&[("demo".to_string(), h1.clone())]);
    assert!(
        proj.to_discover.is_empty(),
        "handle 不匹配不移除，同 handle 不重扫"
    );

    // 正确 handle 的 cancel → 移除；下轮投影重新进入 to_discover（可重试）。
    reg.clear_source_started("demo", h1);
    let proj = reg.project_sources(&[("demo".to_string(), token(2))]);
    assert_eq!(
        proj.to_discover.len(),
        1,
        "回退后同来源重新进入 to_discover"
    );
}
