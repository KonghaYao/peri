//! 插件面板选择入口：把选中插件加入/移出下一次任务的待提交上下文。

use super::toggle_pending_plugin;
use crate::application::operations::OperationScope;
use crate::kit::atoms::{PENDING_PLUGIN_SELECTIONS, PLUGIN_LIST, PluginSummary};
use peri_acp_types::session::PluginSelection;

fn plugin(name: &str, marketplace: &str) -> PluginSummary {
    PluginSummary {
        name: name.into(),
        marketplace: marketplace.into(),
        ..PluginSummary::default()
    }
}

#[test]
#[serial_test::serial]
fn test_toggle_pending_plugin_adds_then_removes_selected_plugin() {
    crate::kit::atoms::init_atoms();
    {
        let steers_atom = crate::kit::steer_state::STEERS.state();
        let mut steers = steers_atom.write();
        steers.enabled = true;
        steers.plugin_selections = true;
    }
    PENDING_PLUGIN_SELECTIONS.set(Vec::new());
    PLUGIN_LIST.set(vec![plugin("alpha", ""), plugin("beta", "market")]);
    let scope = OperationScope::new("session", 0);

    toggle_pending_plugin(1, &scope);
    assert_eq!(
        PENDING_PLUGIN_SELECTIONS.state().read().clone(),
        vec![PluginSelection::new("beta@market")],
        "只登记插件 ID（名称@marketplace），不携带指引或凭证"
    );

    toggle_pending_plugin(1, &scope);
    assert!(
        PENDING_PLUGIN_SELECTIONS.state().read().is_empty(),
        "再次选择同一插件必须移出待提交上下文"
    );

    crate::kit::steer_state::STEERS.state().write().enabled = false;
    PLUGIN_LIST.set(Vec::new());
}

/// 能力热降级（true → false）后，已登记的选择仍必须能从面板移除；
/// 否则 `commit_input` 的能力门会把提交永久阻断，用户没有清空路径。
#[test]
#[serial_test::serial]
fn test_toggle_pending_plugin_can_remove_after_capability_downgrade() {
    crate::kit::atoms::init_atoms();
    {
        let steers_atom = crate::kit::steer_state::STEERS.state();
        let mut steers = steers_atom.write();
        steers.enabled = true;
        steers.plugin_selections = true;
    }
    PENDING_PLUGIN_SELECTIONS.set(vec![PluginSelection::new("alpha")]);
    PLUGIN_LIST.set(vec![plugin("alpha", "")]);
    *crate::kit::atoms::NOTIFICATION.state().write() = None;
    let scope = OperationScope::new("session", 0);

    // 服务端重新协商后收回子能力，已登记的选择仍在待提交集合里。
    crate::kit::steer_state::STEERS
        .state()
        .write()
        .plugin_selections = false;
    toggle_pending_plugin(0, &scope);

    assert!(
        PENDING_PLUGIN_SELECTIONS.state().read().is_empty(),
        "移除路径不得被能力门阻断"
    );
    assert!(
        crate::kit::atoms::NOTIFICATION.state().read().is_some(),
        "移除必须给出可见反馈"
    );

    crate::kit::steer_state::STEERS.state().write().enabled = false;
    PLUGIN_LIST.set(Vec::new());
}

#[test]
#[serial_test::serial]
fn test_toggle_pending_plugin_reports_unavailable_capability() {
    crate::kit::atoms::init_atoms();
    crate::kit::steer_state::STEERS.state().write().enabled = false;
    PENDING_PLUGIN_SELECTIONS.set(Vec::new());
    PLUGIN_LIST.set(vec![plugin("alpha", "")]);
    // 上一测试可能留下移除提示；先清空才能证明本次拒绝本身产生了反馈。
    *crate::kit::atoms::NOTIFICATION.state().write() = None;

    toggle_pending_plugin(0, &OperationScope::new("session", 0));

    assert!(
        PENDING_PLUGIN_SELECTIONS.state().read().is_empty(),
        "服务端未协商用户输入队列时不得静默记录选择"
    );
    assert!(
        crate::kit::atoms::NOTIFICATION.state().read().is_some(),
        "必须明确提示选择入口不可用"
    );

    PLUGIN_LIST.set(Vec::new());
}
