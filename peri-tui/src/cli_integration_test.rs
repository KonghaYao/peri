//! CLI 参数解析集成测试

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "peri")]
struct TestCli {
    #[arg(short = 'a', long = "approve")]
    approve: bool,
    #[arg(short = 'p', long = "print")]
    print: Option<Option<String>>,
    #[arg(long = "output-format", visible_alias = "outputFormat")]
    output_format: Option<String>,
    #[arg(long = "max-turns", visible_alias = "maxTurns")]
    max_turns: Option<u32>,
    #[arg(long = "bare")]
    bare: bool,
    #[arg(long = "permission-mode", visible_alias = "permissionMode")]
    permission_mode: Option<String>,
    #[arg(long = "dangerously-skip-permissions")]
    skip_permissions: bool,
    #[arg(long = "model")]
    model: Option<String>,
    #[arg(long = "effort")]
    effort: Option<String>,
    #[arg(short = 'c', long = "continue")]
    cont: bool,
    #[arg(short = 'r', long = "resume")]
    resume: Option<Option<String>>,
    #[arg(long = "session-id", visible_alias = "sessionId")]
    session_id: Option<String>,
    #[arg(short = 'n', long = "name")]
    session_name: Option<String>,
    #[arg(long = "no-session-persistence")]
    no_session_persistence: bool,
    #[arg(long = "allowedTools", visible_alias = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
    #[arg(long = "disallowedTools", visible_alias = "disallowed-tools")]
    disallowed_tools: Option<Vec<String>>,
    #[arg(long = "settings")]
    settings: Option<String>,
    #[arg(long = "config-file", visible_alias = "configFile")]
    config_file: Option<PathBuf>,
    #[arg(long = "db-path", visible_alias = "dbPath")]
    db_path: Option<PathBuf>,
}

