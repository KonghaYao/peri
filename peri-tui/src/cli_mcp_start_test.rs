use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;
use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;

use super::{Args, run};

#[test]
fn instance_is_required() {
    let error = run(&[]).unwrap_err().to_string();
    assert!(error.contains("<INSTANCE>"), "{error}");
}

#[test]
fn unknown_instance_is_rejected() {
    let error = run(&[OsString::from("sandbox"), OsString::from("--stdio")])
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown builtin MCP instance: sandbox"));
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        assert!(error.contains(name), "error should list {name}: {error}");
    }
}

#[test]
fn transport_is_required_and_exclusive() {
    let missing = Args::try_parse_from(["peri mcp-start", "workspace"])
        .unwrap_err()
        .to_string();
    assert!(missing.contains("--stdio"), "{missing}");
    assert!(missing.contains("--http"), "{missing}");
    let both = Args::try_parse_from(["peri mcp-start", "workspace", "--stdio", "--http"])
        .unwrap_err()
        .to_string();
    assert!(both.contains("cannot be used with"), "{both}");
}

#[test]
fn http_defaults_to_loopback_8765() {
    let args = Args::try_parse_from(["peri mcp-start", "web", "--http"]).unwrap();
    assert_eq!(args.instance, "web");
    assert!(args.http);
    assert!(!args.stdio);
    assert_eq!(args.host, "127.0.0.1");
    assert_eq!(args.port, 8765);
    assert_eq!(args.workspace, None);
}

#[test]
fn http_accepts_host_port_and_workspace() {
    let args = Args::try_parse_from([
        "peri mcp-start",
        "artifact",
        "--http",
        "--host",
        "0.0.0.0",
        "--port",
        "9000",
        "--workspace",
        "/tmp/project",
    ])
    .unwrap();
    assert_eq!(args.host, "0.0.0.0");
    assert_eq!(args.port, 9000);
    assert_eq!(args.workspace, Some(PathBuf::from("/tmp/project")));
}

#[test]
fn every_reserved_instance_parses() {
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        let args = Args::try_parse_from(["peri mcp-start", name, "--stdio"])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(args.instance, *name);
        assert!(args.stdio);
    }
}
