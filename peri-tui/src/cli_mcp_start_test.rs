use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;

use super::{Args, display_path, resolve_options, run};

fn parse(args: &[&str]) -> Args {
    let mut argv = vec!["peri mcp-start"];
    argv.extend_from_slice(args);
    Args::try_parse_from(argv).unwrap()
}

fn write_mcp_file(dir: &Path, contents: &str) {
    fs::write(dir.join(".mcp.json"), contents).unwrap();
}

fn workspace_arg(dir: &Path) -> &str {
    dir.to_str().unwrap()
}

#[test]
fn instance_is_optional_but_transport_is_required() {
    let missing = Args::try_parse_from(["peri mcp-start"])
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
fn unknown_instance_is_rejected() {
    let error = run(&[OsString::from("sandbox"), OsString::from("--stdio")])
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown builtin MCP instance: sandbox"));
    for name in BUILTIN_RESERVED_INSTANCE_NAMES.iter() {
        assert!(error.contains(name), "error should list {name}: {error}");
    }
}

#[test]
fn http_defaults_to_loopback_8765() {
    let args = parse(&["web", "--http"]);
    assert_eq!(args.instance.as_deref(), Some("web"));
    assert!(args.http);
    assert!(!args.stdio);
    assert_eq!(args.host, "127.0.0.1");
    assert_eq!(args.port, 8765);
    assert_eq!(args.workspace, None);
}

#[test]
fn http_accepts_host_port_and_workspace() {
    let args = parse(&[
        "artifact",
        "--http",
        "--host",
        "0.0.0.0",
        "--port",
        "9000",
        "--workspace",
        "/tmp/project",
    ]);
    assert_eq!(args.host, "0.0.0.0");
    assert_eq!(args.port, 9000);
    assert_eq!(args.workspace, Some(PathBuf::from("/tmp/project")));
}

#[test]
fn allowed_host_parses_repeatedly() {
    let args = parse(&[
        "workspace",
        "--http",
        "--allowed-host",
        "sandbox.local",
        "--allowed-host",
        "sandbox.local:8765",
    ]);
    assert_eq!(args.allowed_host, ["sandbox.local", "sandbox.local:8765"]);
}

#[test]
fn allowed_host_requires_http() {
    let error = run(&[
        OsString::from("web"),
        OsString::from("--stdio"),
        OsString::from("--allowed-host"),
        OsString::from("sandbox.local"),
    ])
    .unwrap_err()
    .to_string();
    assert!(error.contains("--allowed-host requires --http"), "{error}");
}

#[test]
fn every_reserved_instance_parses() {
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        let args = parse(&[*name, "--stdio"]);
        assert_eq!(args.instance.as_deref(), Some(*name));
        assert!(args.stdio);
    }
}

#[test]
fn capability_flag_is_repeatable_and_comma_separated() {
    let args = parse(&[
        "--capability",
        "web,cron",
        "--capability",
        "artifact",
        "--stdio",
    ]);
    assert_eq!(args.capability, ["web,cron", "artifact"]);
    assert_eq!(args.instance, None);
}

#[test]
fn help_documents_capability_selection() {
    let help = Args::try_parse_from(["peri mcp-start", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("--capability"), "{help}");
    assert!(help.contains("all/none"), "{help}");
    assert!(help.contains("x-peri.capabilities"), "{help}");
    assert!(help.contains("Legacy compatibility"), "{help}");
}

#[test]
fn instance_and_capability_are_mutually_exclusive() {
    let error = resolve_options(parse(&["web", "--capability", "cron", "--stdio"]))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("either a legacy instance argument or --capability"),
        "{error}"
    );
}

