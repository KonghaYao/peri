use super::*;
use serial_test::serial;

#[test]
#[serial]
fn test_config_from_args_uses_port_env_when_set() {
    unsafe {
        std::env::set_var("PORT", "9090");
    }
    let cfg = Config::parse_from(["peri-web-pty"]);
    assert_eq!(cfg.port, 9090);
    unsafe {
        std::env::remove_var("PORT");
    }
}

#[test]
#[serial]
fn test_config_from_args_defaults_port_when_unset() {
    unsafe {
        std::env::remove_var("PORT");
    }
    let cfg = Config::parse_from(["peri-web-pty"]);
    assert_eq!(cfg.port, 0);
}

#[test]
#[serial]
fn test_config_from_args_uses_cwd_when_set() {
    unsafe {
        std::env::set_var("CWD", "/tmp");
    }
    let cfg = Config::parse_from(["peri-web-pty"]);
    assert_eq!(cfg.cwd.as_deref(), Some("/tmp"));
    unsafe {
        std::env::remove_var("CWD");
    }
}

#[test]
#[serial]
fn test_config_from_args_uses_cmd_when_set() {
    unsafe {
        std::env::set_var("CMD", "npm run dev");
    }
    let cfg = Config::parse_from(["peri-web-pty"]);
    assert_eq!(cfg.initial_cmd.as_deref(), Some("npm run dev"));
    unsafe {
        std::env::remove_var("CMD");
    }
}

#[test]
#[serial]
fn test_config_from_args_defaults_when_all_unset() {
    unsafe {
        std::env::remove_var("PORT");
        std::env::remove_var("SHELL");
        std::env::remove_var("CWD");
        std::env::remove_var("CMD");
    }
    let cfg = Config::parse_from(["peri-web-pty"]);
    assert_eq!(cfg.port, 0);
    assert!(cfg.shell.is_none());
    assert!(cfg.cwd.is_none());
    assert!(cfg.initial_cmd.is_none());
}
