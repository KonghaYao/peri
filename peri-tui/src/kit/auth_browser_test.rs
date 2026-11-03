use super::*;

#[test]
fn macos_uses_open_without_shell() {
    let (program, args) = browser_command_for("macos", "https://example.test/auth?state=secret");
    assert_eq!(program, "open");
    assert_eq!(args, vec!["https://example.test/auth?state=secret"]);
}

#[test]
fn linux_uses_xdg_open_without_shell() {
    let (program, args) = browser_command_for("linux", "https://example.test/auth");
    assert_eq!(program, "xdg-open");
    assert_eq!(args, vec!["https://example.test/auth"]);
}

#[test]
fn windows_uses_cmd_start_with_empty_title() {
    let (program, args) = browser_command_for("windows", "https://example.test/auth");
    assert_eq!(program, "cmd");
    assert_eq!(
        args,
        vec!["/C", "start", "", "https://example.test/auth"],
        "cmd start needs an empty title argument before the URL"
    );
}

#[test]
fn unknown_platform_falls_back_to_xdg_open() {
    let (program, _) = browser_command_for("freebsd", "https://example.test/auth");
    assert_eq!(program, "xdg-open");
}
