use std::collections::HashMap;
use std::path::Path;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |key| map.get(key).cloned()
}

fn install(root: &Path, version: Option<&str>) {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin dir");
    std::fs::write(bin.join("kitesurf"), b"#!/bin/sh\n").expect("write kitesurf");
    if let Some(version) = version {
        std::fs::write(root.join("current-version.txt"), format!("{version}\n"))
            .expect("write current-version.txt");
    }
}

#[test]
fn http_and_ws_loopback_endpoints_share_one_canonical_form() {
    let cases = [
        (
            "http://127.0.0.1:18971",
            "ws://127.0.0.1:18971/devtools/browser",
        ),
        (
            "ws://127.0.0.1:18971/devtools/browser",
            "ws://127.0.0.1:18971/devtools/browser",
        ),
        (
            "http://localhost:18971/some/path?owner=someone",
            "ws://localhost:18971/devtools/browser",
        ),
        (
            "  ws://127.0.0.1:9300/devtools/browser?owner=x&token=y  ",
            "ws://127.0.0.1:9300/devtools/browser",
        ),
        // Every 127/8 address is loopback, not just 127.0.0.1.
        (
            "http://127.9.9.9:8080",
            "ws://127.9.9.9:8080/devtools/browser",
        ),
        ("http://[::1]:18971", "ws://[::1]:18971/devtools/browser"),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize_endpoint(input).unwrap(), expected, "{input}");
    }
}

#[test]
fn path_and_query_never_survive_normalization() {
    let canonical =
        normalize_endpoint("http://127.0.0.1:18971/devtools/page/abc?owner=me").unwrap();
    assert_eq!(canonical, "ws://127.0.0.1:18971/devtools/browser");
    assert!(!canonical.contains("owner"), "{canonical}");
    assert!(!canonical.contains("page"), "{canonical}");
}

#[test]
fn unsupported_or_ambiguous_urls_are_invalid_arguments() {
    for (input, reason) in [
        ("https://127.0.0.1:18971", "TLS"),
        ("wss://localhost:18971", "TLS"),
        ("http://127.0.0.1", "explicit port"),
        ("ws://localhost", "explicit port"),
        ("http://example.test:18971", "not loopback"),
        ("ws://10.0.0.1:18971", "not loopback"),
        ("http://0.0.0.0:18971", "not loopback"),
        ("ftp://127.0.0.1:18971", "expected http:// or ws://"),
        ("localhost:18971", "expected http:// or ws://"),
        ("", "must not be empty"),
        ("   ", "must not be empty"),
        ("not a url", "invalid argument"),
    ] {
        let error = normalize_endpoint(input).unwrap_err();
        assert_eq!(error.code(), "invalid_argument", "{input:?}");
        let rendered = error.to_string();
        assert!(
            rendered.contains(reason),
            "{input:?} should explain {reason:?}, got {rendered}"
        );
    }
}

#[test]
fn only_macos_arm64_and_linux_x64_are_supported_platforms() {
    assert!(platform_for("macos", "aarch64").supported);
    assert!(platform_for("linux", "x86_64").supported);
    for (os, arch) in [
        ("macos", "x86_64"),
        ("linux", "aarch64"),
        ("windows", "x86_64"),
        ("freebsd", "x86_64"),
    ] {
        let platform = platform_for(os, arch);
        assert!(!platform.supported, "{os}-{arch}");
        assert_eq!(platform.os, os);
        assert_eq!(platform.arch, arch);
    }

    let current = platform();
    assert_eq!(current.os, std::env::consts::OS);
    assert_eq!(current.arch, std::env::consts::ARCH);
    assert_eq!(
        current.supported,
        install_supported(std::env::consts::OS, std::env::consts::ARCH)
    );
    let json = current.to_json();
    assert_eq!(json["os"], current.os);
    assert_eq!(json["arch"], current.arch);
    assert_eq!(json["supported"], current.supported);
}

#[test]
fn the_recommended_endpoint_is_a_convention_for_port_18971() {
    assert_eq!(RECOMMENDED_PORT, 18971);
    assert_eq!(recommended_endpoint(), "http://127.0.0.1:18971");
    assert_eq!(
        recommended_canonical(),
        "ws://127.0.0.1:18971/devtools/browser"
    );
    assert_eq!(
        normalize_endpoint(&recommended_endpoint()).unwrap(),
        recommended_canonical()
    );
}

