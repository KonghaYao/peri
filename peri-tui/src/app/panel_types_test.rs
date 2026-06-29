use super::*;

#[test]
fn test_panel_scope() {
    assert_eq!(PanelKind::Model.scope(), PanelScope::Session);
    assert_eq!(PanelKind::Login.scope(), PanelScope::Session);
    assert_eq!(PanelKind::Agent.scope(), PanelScope::Session);
    assert_eq!(PanelKind::Hooks.scope(), PanelScope::Session);
    assert_eq!(PanelKind::Config.scope(), PanelScope::Session);
    assert_eq!(PanelKind::ThreadBrowser.scope(), PanelScope::Session);
    assert_eq!(PanelKind::Mcp.scope(), PanelScope::Global);
    assert_eq!(PanelKind::Plugin.scope(), PanelScope::Global);
    assert_eq!(PanelKind::Cron.scope(), PanelScope::Global);
    assert_eq!(PanelKind::Status.scope(), PanelScope::Global);
    assert_eq!(PanelKind::Memory.scope(), PanelScope::Global);
    assert_eq!(PanelKind::Betas.scope(), PanelScope::Global);
    assert_eq!(PanelKind::Workflow.scope(), PanelScope::Global);
}

#[test]
fn test_mutex_group() {
    assert_eq!(PanelKind::Model.mutex_group(), MutexGroup::Settings);
    assert_eq!(PanelKind::Config.mutex_group(), MutexGroup::Settings);
    assert_eq!(PanelKind::Agent.mutex_group(), MutexGroup::Agent);
    assert_eq!(PanelKind::Mcp.mutex_group(), MutexGroup::Tools);
    assert_eq!(PanelKind::Status.mutex_group(), MutexGroup::Info);
    assert_eq!(PanelKind::ThreadBrowser.mutex_group(), MutexGroup::Thread);
}

#[test]
fn test_panel_kind_priority_ordering() {
    // 优先级从小到大排列
    assert!(PanelKind::Agent.priority() < PanelKind::Hooks.priority());
    assert!(PanelKind::Hooks.priority() < PanelKind::Model.priority());
    assert!(PanelKind::Model.priority() < PanelKind::Login.priority());
    assert!(PanelKind::Login.priority() < PanelKind::Config.priority());
    assert!(PanelKind::Config.priority() < PanelKind::Mcp.priority());
    assert!(PanelKind::Mcp.priority() < PanelKind::Plugin.priority());
    assert!(PanelKind::Plugin.priority() < PanelKind::Cron.priority());
    assert!(PanelKind::Cron.priority() < PanelKind::Status.priority());
    assert!(PanelKind::Status.priority() < PanelKind::Memory.priority());
    assert!(PanelKind::Memory.priority() < PanelKind::Tasks.priority());
    assert!(PanelKind::Tasks.priority() < PanelKind::Betas.priority());
    assert!(PanelKind::Betas.priority() < PanelKind::Workflow.priority());
}
