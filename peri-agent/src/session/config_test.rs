use super::*;

#[test]
fn test_permission_mode_hitl_enabled() {
    assert!(PermissionMode::Default.hitl_enabled());
    assert!(PermissionMode::AcceptEdit.hitl_enabled());
    assert!(PermissionMode::Auto.hitl_enabled());
    assert!(!PermissionMode::Bypass.hitl_enabled());
}

#[test]
fn test_permission_mode_requires_approval() {
    // Default 模式：编辑类工具必审批
    assert!(PermissionMode::Default.requires_approval(true, false));
    // Default 模式：默认列表工具审批
    assert!(PermissionMode::Default.requires_approval(false, true));
    // Default 模式：非默认非编辑不审批
    assert!(!PermissionMode::Default.requires_approval(false, false));

    // Bypass 模式：全部跳过
    assert!(!PermissionMode::Bypass.requires_approval(true, true));
}

#[test]
fn test_session_config_permission_switch() {
    let cfg = SessionConfig::new();
    assert_eq!(cfg.permission_mode(), PermissionMode::Default);

    cfg.set_permission_mode(PermissionMode::Bypass);
    assert_eq!(cfg.permission_mode(), PermissionMode::Bypass);
    assert!(!cfg.permission_mode().hitl_enabled());
}

#[test]
fn test_session_config_cancel() {
    let cfg = SessionConfig::new();
    assert!(!cfg.is_cancelled());

    let child = cfg.cancel_token.child_token();
    cfg.cancel();
    assert!(cfg.is_cancelled());
    assert!(child.is_cancelled(), "子 token 应跟随父 token 取消");
}

#[test]
fn test_session_config_max_iterations() {
    let cfg = SessionConfig::new();
    assert_eq!(cfg.max_iterations(), 500);

    cfg.set_max_iterations(100);
    assert_eq!(cfg.max_iterations(), 100);
}
