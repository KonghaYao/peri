use clap::Parser;

use super::{resource_input, Args, OwnerIncarnationGuard};

#[test]
fn crashed_owner_marker_blocks_new_incarnation_until_cleanup_proven() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, [3u8; 32]).unwrap();
    let guard = OwnerIncarnationGuard::claim(&secret).unwrap();
    assert!(OwnerIncarnationGuard::claim(&secret).is_err());
    drop(guard); // Simulates an abrupt process exit: Drop cannot claim cleanup.
    assert!(OwnerIncarnationGuard::claim(&secret).is_err());
    let marker = dir.path().join("secret.workspace-owner-unclean");
    std::fs::remove_file(&marker).unwrap(); // External cleanup proof is required.
    let guard = OwnerIncarnationGuard::claim(&secret).unwrap();
    guard.clear_after_cleanup().unwrap();
    assert!(OwnerIncarnationGuard::claim(&secret).is_ok());
}

#[test]
fn transport_is_required_and_exclusive() {
    let missing = Args::try_parse_from(["worker", "--workspace", "."])
        .unwrap_err()
        .to_string();
    assert!(missing.contains("--stdio"));
    let both = Args::try_parse_from(["worker", "--workspace", ".", "--stdio", "--http"])
        .unwrap_err()
        .to_string();
    assert!(both.contains("cannot be used with"));
}

#[test]
fn no_roots_flags_reject_plugin_roots() {
    let error = Args::try_parse_from([
        "worker",
        "--workspace",
        ".",
        "--stdio",
        "--no-skill-roots",
        "--plugin-skill-root",
        "demo=.",
    ])
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot be used with"));
}

#[test]
fn explicit_empty_roots_override_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let args = Args::try_parse_from([
        "worker",
        "--workspace",
        directory.path().to_str().unwrap(),
        "--stdio",
        "--no-skill-roots",
        "--no-agent-roots",
    ])
    .unwrap();
    let input = resource_input(&args, directory.path()).unwrap();
    assert!(input.skill_roots.is_empty());
    assert!(input.agent_roots.is_empty());
}

#[test]
fn omitted_roots_publish_project_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let args = Args::try_parse_from([
        "worker",
        "--workspace",
        directory.path().to_str().unwrap(),
        "--stdio",
    ])
    .unwrap();
    let input = resource_input(&args, directory.path()).unwrap();
    assert!(input
        .skill_roots
        .iter()
        .any(|root| root.path == directory.path().join(".claude/skills")));
    assert_eq!(input.agent_roots.len(), 2);
}

#[test]
fn supplied_skill_root_must_be_a_directory() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("not-a-directory");
    std::fs::write(&file, "x").unwrap();
    let args = Args::try_parse_from([
        "worker",
        "--workspace",
        directory.path().to_str().unwrap(),
        "--stdio",
        "--skill-root",
        file.to_str().unwrap(),
    ])
    .unwrap();
    let error = resource_input(&args, directory.path())
        .unwrap_err()
        .to_string();
    assert_eq!(error, "skill root is not a directory");
}

#[tokio::test]
async fn non_loopback_http_is_rejected_before_binding() {
    let directory = tempfile::tempdir().unwrap();
    let server = peri_mcp_workspace::WorkspaceMcpServer::new(
        directory.path().to_string_lossy().into_owned(),
        None,
    );
    let error = super::serve_http(server, std::net::SocketAddr::from(([0, 0, 0, 0], 0)))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-loopback HTTP binding requires authentication"));
}
