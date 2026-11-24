use super::*;

#[test]
fn an_existing_install_is_reported_without_running_the_script() {
    let root = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(root.path().join("bin")).expect("bin dir");
    std::fs::write(root.path().join("bin").join("kitesurf"), "").expect("binary");
    std::fs::write(root.path().join("current-version.txt"), "9.9.9\n").expect("version");
    let lookup =
        |key: &str| (key == "KITESURF_INSTALL_DIR").then(|| root.path().display().to_string());
    assert_eq!(install(&lookup).expect("install detection"), 0);
}

#[test]
fn install_without_a_home_or_override_fails_before_any_download() {
    let lookup = |_key: &str| None;
    let error = install(&lookup).expect_err("missing install root");
    assert!(error.to_string().contains("HOME is not set"), "{error:#}");
}

#[test]
fn update_without_a_home_or_override_fails_before_any_download() {
    let lookup = |_key: &str| None;
    let error = update_kitesurf(&lookup).expect_err("missing install root");
    assert!(error.to_string().contains("HOME is not set"), "{error:#}");
}

#[test]
fn the_probe_url_is_the_canonical_cdp_form() {
    assert_eq!(
        engine_probe_url(18971),
        "ws://127.0.0.1:18971/devtools/browser"
    );
    assert_eq!(engine_url(18971), "http://127.0.0.1:18971");
}

#[test]
fn engine_args_keep_the_port_and_enable_private_network_by_default() {
    assert_eq!(
        engine_args(18971, true),
        vec!["serve", "--port", "18971", "--allow-private-network"]
    );
    assert_eq!(engine_args(18971, false), vec!["serve", "--port", "18971"]);
}

#[test]
fn installer_dirs_are_unique_private_and_reserve_the_script_with_create_new() {
    let first = make_installer_dir().expect("first installer dir");
    let second = make_installer_dir().expect("second installer dir");
    assert_ne!(first, second, "installer dirs must not collide");
    assert!(first.starts_with(std::env::temp_dir()));

    for dir in [&first, &second] {
        let mode = std::fs::metadata(dir)
            .expect("dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} must be owner-only", dir.display());

        let script = create_installer_script(dir).expect("reserve script");
        assert_eq!(script.parent(), Some(dir.as_path()));
        let second_create = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&script)
            .expect_err("create_new must refuse the reserved path");
        assert_eq!(second_create.kind(), ErrorKind::AlreadyExists);
    }

    cleanup_installer_dir(&first);
    cleanup_installer_dir(&second);
    assert!(!first.exists(), "{} must be removed", first.display());
    assert!(!second.exists(), "{} must be removed", second.display());
    cleanup_installer_dir(&first);
}
