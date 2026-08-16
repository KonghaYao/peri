//! McpSkillRegistry 状态机全路径测试：project_connected 投影矩阵、token
//! 拒绝回写、on_change 触发/不触发矩阵、find 精确 + 别名、顺序确定性。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::*;

fn token(v: u32) -> HandleToken {
    Arc::new(v)
}

fn skill(full_name: &str) -> SkillMetadata {
    SkillMetadata {
        name: full_name.to_string(),
        description: format!("desc of {full_name}"),
        ..SkillMetadata::default()
    }
}

/// started + completed 一步到位（同 handle，模拟发现任务正常生命周期）。
fn complete(
    reg: &McpSkillRegistry,
    server: &str,
    handle: HandleToken,
    entries: Vec<SkillMetadata>,
) {
    reg.mark_discovery_started(server, handle.clone());
    reg.mark_discovery_completed(server, handle, entries);
}

// ─── project_connected：to_discover / removed_any 全矩阵 ───────────────────

#[test]
fn project_connected_new_server_goes_to_discover() {
    let reg = McpSkillRegistry::new();
    let h = token(1);
    let proj = reg.project_connected(&[("srv".to_string(), h.clone())]);
    assert_eq!(proj.to_discover.len(), 1);
    assert_eq!(proj.to_discover[0].0, "srv");
    assert!(!proj.removed_any);
}

#[test]
fn project_connected_empty_registry_empty_connected() {
    let reg = McpSkillRegistry::new();
    let proj = reg.project_connected(&[]);
    assert!(proj.to_discover.is_empty());
    assert!(!proj.removed_any);
}

#[test]
fn project_connected_no_change_when_started_same_handle() {
    let reg = McpSkillRegistry::new();
    let h = token(1);
    reg.mark_discovery_started("srv", h.clone());
    let proj = reg.project_connected(&[("srv".to_string(), h.clone())]);
    assert!(proj.to_discover.is_empty(), "同 handle 已 Started 不应重扫");
    assert!(!proj.removed_any);
}

#[test]
fn project_connected_no_change_when_discovered_same_handle() {
    let reg = McpSkillRegistry::new();
    let h = token(1);
    complete(&reg, "srv", h.clone(), vec![skill("mcp__srv__a")]);
    let proj = reg.project_connected(&[("srv".to_string(), h.clone())]);
    assert!(proj.to_discover.is_empty());
    assert!(!proj.removed_any);
}

#[test]
fn project_connected_disconnect_removes_and_flags() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__a")]);
    let proj = reg.project_connected(&[]);
    assert!(proj.to_discover.is_empty());
    assert!(proj.removed_any, "有条目被移除应 removed_any=true");
    assert!(reg.discovery_state("srv").is_none());
    // 再次投影（无变化）不再报 removed_any
    let proj2 = reg.project_connected(&[]);
    assert!(!proj2.removed_any);
}

#[test]
fn project_connected_disconnect_of_started_also_flags() {
    let reg = McpSkillRegistry::new();
    reg.mark_discovery_started("srv", token(1));
    let proj = reg.project_connected(&[]);
    assert!(proj.removed_any);
}

#[test]
fn project_connected_reconnect_new_handle_rescans() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__a")]);
    // 新 handle（重连）→ 重扫
    let h2 = token(2);
    let proj = reg.project_connected(&[("srv".to_string(), h2.clone())]);
    assert_eq!(proj.to_discover.len(), 1);
    assert_eq!(proj.to_discover[0].0, "srv");
    assert!(!proj.removed_any);
}

#[test]
fn project_connected_mixed_removal_and_discover() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "old", token(1), vec![skill("mcp__old__a")]);
    let h = token(2);
    let proj = reg.project_connected(&[("new".to_string(), h.clone())]);
    assert!(proj.removed_any, "old 被移除");
    assert_eq!(proj.to_discover.len(), 1);
    assert_eq!(proj.to_discover[0].0, "new");
}

// ─── mark_discovery_completed：ptr_eq 拒绝 ─────────────────────────────────

