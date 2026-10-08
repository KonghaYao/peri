//! 定向测试：grant / revoke / 摘要变化失效 / 非交互默认拒绝。
//!
//! `set_global_config_path` 是进程级全局（数据面唯一实例），全部用 `#[serial]`
//! 串行，且每个用例把信任文件指向自己的 tempdir，不触碰真实用户目录。

use std::path::Path;

use serial_test::serial;

use super::*;

struct TestScope {
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

impl TestScope {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        // 信任文件跟随选中 global 配置：<temp>/peri/settings.json → <temp>/peri/hook-trust.json
        crate::io::set_global_config_path(Some(home.path().join("peri").join("settings.json")));
        Self {
            _home: home,
            workspace,
        }
    }

    fn workspace(&self) -> &Path {
        self.workspace.path()
    }

    fn write_settings(&self, kind: SettingsSourceKind, content: &str) {
        let path = self.workspace().join(kind.settings_relative_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn binding(&self, kind: SettingsSourceKind) -> HookTrustEntry {
        settings_binding(self.workspace(), kind)
            .unwrap()
            .expect("workspace exists on disk")
    }
}

#[test]
#[serial]
fn non_interactive_default_is_denied_without_any_grant() {
    let scope = TestScope::new();
    scope.write_settings(SettingsSourceKind::Project, r#"{"hooks":{}}"#);
    let binding = scope.binding(SettingsSourceKind::Project);
    assert!(
        !is_trusted(&binding).unwrap(),
        "无任何授权记录时必须默认拒绝（非交互不弹窗、不执行）"
    );
    assert!(!trust_store_path().exists(), "默认拒绝阶段不得创建信任文件");
}

#[test]
#[serial]
fn grant_allows_and_revoke_restores_denial() {
    let scope = TestScope::new();
    scope.write_settings(SettingsSourceKind::Project, r#"{"hooks":{}}"#);
    let binding = scope.binding(SettingsSourceKind::Project);

    grant(&binding).unwrap();
    assert!(is_trusted(&binding).unwrap(), "显式授权后来源应被放行");
    assert_eq!(list(&binding.workspace).unwrap(), vec![binding.clone()]);

    assert!(revoke(&binding.workspace, &binding.source).unwrap());
    assert!(!is_trusted(&binding).unwrap(), "撤销后必须立即回到拒绝");
    assert!(
        !revoke(&binding.workspace, &binding.source).unwrap(),
        "重复撤销返回未命中"
    );
}

#[test]
#[serial]
fn digest_change_invalidates_existing_grant() {
    let scope = TestScope::new();
    scope.write_settings(
        SettingsSourceKind::Project,
        r#"{"hooks":{"PreToolUse":[]}}"#,
    );
    let before = scope.binding(SettingsSourceKind::Project);
    grant(&before).unwrap();
    assert!(is_trusted(&before).unwrap());

    // 项目改写了 settings.json：来源摘要变化 ⇒ 既有授权失效（必须重新授权）
    scope.write_settings(
        SettingsSourceKind::Project,
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo changed"}]}]}}"#,
    );
    let after = scope.binding(SettingsSourceKind::Project);
    assert_ne!(before.digest, after.digest, "内容变化必须改变摘要");
    assert!(
        !is_trusted(&after).unwrap(),
        "来源摘要变化后旧授权不得继续生效"
    );

    grant(&after).unwrap();
    assert!(is_trusted(&after).unwrap(), "重新授权新摘要后放行");
    assert!(!is_trusted(&before).unwrap(), "旧摘要授权不得复活");
}

#[test]
#[serial]
fn source_change_does_not_borrow_other_source_grant() {
    let scope = TestScope::new();
    scope.write_settings(SettingsSourceKind::Project, r#"{"hooks":{}}"#);
    scope.write_settings(SettingsSourceKind::Local, r#"{"hooks":{}}"#);
    let project = scope.binding(SettingsSourceKind::Project);
    let local = scope.binding(SettingsSourceKind::Local);
    grant(&project).unwrap();

    assert!(is_trusted(&project).unwrap());
    assert!(
        !is_trusted(&local).unwrap(),
        "project 的授权不得被 local 来源借用"
    );
}

#[test]
#[serial]
fn other_workspace_does_not_borrow_workspace_grant() {
    let scope = TestScope::new();
    scope.write_settings(SettingsSourceKind::Project, r#"{"hooks":{}}"#);
    let binding = scope.binding(SettingsSourceKind::Project);
    grant(&binding).unwrap();

    let other = tempfile::tempdir().unwrap();
    let other_binding = settings_binding(other.path(), SettingsSourceKind::Project)
        .unwrap()
        .expect("other workspace exists");
    assert_ne!(binding.workspace, other_binding.workspace);
    assert!(
        !is_trusted(&other_binding).unwrap(),
        "授权绑定 canonical workspace，不得跨 workspace 生效"
    );
    assert!(list(&other_binding.workspace).unwrap().is_empty());
}

#[test]
#[serial]
fn regrant_updates_digest_without_duplicate_entries() {
    let scope = TestScope::new();
    scope.write_settings(SettingsSourceKind::Project, "{}");
    let first = scope.binding(SettingsSourceKind::Project);
    grant(&first).unwrap();

    scope.write_settings(SettingsSourceKind::Project, r#"{"hooks":{}}"#);
    let second = scope.binding(SettingsSourceKind::Project);
    grant(&second).unwrap();
    grant(&second).unwrap();

    let entries = list(&second.workspace).unwrap();
    assert_eq!(entries, vec![second.clone()], "同来源只保留一条最新记录");
}

#[test]
#[serial]
fn missing_settings_file_has_no_digest_collision_with_empty_file() {
    let scope = TestScope::new();
    let missing = scope.binding(SettingsSourceKind::Project);
    grant(&missing).unwrap();
    assert!(is_trusted(&missing).unwrap());
    scope.write_settings(SettingsSourceKind::Project, "");
    let empty = scope.binding(SettingsSourceKind::Project);
    assert_ne!(
        missing.digest, empty.digest,
        "缺失文件与空文件是不同来源状态，不得共享授权"
    );
    assert!(!is_trusted(&empty).unwrap());
    grant(&empty).unwrap();
    assert!(is_trusted(&empty).unwrap());
}

#[test]
#[serial]
fn invalid_store_content_is_a_data_error_not_silent_deny() {
    let scope = TestScope::new();
    let binding = scope.binding(SettingsSourceKind::Project);
    grant(&binding).unwrap();
    std::fs::write(trust_store_path(), "{ not json").unwrap();

    let error = is_trusted(&binding).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