#[test]
fn legacy_instance_does_not_read_mcp_file() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_file(dir.path(), "{not json");
    let options = resolve_options(parse(&[
        "web",
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap();
    assert_eq!(options.instance, "web");
}

#[test]
fn capability_mode_reads_extension_from_mcp_file() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_file(
        dir.path(),
        r#"{"mcpServers":{"peri":{"x-peri":{"capabilities":["cron"]}}}}"#,
    );
    let options = resolve_options(parse(&[
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap();
    assert_eq!(options.instance, "cron");
}

#[test]
fn cli_capability_overrides_mcp_file() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_file(
        dir.path(),
        r#"{"mcpServers":{"peri":{"x-peri":{"capabilities":["web"]}}}}"#,
    );
    let options = resolve_options(parse(&[
        "--capability",
        "cron",
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap();
    assert_eq!(options.instance, "cron");
}

#[test]
fn legacy_six_entries_map_to_caps_in_mcp_file() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_file(
        dir.path(),
        r#"{"mcpServers":{
            "workspace":{"disabled":true},
            "web":{"disabled":true},
            "artifact":{"disabled":true},
            "cron":{"disabled":true},
            "monitor":{},
            "workflow":{"disabled":true}
        }}"#,
    );
    let options = resolve_options(parse(&[
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap();
    assert_eq!(options.instance, "monitor");
}

#[test]
fn no_selection_defaults_to_all_and_fails_until_router_lands() {
    let dir = tempfile::tempdir().unwrap();
    let error = resolve_options(parse(&[
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("router is not implemented"), "{error}");
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        assert!(error.contains(name), "error should list {name}: {error}");
    }
}

#[test]
fn unknown_capability_is_a_load_error() {
    let error = run(&[
        OsString::from("--capability"),
        OsString::from("sandbox"),
        OsString::from("--stdio"),
    ])
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("unknown builtin MCP capability: `sandbox`"),
        "{error}"
    );
}

#[test]
fn duplicate_capability_is_a_load_error() {
    let error = run(&[
        OsString::from("--capability"),
        OsString::from("web,web"),
        OsString::from("--stdio"),
    ])
    .unwrap_err()
    .to_string();
    assert!(error.contains("duplicate capability `web`"), "{error}");
}

#[test]
fn all_is_parsed_but_rejected_until_router_lands() {
    let error = run(&[
        OsString::from("--capability"),
        OsString::from("all"),
        OsString::from("--stdio"),
    ])
    .unwrap_err()
    .to_string();
    assert!(error.contains("router is not implemented"), "{error}");
}

#[test]
fn none_is_an_empty_selection_error() {
    let error = run(&[
        OsString::from("--capability"),
        OsString::from("none"),
        OsString::from("--stdio"),
    ])
    .unwrap_err()
    .to_string();
    assert!(error.contains("is empty"), "{error}");
    assert!(error.contains("command line"), "{error}");
}

#[test]
fn invalid_mcp_file_is_reported_with_path() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_file(dir.path(), "{not json");
    let error = resolve_options(parse(&[
        "--capability",
        "web",
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("invalid JSON in MCP config"), "{error}");
    // The workspace is canonicalized, so compare against the canonical file and
    // require the friendly rendering that hides the Windows `\\?\` prefix.
    let expected = fs::canonicalize(dir.path().join(".mcp.json")).unwrap();
    assert!(error.contains(&display_path(&expected)), "{error}");
    assert!(error.contains(".mcp.json"), "{error}");
    assert!(!error.contains(r"\\?\"), "{error}");
}

#[test]
fn display_path_strips_windows_verbatim_prefix() {
    assert_eq!(
        display_path(Path::new(r"\\?\C:\workspace\.mcp.json")),
        r"C:\workspace\.mcp.json"
    );
    assert_eq!(
        display_path(Path::new(r"\\?\UNC\server\share\.mcp.json")),
        r"\\server\share\.mcp.json"
    );
    assert_eq!(
        display_path(Path::new(r"C:\workspace\.mcp.json")),
        r"C:\workspace\.mcp.json"
    );
}

#[test]
fn misplaced_capabilities_and_legacy_overrides_are_typed_errors() {
    let dir = tempfile::tempdir().unwrap();
    write_mcp_file(dir.path(), r#"{"capabilities":["web"]}"#);
    let error = resolve_options(parse(&[
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("not a Peri capability selector"),
        "{message}"
    );

    write_mcp_file(dir.path(), r#"{"mcpServers":{"web":{"command":"npx"}}}"#);
    let error = resolve_options(parse(&[
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("must not declare command/url"),
        "{message}"
    );

    write_mcp_file(
        dir.path(),
        r#"{"meta_harness":{"SandboxMiddleware":false}}"#,
    );
    let error = resolve_options(parse(&[
        "--stdio",
        "--workspace",
        workspace_arg(dir.path()),
    ]))
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("unknown legacy meta_harness policy key"),
        "{message}"
    );
}