#[test]
fn completed_old_handle_writeback_discarded() {
    let reg = McpSkillRegistry::new();
    reg.mark_discovery_started("srv", token(1));
    // 重连任务覆盖为 handle 2 的 Started
    let h2 = token(2);
    reg.mark_discovery_started("srv", h2.clone());
    // 旧任务（handle 1）回写 → 丢弃
    reg.mark_discovery_completed("srv", token(1), vec![skill("mcp__srv__stale")]);
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Started { handle }) => {
            assert!(Arc::ptr_eq(&handle, &h2), "应保持 handle 2 的 Started");
        }
        other => panic!("应保持 handle 2 的 Started，实际: {other:?}"),
    }
    assert!(reg.all_skills().is_empty(), "旧回写不应产生条目");
}

#[test]
fn completed_matching_handle_applied() {
    let reg = McpSkillRegistry::new();
    let h = token(1);
    reg.mark_discovery_started("srv", h.clone());
    reg.mark_discovery_completed("srv", h, vec![skill("mcp__srv__a")]);
    let skills = reg.all_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "mcp__srv__a");
}

#[test]
fn completed_unknown_server_ignored() {
    let reg = McpSkillRegistry::new();
    reg.mark_discovery_completed("ghost", token(1), vec![skill("mcp__ghost__a")]);
    assert!(reg.discovery_state("ghost").is_none());
}

// ─── on_change 触发/不触发矩阵 ─────────────────────────────────────────────

fn counter_reg() -> (McpSkillRegistry, Arc<AtomicUsize>) {
    let reg = McpSkillRegistry::new();
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    reg.set_on_change(Some(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    })));
    (reg, count)
}

#[test]
fn on_change_fires_on_completed_name_set_change() {
    let (reg, count) = counter_reg();
    let h = token(1);
    reg.mark_discovery_started("srv", h.clone());
    reg.mark_discovery_completed("srv", h.clone(), vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // 再完成一次，name 集变化 → 再触发
    reg.mark_discovery_completed("srv", h, vec![skill("mcp__srv__a"), skill("mcp__srv__b")]);
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn on_change_not_fired_on_completed_same_names() {
    let (reg, count) = counter_reg();
    let h = token(1);
    reg.mark_discovery_started("srv", h.clone());
    reg.mark_discovery_completed("srv", h.clone(), vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // 同名（大小写不同）→ 视为无变化
    reg.mark_discovery_completed("srv", h, vec![skill("MCP__SRV__A")]);
    assert_eq!(count.load(Ordering::SeqCst), 1, "name 集相同不应触发");
}

#[test]
fn on_change_not_fired_on_completed_empty_from_started() {
    let (reg, count) = counter_reg();
    let h = token(1);
    reg.mark_discovery_started("srv", h.clone());
    reg.mark_discovery_completed("srv", h, vec![]);
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "Started→空条目 name 集未变化"
    );
}

#[test]
fn on_change_not_fired_on_started() {
    let (reg, count) = counter_reg();
    reg.mark_discovery_started("srv", token(1));
    reg.mark_discovery_started("srv", token(2));
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn on_change_fires_once_on_started_over_discovered_non_empty() {
    let (reg, count) = counter_reg();
    let h = token(1);
    complete(&reg, "srv", h, vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1, "完成回写触发一次");
    // Discovered(非空) → Started：旧条目消失，触发一次（评审 LOW-2）
    reg.mark_discovery_started("srv", token(2));
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "覆盖非空 Discovered 触发恰一次"
    );
    // Started → Started：不触发
    reg.mark_discovery_started("srv", token(3));
    assert_eq!(count.load(Ordering::SeqCst), 2, "Started→Started 不触发");
    assert!(
        reg.all_skills().is_empty(),
        "Started 覆盖后旧条目从 all_skills 消失"
    );
}

#[test]
fn on_change_not_fired_on_started_over_discovered_empty() {
    let (reg, count) = counter_reg();
    complete(&reg, "srv", token(1), vec![]);
    assert_eq!(count.load(Ordering::SeqCst), 0, "空条目完成不触发");
    // Discovered(空) → Started：无陈旧条目可撤，不触发
    reg.mark_discovery_started("srv", token(2));
    assert_eq!(count.load(Ordering::SeqCst), 0, "覆盖空 Discovered 不触发");
}

#[test]
fn on_change_not_fired_on_started_over_started() {
    let (reg, count) = counter_reg();
    reg.mark_discovery_started("srv", token(1));
    let h2 = token(2);
    reg.mark_discovery_started("srv", h2.clone());
    assert_eq!(count.load(Ordering::SeqCst), 0, "Started→Started 不触发");
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Started { handle }) => {
            assert!(Arc::ptr_eq(&handle, &h2), "覆盖后应持新 handle");
        }
        other => panic!("应 Started: {other:?}"),
    }
}

