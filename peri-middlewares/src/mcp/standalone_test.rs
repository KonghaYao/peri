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
