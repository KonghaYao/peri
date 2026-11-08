use std::path::PathBuf;

use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;

use super::super::builtin::standalone::StandaloneFactory;

#[test]
fn unknown_instance_is_rejected() {
    let error = match StandaloneFactory::new("sandbox", PathBuf::from(".")) {
        Ok(_) => panic!("unknown instance must be rejected"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("unknown builtin MCP instance: sandbox"));
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        assert!(error.contains(name), "error should list {name}: {error}");
    }
}

#[tokio::test]
async fn every_builtin_instance_builds_a_handler() {
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        let factory = StandaloneFactory::new(name, PathBuf::from("."))
            .unwrap_or_else(|error| panic!("{name} factory: {error}"));
        factory
            .create()
            .unwrap_or_else(|error| panic!("{name} handler: {error}"));
    }
}

#[tokio::test]
async fn builtin_instances_without_workspace_accept_clean_transport_close() {
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        if *name == "workspace" {
            continue;
        }
        let factory = StandaloneFactory::new(name, PathBuf::from(".")).unwrap();
        assert!(factory.workspace_handle().is_none());
        super::cleanup_workspace(factory.workspace_handle())
            .await
            .unwrap_or_else(|error| panic!("{name} shutdown is not workspace cleanup: {error}"));
    }
}

#[test]
fn real_workspace_requires_positive_cleanup_evidence() {
    use peri_acp_types::tasks::TaskShutdownReport;

    assert!(super::require_workspace_cleanup(Some(TaskShutdownReport::Complete)).is_ok());
    for report in [None, Some(TaskShutdownReport::Incomplete)] {
        let error = super::require_workspace_cleanup(report).unwrap_err();
        assert!(error
            .to_string()
            .contains("Workspace shell task cleanup incomplete"));
    }
}