#[test]
fn on_change_not_fired_on_clear() {
    let (reg, count) = counter_reg();
    let h = token(1);
    reg.mark_discovery_started("srv", h.clone());
    reg.clear_discovery_started("srv", h);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(reg.discovery_state("srv").is_none());
}

#[test]
fn on_change_fired_on_project_connected_removal() {
    let (reg, count) = counter_reg();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    reg.project_connected(&[]);
    assert_eq!(count.load(Ordering::SeqCst), 2, "断连移除应触发 on_change");
}

#[test]
fn on_change_not_fired_on_project_connected_no_removal() {
    let (reg, count) = counter_reg();
    let h = token(1);
    complete(&reg, "srv", h.clone(), vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    reg.project_connected(&[("srv".to_string(), h)]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn on_change_stale_writeback_does_not_fire() {
    let (reg, count) = counter_reg();
    reg.mark_discovery_started("srv", token(1));
    reg.mark_discovery_started("srv", token(2));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    reg.mark_discovery_completed("srv", token(1), vec![skill("mcp__srv__stale")]);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

// ─── refresh_entries（读取面热更新回写）────────────────────────────────────

#[test]
fn refresh_entries_started_state_rejects() {
    let (reg, count) = counter_reg();
    reg.mark_discovery_started("srv", token(1));
    // Started（发现任务进行中，不写——整体覆盖以发现完成为准）→ 拒绝
    assert!(!reg.refresh_entries("srv", &token(1), vec![skill("mcp__srv__a")]));
    assert_eq!(count.load(Ordering::SeqCst), 0, "拒绝回写不触发 on_change");
    assert!(reg.all_skills().is_empty(), "Started 状态不得写入条目");
}

#[test]
fn refresh_entries_handle_mismatch_rejects() {
    let (reg, count) = counter_reg();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // 恢复期间重连/新发现 → 旧 handle（ptr 不同）回写丢弃
    assert!(!reg.refresh_entries("srv", &token(2), vec![skill("mcp__srv__b")]));
    assert_eq!(count.load(Ordering::SeqCst), 1, "拒绝回写不触发 on_change");
    assert_eq!(
        reg.skills_of("srv")[0].name,
        "mcp__srv__a",
        "条目保持原样，不被旧 handle 覆盖"
    );
}

#[test]
fn refresh_entries_matching_handle_writes_and_fires_once() {
    let (reg, count) = counter_reg();
    let h = token(1);
    complete(&reg, "srv", h.clone(), vec![skill("mcp__srv__a")]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // Discovered + 同 handle → 回写成功 + on_change 触发一次
    assert!(reg.refresh_entries("srv", &h, vec![skill("mcp__srv__a"), skill("mcp__srv__b")]));
    assert_eq!(count.load(Ordering::SeqCst), 2, "替换触发恰一次");
    let names: Vec<String> = reg
        .skills_of("srv")
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(
        names,
        vec!["mcp__srv__a".to_string(), "mcp__srv__b".to_string()]
    );
}

#[test]
fn refresh_entries_empty_to_empty_no_fire() {
    let (reg, count) = counter_reg();
    let h = token(1);
    complete(&reg, "srv", h.clone(), vec![]);
    assert_eq!(count.load(Ordering::SeqCst), 0, "空条目完成不触发");
    // 理论态：old/new entries 均为空 → 回写成功但不触发 on_change
    assert!(reg.refresh_entries("srv", &h, vec![]));
    assert_eq!(count.load(Ordering::SeqCst), 0, "两空替换不触发");
    assert!(reg.all_skills().is_empty());
}

// ─── clear_discovery_started ───────────────────────────────────────────────

#[test]
fn clear_started_handle_mismatch_keeps_entry() {
    let reg = McpSkillRegistry::new();
    reg.mark_discovery_started("srv", token(1));
    reg.clear_discovery_started("srv", token(2));
    assert!(reg.discovery_state("srv").is_some(), "handle 不匹配不移除");
}

#[test]
fn clear_discovered_state_not_removed() {
    let reg = McpSkillRegistry::new();
    let h = token(1);
    complete(&reg, "srv", h.clone(), vec![skill("mcp__srv__a")]);
    reg.clear_discovery_started("srv", h);
    assert!(
        matches!(
            reg.discovery_state("srv"),
            Some(ServerDiscoveryState::Discovered { .. })
        ),
        "Discovered 状态不受 clear_discovery_started 影响"
    );
}

// ─── all_skills / skills_of / server_names 顺序与过滤 ─────────────────────

#[test]
fn all_skills_deterministic_order_and_skips_started() {
    let reg = McpSkillRegistry::new();
    // 插入顺序故意与 BTreeMap 键序不同
    complete(
        &reg,
        "zeta",
        token(1),
        vec![skill("mcp__zeta__1"), skill("mcp__zeta__2")],
    );
    complete(&reg, "alpha", token(2), vec![skill("mcp__alpha__x")]);
    complete(&reg, "middle", token(3), vec![]); // 空条目
    reg.mark_discovery_started("omega", token(4)); // Started → 跳过

    let names: Vec<String> = reg.all_skills().iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        vec!["mcp__alpha__x", "mcp__zeta__1", "mcp__zeta__2"],
        "BTreeMap 键序 + 每 server 条目序；Started/空条目跳过"
    );
}

#[test]
fn skills_of_single_server() {
    let reg = McpSkillRegistry::new();
    complete(
        &reg,
        "srv",
        token(1),
        vec![skill("mcp__srv__a"), skill("mcp__srv__b")],
    );
    let names: Vec<String> = reg
        .skills_of("srv")
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names, vec!["mcp__srv__a", "mcp__srv__b"]);
    assert!(reg.skills_of("ghost").is_empty());
}

#[test]
fn server_names_only_discovered_non_empty() {
    let reg = McpSkillRegistry::new();
    complete(
        &reg,
        "with_skills",
        token(1),
        vec![skill("mcp__with_skills__a")],
    );
    complete(&reg, "empty", token(2), vec![]);
    reg.mark_discovery_started("pending", token(3));
    assert_eq!(reg.server_names(), vec!["with_skills"]);
}

// ─── find：精确 + 别名 ─────────────────────────────────────────────────────

#[test]
fn find_exact_case_insensitive() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__MySkill")]);
    let hit = reg.find("MCP__SRV__MYSKILL").expect("大小写无关精确匹配");
    assert_eq!(hit.name, "mcp__srv__MySkill");
}

#[test]
fn find_alias_server_colon_skill() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__MySkill")]);
    let hit = reg.find("srv:MySkill").expect("别名应命中");
    assert_eq!(hit.name, "mcp__srv__MySkill");
    // 别名拼名必须与 mcp_skill_name 公共 helper 一致（SkillTool 别名分支同源）
    assert_eq!(hit.name, mcp_skill_name("srv", "MySkill"));
    // 大小写无关别名
    assert!(reg.find("SRV:myskill").is_some());
}

#[test]
fn find_miss_returns_none() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__a")]);
    assert!(reg.find("other__a").is_none());
    assert!(reg.find("srv:").is_none(), "空后缀不构成别名");
    assert!(reg.find("mcp__srv__b").is_none());
}

#[test]
fn find_prefers_exact_over_alias() {
    let reg = McpSkillRegistry::new();
    complete(&reg, "srv", token(1), vec![skill("mcp__srv__x")]);
    complete(&reg, "srv:x", token(2), vec![skill("mcp__srv:x__y")]);
    // 精确未中 "srv:x"（没有叫这个全名的 skill）→ 走别名拼 mcp__srv__x
    let hit = reg.find("srv:x").expect("别名应命中 mcp__srv__x");
    assert_eq!(hit.name, "mcp__srv__x");
}

// ─── mcp_skill_name ────────────────────────────────────────────────────────

#[test]
fn mcp_skill_name_format() {
    assert_eq!(
        mcp_skill_name("github", "code-review"),
        "mcp__github__code-review"
    );
}