#[test]
fn test_print_with_prompt() {
    let cli = TestCli::try_parse_from(["peri", "-p", "hello world"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.print, Some(Some("hello world".to_string())));
}

#[test]
fn test_print_without_prompt() {
    let cli = TestCli::try_parse_from(["peri", "-p"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.print, Some(None));
}

#[test]
fn test_output_format_aliases() {
    let cli = TestCli::try_parse_from(["peri", "--output-format", "json"]);
    assert!(cli.is_ok());
    let cli = TestCli::try_parse_from(["peri", "--outputFormat", "json"]);
    assert!(cli.is_ok());
}

#[test]
fn test_permission_mode_aliases() {
    let cli = TestCli::try_parse_from(["peri", "--permission-mode", "bypass"]);
    assert!(cli.is_ok());
    let cli = TestCli::try_parse_from(["peri", "--permissionMode", "bypass"]);
    assert!(cli.is_ok());
}

#[test]
fn test_allowed_tools() {
    let cli = TestCli::try_parse_from(["peri", "--allowedTools", "Bash", "--allowedTools", "Edit"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(
        cli.allowed_tools,
        Some(vec!["Bash".to_string(), "Edit".to_string()])
    );
}

#[test]
fn test_resume_with_value() {
    let cli = TestCli::try_parse_from(["peri", "-r", "abc-123"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.resume, Some(Some("abc-123".to_string())));
}

#[test]
fn test_resume_without_value() {
    let cli = TestCli::try_parse_from(["peri", "-r"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.resume, Some(None));
}

#[test]
fn test_meta_session_nested_grammar() {
    let cli = Cli::try_parse_from([
        "peri",
        "--db-path",
        "/tmp/threads.db",
        "meta",
        "session",
        "550e8400-e29b-41d4-a716-446655440000",
        "--json",
    ])
    .unwrap();
    assert_eq!(cli.db_path, Some(PathBuf::from("/tmp/threads.db")));
    let Some(Commands::Meta {
        action: MetaAction::Session { session_id, json },
    }) = cli.command
    else {
        panic!("expected meta session command");
    };
    assert_eq!(session_id, "550e8400-e29b-41d4-a716-446655440000");
    assert!(json);
}

#[test]
fn test_meta_session_requires_explicit_id() {
    assert!(Cli::try_parse_from(["peri", "meta", "session"]).is_err());
}

#[test]
fn test_meta_rejects_unknown_expansions() {
    assert!(
        Cli::try_parse_from([
            "peri",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
            "--include",
            "messages",
        ])
        .is_err()
    );
    assert!(Cli::try_parse_from(["peri", "meta", "list"]).is_err());
}

#[test]
fn test_print_conflicts_with_meta() {
    let cli = Cli::try_parse_from([
        "peri",
        "--print=prompt",
        "meta",
        "session",
        "550e8400-e29b-41d4-a716-446655440000",
    ])
    .unwrap();
    assert!(validate_cli(&cli).is_err());
}

#[test]
fn test_meta_rejects_every_unrelated_top_level_option() {
    let cases: &[&[&str]] = &[
        &["--approve"],
        &["--print=prompt"],
        &["--output-format", "json"],
        &["--max-turns", "1"],
        &["--bare"],
        &["--permission-mode", "default"],
        &["--dangerously-skip-permissions"],
        &["--model", "sonnet"],
        &["--effort", "high"],
        &["--continue"],
        &["--resume=session"],
        &["--session-id", "550e8400-e29b-41d4-a716-446655440000"],
        &["--name", "name"],
        &["--no-session-persistence"],
        &["--allowedTools", "Bash"],
        &["--disallowedTools", "Edit"],
        &["--settings", "{}"],
        &["--config-file", "/tmp/settings.json"],
    ];

    for unrelated in cases {
        let mut args = vec!["peri"];
        args.extend_from_slice(unrelated);
        args.extend_from_slice(&["meta", "session", "550e8400-e29b-41d4-a716-446655440000"]);
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(
            validate_cli(&cli).is_err(),
            "Meta accepted unrelated option: {unrelated:?}"
        );
    }
}

#[test]
fn test_argv_meta_detection_respects_top_level_subcommand_position() {
    let ordinary_cases: &[&[&str]] = &[
        &["-p", "meta", "--help"],
        &["--print", "meta", "--help"],
        &["-r", "meta", "--help"],
        &["--resume", "meta", "--help"],
        &["--model", "meta"],
        &["--model", "meta", "--definitely-invalid"],
        &["--model", "meta", "--settings"],
        &["--model", "meta", "--effort"],
        &[
            "--db-path",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &[
            "--definitely-invalid",
            "value",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &[
            "--help",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &["-p", "meta", "value"],
        &["--resume", "meta", "value"],
    ];
    for ordinary in ordinary_cases {
        let mut args = vec![OsString::from("peri")];
        args.extend(ordinary.iter().map(OsString::from));
        assert!(
            !argv_requests_meta(&args),
            "ordinary argv was classified as Meta: {ordinary:?}"
        );
    }

    let meta_cases: &[&[&str]] = &[
        &[
            "--db-path",
            "threads.db",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &["meta", "session"],
        &["meta", "list"],
        &[
            "-p",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &[
            "--print",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &[
            "-r",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &[
            "--resume",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
        &[
            "--definitely-invalid",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
            "--json",
        ],
        &[
            "--db-path",
            "--definitely-invalid",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
            "--json",
        ],
        &[
            "--approve",
            "--unknown",
            "meta",
            "session",
            "550e8400-e29b-41d4-a716-446655440000",
        ],
    ];
    for meta in meta_cases {
        let mut args = vec![OsString::from("peri")];
        args.extend(meta.iter().map(OsString::from));
        assert!(
            argv_requests_meta(&args),
            "Meta argv was not classified as Meta: {meta:?}"
        );
    }

    let other_command = [
        OsString::from("peri"),
        OsString::from("acp"),
        OsString::from("--model"),
        OsString::from("meta"),
    ];
    assert!(!argv_requests_meta(&other_command));
}

#[test]
fn test_combined_model_effort() {
    let cli = TestCli::try_parse_from(["peri", "--model", "sonnet", "--effort", "high"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.model, Some("sonnet".to_string()));
    assert_eq!(cli.effort, Some("high".to_string()));
}

#[test]
fn test_disallowed_tools_alias() {
    let cli = TestCli::try_parse_from(["peri", "--disallowed-tools", "WebFetch"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.disallowed_tools, Some(vec!["WebFetch".to_string()]));
}

// ─── Sync 子命令（r2-encrypted-transfer Slice 3）───────────────────────────

use super::*;

#[test]
fn test_sync_server_defaults_to_https() {
    // 旧 wss:// 默认值废止；默认必须是 https:// 端点。
    let cli = Cli::try_parse_from(["peri", "sync", "send", "--to", "AAAABBBBCCCCDDDD"]).unwrap();
    let Commands::Sync { server, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(
        server.starts_with("https://"),
        "server 默认必须是 https，实际: {server}"
    );
}

#[test]
fn test_sync_server_explicit_override() {
    let cli = Cli::try_parse_from(["peri", "sync", "receive", "--server", "https://example.com"])
        .unwrap();
    let Commands::Sync { server, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert_eq!(server, "https://example.com");
}

#[test]
fn test_sync_keystore_path_flag() {
    let cli = Cli::try_parse_from([
        "peri",
        "sync",
        "send",
        "--to",
        "AAAABBBBCCCCDDDD",
        "--keystore-path",
        "/tmp/ks",
    ])
    .unwrap();
    let Commands::Sync { keystore_path, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert_eq!(keystore_path.as_deref(), Some("/tmp/ks"));
}

#[test]
fn test_sync_device_init_parse() {
    let cli = Cli::try_parse_from(["peri", "sync", "device", "init", "--name", "laptop"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    match action {
        SyncAction::Device { action } => match action {
            peri_tui::sync::device_cli::DeviceAction::Init { name } => {
                assert_eq!(name.as_deref(), Some("laptop"));
            }
            _ => panic!("expected device init"),
        },
        _ => panic!("expected device subcommand"),
    }
}

#[test]
fn test_sync_device_show_add_list_remove_parse() {
    let cli = Cli::try_parse_from(["peri", "sync", "device", "show"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(
        action,
        SyncAction::Device {
            action: peri_tui::sync::device_cli::DeviceAction::Show
        }
    ));

    let cli = Cli::try_parse_from([
        "peri",
        "sync",
        "device",
        "add",
        "peri://device/AAAABBBBCCCCDDDD?ed=x&x=y&n=phone",
    ])
    .unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(
        action,
        SyncAction::Device {
            action: peri_tui::sync::device_cli::DeviceAction::Add { .. }
        }
    ));

    let cli = Cli::try_parse_from(["peri", "sync", "device", "list"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(
        action,
        SyncAction::Device {
            action: peri_tui::sync::device_cli::DeviceAction::List
        }
    ));

    let cli =
        Cli::try_parse_from(["peri", "sync", "device", "remove", "AAAABBBBCCCCDDDD"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(
        action,
        SyncAction::Device {
            action: peri_tui::sync::device_cli::DeviceAction::Remove { .. }
        }
    ));
}

#[test]
fn test_sync_send_requires_to() {
    // send 必须带 --to；缺失时解析失败。
    assert!(Cli::try_parse_from(["peri", "sync", "send"]).is_err());
    let cli = Cli::try_parse_from(["peri", "sync", "send", "--to", "AAAABBBBCCCCDDDD"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    match action {
        SyncAction::Send { to } => assert_eq!(to, "AAAABBBBCCCCDDDD"),
        _ => panic!("expected send"),
    }
}

#[test]
fn test_sync_receive_parse() {
    let cli = Cli::try_parse_from(["peri", "sync", "receive"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(action, SyncAction::Receive));
}

#[test]
fn test_sync_legacy_ws_actions_still_parse() {
    // 旧 WebSocket sender/receiver 保持可解析（Slice 4 移除），行为不变。
    let cli = Cli::try_parse_from(["peri", "sync", "sender"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(action, SyncAction::Sender));
    let cli = Cli::try_parse_from(["peri", "sync", "receiver"]).unwrap();
    let Commands::Sync { action, .. } = cli.command.unwrap() else {
        panic!("expected sync command");
    };
    assert!(matches!(action, SyncAction::Receiver));
}

// ─── 全局配置/数据库路径参数（Slice C1）─────────────────────────────────────

#[test]
fn test_config_file_flag_parses() {
    let cli = TestCli::try_parse_from(["peri", "--config-file", "/tmp/cfg.json"]).unwrap();
    assert_eq!(cli.config_file, Some(PathBuf::from("/tmp/cfg.json")));
}

#[test]
fn test_config_file_alias_parses() {
    let cli = TestCli::try_parse_from(["peri", "--configFile", "/tmp/cfg.json"]).unwrap();
    assert_eq!(cli.config_file, Some(PathBuf::from("/tmp/cfg.json")));
}

#[test]
fn test_config_file_equals_form_parses() {
    let cli = TestCli::try_parse_from(["peri", "--config-file=/tmp/cfg.json"]).unwrap();
    assert_eq!(cli.config_file, Some(PathBuf::from("/tmp/cfg.json")));
}

#[test]
fn test_config_file_camel_equals_form_parses() {
    let cli = TestCli::try_parse_from(["peri", "--configFile=/tmp/cfg.json"]).unwrap();
    assert_eq!(cli.config_file, Some(PathBuf::from("/tmp/cfg.json")));
}

#[test]
fn test_db_path_flag_parses() {
    let cli = TestCli::try_parse_from(["peri", "--db-path", "/tmp/threads.db"]).unwrap();
    assert_eq!(cli.db_path, Some(PathBuf::from("/tmp/threads.db")));
}

#[test]
fn test_db_path_alias_parses() {
    let cli = TestCli::try_parse_from(["peri", "--dbPath", "/tmp/threads.db"]).unwrap();
    assert_eq!(cli.db_path, Some(PathBuf::from("/tmp/threads.db")));
}

#[test]
fn test_config_file_missing_value_errors() {
    assert!(TestCli::try_parse_from(["peri", "--config-file"]).is_err());
}

#[test]
fn test_db_path_missing_value_errors() {
    assert!(TestCli::try_parse_from(["peri", "--db-path"]).is_err());
}

#[test]
fn test_config_file_and_db_path_combined() {
    let cli = TestCli::try_parse_from([
        "peri",
        "--config-file",
        "/tmp/cfg.json",
        "--db-path",
        "/tmp/threads.db",
    ])
    .unwrap();
    assert_eq!(cli.config_file, Some(PathBuf::from("/tmp/cfg.json")));
    assert_eq!(cli.db_path, Some(PathBuf::from("/tmp/threads.db")));
}

#[test]
fn test_real_cli_parses_config_and_db_flags() {
    // 直测真实 Cli（防 TestCli 镜像漂移）
    let cli = Cli::try_parse_from([
        "peri",
        "--config-file=/tmp/cfg.json",
        "--db-path",
        "/tmp/threads.db",
    ])
    .unwrap();
    assert_eq!(cli.config_file, Some(PathBuf::from("/tmp/cfg.json")));
    assert_eq!(cli.db_path, Some(PathBuf::from("/tmp/threads.db")));
}