#[test]
fn install_detection_reads_the_install_root_and_its_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    install(dir.path(), Some("v0.5.4"));

    let found = detect_install(dir.path(), &env(&[("HOME", "/nonexistent")]));
    assert!(found.installed);
    assert_eq!(found.version.as_deref(), Some("v0.5.4"));
    assert_eq!(
        found.path.as_deref(),
        Some(dir.path().join("bin/kitesurf").as_path())
    );
    assert!(!found.on_path);

    let json = found.to_json();
    assert_eq!(json["installed"], true);
    assert_eq!(json["version"], "v0.5.4");
    assert_eq!(json["onPath"], false);
    assert_eq!(json["installCommand"], INSTALL_COMMAND);
    assert!(json.get("installInstruction").is_none());
}

#[test]
fn an_install_without_current_version_reports_no_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    install(dir.path(), None);

    let found = detect_install(dir.path(), &env(&[("HOME", "/nonexistent")]));
    assert!(found.installed);
    assert_eq!(found.version, None);
    assert!(found.path.is_some());
}

#[test]
fn the_default_home_install_is_found_when_the_override_is_empty() {
    let home = tempfile::tempdir().expect("temp home");
    let root = home.path().join(".kitesurf");
    install(&root, Some("v0.5.2"));
    let empty_override = tempfile::tempdir().expect("empty override");

    let found = detect_install(
        empty_override.path(),
        &env(&[("HOME", home.path().to_str().unwrap())]),
    );
    assert!(found.installed, "falls back to $HOME/.kitesurf");
    assert_eq!(found.version.as_deref(), Some("v0.5.2"));
    assert_eq!(
        found.path.as_deref(),
        Some(root.join("bin/kitesurf").as_path())
    );
}

#[test]
fn the_configured_install_root_wins_over_the_default_home() {
    let configured = tempfile::tempdir().expect("configured root");
    install(configured.path(), Some("v0.6.0"));
    let home = tempfile::tempdir().expect("temp home");
    install(&home.path().join(".kitesurf"), Some("v0.5.2"));

    let found = detect_install(
        configured.path(),
        &env(&[("HOME", home.path().to_str().unwrap())]),
    );
    assert_eq!(found.version.as_deref(), Some("v0.6.0"));
    assert_eq!(
        found.path.as_deref(),
        Some(configured.path().join("bin/kitesurf").as_path())
    );
}

#[test]
fn a_kitesurf_on_path_is_found_without_a_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    std::fs::write(bin.join("kitesurf"), b"#!/bin/sh\n").expect("write shim");

    let found = detect_install(
        &dir.path().join("missing"),
        &env(&[("HOME", "/nonexistent"), ("PATH", bin.to_str().unwrap())]),
    );
    assert!(found.installed);
    assert_eq!(found.version, None, "PATH alone has no install root");
    assert!(found.on_path);
    assert_eq!(found.path.as_deref(), Some(bin.join("kitesurf").as_path()));
}

#[test]
fn missing_installs_ask_the_user_before_installing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let found = detect_install(dir.path(), &env(&[("HOME", "/nonexistent")]));
    assert!(!found.installed);
    assert_eq!(found.version, None);
    assert_eq!(found.path, None);

    let json = found.to_json();
    assert_eq!(json["installed"], false);
    assert_eq!(json["installCommand"], INSTALL_COMMAND);
    assert_eq!(json["installInstruction"], INSTALL_INSTRUCTION);
    assert!(
        INSTALL_INSTRUCTION.contains("Ask the user"),
        "the instruction must gate the install on user consent"
    );
}

#[test]
fn next_steps_follow_the_onboarding_state_machine() {
    let serve = format!("kitesurf serve --port {RECOMMENDED_PORT}");
    let connect = format!("browser_connect(url: \"{}\")", recommended_endpoint());

    assert_eq!(
        next_steps(false, false, None, false),
        vec![INSTALL_COMMAND.to_owned(), serve.clone(), connect.clone()],
        "missing install: install, start, connect"
    );
    assert_eq!(
        next_steps(true, false, None, false),
        vec![serve.clone(), connect.clone()],
        "installed but not listening: start, connect"
    );
    assert_eq!(
        next_steps(true, true, None, false),
        vec![connect.clone()],
        "listening but not connected: connect"
    );
    assert_eq!(
        next_steps(
            true,
            true,
            Some("ws://127.0.0.1:18971/devtools/browser"),
            true
        ),
        Vec::<String>::new(),
        "connected and reachable: nothing to do"
    );
    assert_eq!(
        next_steps(
            true,
            false,
            Some("ws://127.0.0.1:18971/devtools/browser"),
            false
        ),
        vec![serve],
        "connected but unreachable: restart the engine on that port"
    );
    assert_eq!(
        next_steps(
            true,
            false,
            Some("ws://127.0.0.1:9300/devtools/browser"),
            false
        ),
        vec!["kitesurf serve --port 9300".to_owned()],
        "the restart hint keeps the connected port"
    );
}
