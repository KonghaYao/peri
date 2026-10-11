use super::*;

fn session(
    id: &str,
    parent: Option<&str>,
    machine: &str,
    cwd: &str,
    old: Option<WorkspaceId>,
) -> LegacySession {
    LegacySession {
        id: id.to_owned(),
        parent_id: parent.map(str::to_owned),
        machine_id: machine.to_owned(),
        cwd: cwd.into(),
        execution_workspace_id: old,
        derived_root: None,
    }
}

/// 登记行：规划只读 `id` / `root` / `path_source`，其余三列是搬运时的证据（这里给可辨认值）。
fn registration(
    id: WorkspaceId,
    root: &str,
    path_source: WorkspacePathSource,
) -> LegacyRegistration {
    LegacyRegistration {
        id,
        root: root.into(),
        path_source,
        project_id: format!("project-{id}"),
        root_identity: r#"{"device":1,"inode":1}"#.to_owned(),
        discovery: format!(r#"{{"root":{root:?}}}"#),
    }
}

#[test]
fn same_worktree_merges_and_children_inherit_without_losing_cwd() {
    let old_a = WorkspaceId::new();
    let old_b = WorkspaceId::new();
    let roots = [
        registration(old_a, "/repo", WorkspacePathSource::Discovered),
        registration(old_b, "/repo", WorkspacePathSource::Unverified),
    ];
    let sessions = [
        session("root-a", None, "machine-a", "/repo", Some(old_a)),
        session("root-b", None, "machine-a", "/repo/src", Some(old_b)),
        session("child", Some("root-b"), "machine-a", "/repo/src", None),
    ];
    let plan = plan_local_workspaces(&sessions, &roots).unwrap();
    assert_eq!(plan.workspaces.len(), 1);
    assert_eq!(plan.workspaces[0].path, PathBuf::from("/repo"));
    assert_eq!(
        plan.workspaces[0].path_source,
        WorkspacePathSource::Unverified
    );
    let id = plan.session_workspace_ids["root-a"];
    assert_eq!(plan.session_workspace_ids["root-b"], id);
    assert_eq!(plan.session_workspace_ids["child"], id);
    assert_eq!(sessions[1].cwd, PathBuf::from("/repo/src"));
}

#[test]
fn old_workspace_id_shared_across_machines_is_split() {
    let old = WorkspaceId::new();
    let roots = [registration(old, "/repo", WorkspacePathSource::Discovered)];
    let sessions = [
        session("a", None, "machine-a", "/repo", Some(old)),
        session("b", None, "machine-b", "/repo", Some(old)),
    ];
    let plan = plan_local_workspaces(&sessions, &roots).unwrap();
    assert_eq!(plan.workspaces.len(), 2);
    assert_ne!(
        plan.session_workspace_ids["a"],
        plan.session_workspace_ids["b"]
    );
    assert!(plan.workspaces.iter().any(|workspace| workspace.id == old));
}

#[test]
fn damaged_parent_relation_and_relative_cwd_fail_instead_of_reassigning() {
    let no_registrations = [];
    let invalid = [session("a", None, "machine-a", "relative", None)];
    assert!(plan_local_workspaces(&invalid, &no_registrations).is_err());

    let conflicting = [
        session("root", None, "machine-a", "/repo", None),
        session("child", Some("root"), "machine-b", "/repo", None),
    ];
    assert!(plan_local_workspaces(&conflicting, &no_registrations).is_err());

    let cycle = [
        session("a", Some("b"), "machine-a", "/repo", None),
        session("b", Some("a"), "machine-a", "/repo", None),
    ];
    assert!(plan_local_workspaces(&cycle, &no_registrations).is_err());
}

#[test]
fn remote_root_inversion_preserves_posix_and_windows_saved_paths() {
    assert_eq!(
        derive_remote_root("/repo/src/lib", "src/lib").unwrap(),
        PathBuf::from("/repo")
    );
    assert_eq!(
        derive_remote_root("C:\\repo\\src", "src").unwrap(),
        PathBuf::from("C:\\repo")
    );
    assert!(derive_remote_root("/other/src", "repo/src").is_err());
    assert!(derive_remote_root("/repo/src", "../src").is_err());
}

#[test]
fn saved_paths_accept_foreign_absolute_formats_but_reject_relative_paths() {
    for saved in [
        "/",
        "/repo/src",
        "C:/repo",
        r"C:\repo",
        r"\\server\share\repo",
    ] {
        validate_saved_cwd(Path::new(saved)).unwrap();
    }
    for saved in ["", "repo/src", "C:repo", r"\repo", r"\\server"] {
        assert_eq!(
            validate_saved_cwd(Path::new(saved))
                .unwrap_err()
                .to_string(),
            "legacy session path is not absolute"
        );
    }
}

#[tokio::test]
async fn reads_existing_local_registration_and_environment_without_git_probe() {
    use sqlx::{sqlite::SqliteConnectOptions, Connection};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let worktree = directory.path().join("project");
    std::fs::create_dir(&worktree).unwrap();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    for statement in crate::sessions::canonical::V10_CREATE_TABLES
        .iter()
        .chain(crate::sessions::canonical::V10_CREATE_INDEXES)
    {
        sqlx::query(*statement)
            .execute(&mut connection)
            .await
            .unwrap();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let project = uuid::Uuid::new_v4().to_string();
    let registration = WorkspaceId::new();
    let root = worktree.to_str().unwrap();
    let identity = r#"{"device":1,"inode":1}"#;
    let discovery = serde_json::json!({
        "root": root, "root_identity": {"device":1,"inode":1},
        "common_dir": null, "common_identity": null,
        "private_dir": null, "private_identity": null
    })
    .to_string();
    sqlx::query("INSERT INTO projects VALUES (?1, ?2, ?3)")
        .bind(&project)
        .bind(root)
        .bind(identity)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspaces VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(registration.to_string())
        .bind(&project)
        .bind(root)
        .bind(identity)
        .bind(&discovery)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads(id, cwd, created_at, updated_at) VALUES (?1, ?2, 'now', 'now')",
    )
    .bind(&id)
    .bind(root)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO session_bindings VALUES (?1, 1, ?2, ?3, '')")
        .bind(&id)
        .bind(&project)
        .bind(registration.to_string())
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO session_environments(thread_id, machine_id) VALUES (?1, ?2)")
        .bind(&id)
        .bind("00000000-0000-4000-8000-000000000001")
        .execute(&mut connection)
        .await
        .unwrap();
    let input = read_local_plan(&mut connection).await.unwrap();
    assert_eq!(input.plan.workspaces.len(), 1);
    assert_eq!(input.plan.workspaces[0].path, worktree);
    assert_eq!(input.plan.session_workspace_ids[&id], registration);
    // 计划之外的输入也一并读回：登记的归属证据与运行机器是搬运时要用的原始行。
    assert_eq!(input.registrations.len(), 1);
    assert_eq!(input.registrations[0].project_id, project);
    assert_eq!(input.registrations[0].root_identity, identity);
    assert_eq!(input.registrations[0].discovery, discovery);
    assert_eq!(input.sessions.len(), 1);
    assert_eq!(
        input.sessions[0].machine_id,
        "00000000-0000-4000-8000-000000000001"
    );
}
