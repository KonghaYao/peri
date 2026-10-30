use super::*;

#[test]
fn test_workspace_binding_and_scope_wire_roundtrip_preserves_identity() {
    let project_id = "cf082d4c-0c76-4f87-aa6e-abcaec8bdb13"
        .parse::<ProjectId>()
        .unwrap();
    let workspace_id = "a1d2c141-0500-469b-9c83-5e6b22e50c89"
        .parse::<WorkspaceId>()
        .unwrap();
    let binding = SessionBinding {
        schema_version: SESSION_BINDING_VERSION,
        revision: 1,
        project_id,
        workspace_id,
        cwd_relative_to_workspace: PathBuf::from("src/含空格 path"),
    };
    let json = serde_json::to_string(&binding).unwrap();
    assert_eq!(
        serde_json::from_str::<SessionBinding>(&json).unwrap(),
        binding
    );
    assert!(json.contains("cf082d4c-0c76-4f87-aa6e-abcaec8bdb13"));
    let scope = ThreadScope::ExactDirectory {
        workspace_id,
        relative_cwd: binding.cwd_relative_to_workspace,
    };
    assert_eq!(
        serde_json::from_value::<ThreadScope>(serde_json::to_value(&scope).unwrap()).unwrap(),
        scope
    );
}

#[test]
fn test_needs_relink_message_states_a_reachable_next_step() {
    let message = WorkspaceError::NeedsRelink.to_string();
    assert!(
        !message.contains("relinking"),
        "文案不得要求产品中不存在的操作：{message}"
    );
    assert!(
        message.contains("start a new session"),
        "文案必须给出可完成的下一步：{message}"
    );
}

#[test]
fn test_unsupported_schema_version_message_names_both_versions_and_next_step() {
    let message = WorkspaceError::UnsupportedSchemaVersion {
        found: 7,
        supported: 6,
    }
    .to_string();
    assert!(
        message.contains("version 7"),
        "文案必须复述实际版本：{message}"
    );
    assert!(
        message.contains("newest supported: 6"),
        "文案必须给出本构建上限：{message}"
    );
    assert!(
        message.contains("use the Peri version that wrote it") && message.contains("upgrade Peri"),
        "文案必须给出可完成的下一步：{message}"
    );
}

#[test]
fn test_workspace_missing_or_malformed_identity_cannot_deserialize() {
    assert!(serde_json::from_value::<SessionBinding>(
        serde_json::json!({"schema_version": 1, "revision": 1})
    )
    .is_err());
    assert!(serde_json::from_str::<WorkspaceId>("\"/path/is/not/an/id\"").is_err());
    assert!(serde_json::from_str::<ProjectId>("null").is_err());
}

#[test]
fn test_binding_cwd_keeps_workspace_root_without_trailing_separator() {
    let root = Path::new("/a/workspace");
    assert_eq!(binding_cwd(root, Path::new("")), root);
    assert_eq!(
        binding_cwd(root, Path::new("")),
        PathBuf::from("/a/workspace")
    );
    assert_eq!(
        binding_cwd(root, Path::new("sub/dir")),
        PathBuf::from("/a/workspace/sub/dir")
    );
    // 根自身带分隔符时按调用方给出的文本返回，不再叠一层。
    assert_eq!(
        binding_cwd(Path::new("/a/workspace/"), Path::new("")),
        PathBuf::from("/a/workspace/")
    );
}

#[test]
fn test_same_execution_directory_normalizes_text_not_meaning() {
    // 同一目录的等价文本写法。
    assert!(same_execution_directory(
        Path::new("/a/workspace"),
        Path::new("/a/workspace/")
    ));
    assert!(same_execution_directory(
        Path::new("/a/workspace"),
        Path::new("/a//workspace")
    ));
    assert!(same_execution_directory(
        Path::new("/a/workspace"),
        Path::new("/a/./workspace")
    ));
    // 不同目录仍不等；`..` 不折叠（不做 canonicalize）。
    assert!(!same_execution_directory(
        Path::new("/a/workspace"),
        Path::new("/a/workspace/sub")
    ));
    assert!(!same_execution_directory(
        Path::new("/a/workspace"),
        Path::new("/a/workspace/../other")
    ));
    assert!(!same_execution_directory(
        Path::new("/a/workspace"),
        Path::new("/a/other")
    ));
}
