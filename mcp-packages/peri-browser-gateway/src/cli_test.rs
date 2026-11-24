use super::*;
use crate::config::{Config, ConfigError};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn bare_and_serve_invocations_keep_the_existing_flag_parser() {
    assert_eq!(parse(&[]), Ok(Command::Serve(Vec::new())));
    let flags = args(&[
        "--bind",
        "127.0.0.1:0",
        "--cdp-url",
        "http://127.0.0.1:18971",
    ]);
    assert_eq!(parse(&flags), Ok(Command::Serve(flags.clone())));
    let served = args(&["serve", "--bind", "127.0.0.1:0"]);
    assert_eq!(
        parse(&served),
        Ok(Command::Serve(args(&["--bind", "127.0.0.1:0"])))
    );
}

#[test]
fn help_covers_every_entry_point() {
    assert_eq!(parse(&args(&["-h"])), Ok(Command::Help));
    assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
    assert_eq!(parse(&args(&["serve", "--help"])), Ok(Command::Help));
    for needle in [
        "serve",
        "install kitesurf",
        "start [OPTIONS]",
        "start kitesurf",
        "stop [--bind IP:PORT]",
        "update [kitesurf]",
        "-V|--version",
        "--view-token",
        "PERI_BROWSER_VIEW_TOKEN",
    ] {
        assert!(USAGE.contains(needle), "help 缺少 {needle}");
    }
}

#[test]
fn help_states_that_the_secret_flag_wins_over_the_environment() {
    assert!(
        USAGE.contains("CLI 参数优先于 PERI_BROWSER_GATEWAY_SECRET"),
        "--secret 优先级说反了：{USAGE}"
    );
    assert!(
        !USAGE.contains("优先用 PERI_BROWSER_GATEWAY_SECRET"),
        "--secret 仍标注为环境变量优先：{USAGE}"
    );
}

#[test]
fn version_flag_reports_the_build_version() {
    assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
    assert_eq!(parse(&args(&["-V"])), Ok(Command::Version));
    assert!(!crate::version().is_empty());
}

#[test]
fn install_requires_the_kitesurf_target() {
    assert_eq!(
        parse(&args(&["install"])),
        Err(CliError::MissingInstallTarget)
    );
    assert_eq!(
        parse(&args(&["install", "chrome"])),
        Err(CliError::UnknownInstallTarget("chrome".to_owned()))
    );
    assert_eq!(
        parse(&args(&["install", "kitesurf"])),
        Ok(Command::InstallKitesurf)
    );
    assert_eq!(
        parse(&args(&["install", "kitesurf", "--force"])),
        Err(CliError::UnknownOption("--force".to_owned()))
    );
}

#[test]
fn update_takes_an_optional_kitesurf_target() {
    assert_eq!(parse(&args(&["update"])), Ok(Command::Update));
    assert_eq!(
        parse(&args(&["update", "kitesurf"])),
        Ok(Command::UpdateKitesurf)
    );
    assert_eq!(
        parse(&args(&["update", "chrome"])),
        Err(CliError::UnknownUpdateTarget("chrome".to_owned()))
    );
    assert_eq!(
        parse(&args(&["update", "kitesurf", "extra"])),
        Err(CliError::UnexpectedUpdateArgument("extra".to_owned()))
    );
    assert_eq!(
        parse(&args(&["update", "kitesurf", "--force"])),
        Err(CliError::UnexpectedUpdateArgument("--force".to_owned()))
    );
    assert_eq!(parse(&args(&["update", "--help"])), Ok(Command::Help));
}

#[test]
fn unknown_update_targets_point_at_the_supported_target_and_the_gateway() {
    let message = CliError::UnknownUpdateTarget("chrome".to_owned()).to_string();
    assert!(
        message.contains("supported: kitesurf"),
        "错误缺少支持的 target：{message}"
    );
    assert!(
        message.contains("without a target"),
        "错误未提示无目标更新网关：{message}"
    );
}

#[test]
fn help_separates_bare_update_from_update_kitesurf() {
    assert!(
        USAGE.contains("update [kitesurf]"),
        "help 未列出 update 的可选目标：{USAGE}"
    );
    assert!(
        USAGE.contains("重新运行 browser 安装脚本"),
        "help 未说明裸 update 复用安装脚本：{USAGE}"
    );
    assert!(
        USAGE.contains("引擎不会升级"),
        "help 未说明裸 update 不升级引擎：{USAGE}"
    );
    assert!(
        USAGE.contains("无条件重跑 kitesurf 官方安装脚本"),
        "help 未说明 update kitesurf 强制重跑安装器：{USAGE}"
    );
    assert!(
        USAGE.contains("不会停止正在运行的进程"),
        "help 未说明安装器不停止运行中的进程：{USAGE}"
    );
}

#[test]
fn start_kitesurf_uses_the_recommended_engine_and_gateway_defaults() {
    assert_eq!(
        parse(&args(&["start", "kitesurf"])),
        Ok(Command::StartKitesurf(StartOptions {
            port: 18971,
            bind: "127.0.0.1:7391".parse().unwrap(),
        }))
    );
}

#[test]
fn start_without_a_target_runs_the_gateway_in_the_background() {
    assert_eq!(
        parse(&args(&["start"])),
        Ok(Command::StartGateway(Vec::new()))
    );
    let flags = args(&[
        "--bind",
        "127.0.0.1:18793",
        "--cdp-url",
        "http://127.0.0.1:18973",
    ]);
    let mut argv = vec!["start".to_owned()];
    argv.extend(flags.clone());
    assert_eq!(parse(&argv), Ok(Command::StartGateway(flags)));
}

#[test]
fn stop_uses_the_gateway_bind_and_rejects_other_flags() {
    assert_eq!(
        parse(&args(&["stop"])),
        Ok(Command::Stop(StopOptions {
            bind: "127.0.0.1:7391".parse().unwrap(),
        }))
    );
    assert_eq!(
        parse(&args(&["stop", "--bind", "[::1]:18793"])),
        Ok(Command::Stop(StopOptions {
            bind: "[::1]:18793".parse().unwrap(),
        }))
    );
    assert_eq!(
        parse(&args(&["stop", "--bind=127.0.0.1:18794"])),
        Ok(Command::Stop(StopOptions {
            bind: "127.0.0.1:18794".parse().unwrap(),
        }))
    );
    for (argv, expected) in [
        (vec!["stop", "--bind"], CliError::MissingBind),
        (
            vec!["stop", "--bind", "0.0.0.0:7391"],
            CliError::InvalidBind,
        ),
        (vec!["stop", "--bind", "127.0.0.1:0"], CliError::InvalidBind),
        (
            vec!["stop", "--listen", "127.0.0.1:7391"],
            CliError::UnknownOption("--listen".to_owned()),
        ),
    ] {
        assert_eq!(parse(&args(&argv)), Err(expected), "argv={argv:?}");
    }
}

#[test]
fn start_flags_override_defaults_in_both_spellings() {
    assert_eq!(
        parse(&args(&[
            "start",
            "kitesurf",
            "--port",
            "18973",
            "--bind",
            "[::1]:18793",
        ])),
        Ok(Command::StartKitesurf(StartOptions {
            port: 18973,
            bind: "[::1]:18793".parse().unwrap(),
        }))
    );
    assert_eq!(
        parse(&args(&[
            "start",
            "kitesurf",
            "--port=18974",
            "--bind=127.0.0.1:18794"
        ])),
        Ok(Command::StartKitesurf(StartOptions {
            port: 18974,
            bind: "127.0.0.1:18794".parse().unwrap(),
        }))
    );
}

#[test]
fn start_rejects_invalid_arguments_before_spawning_anything() {
    let cases = [
        (
            vec!["start", "chrome"],
            CliError::UnknownStartTarget("chrome".to_owned()),
        ),
        (vec!["start", "kitesurf", "--port"], CliError::MissingPort),
        (
            vec!["start", "kitesurf", "--port", "0"],
            CliError::InvalidPort,
        ),
        (
            vec!["start", "kitesurf", "--port", "65536"],
            CliError::InvalidPort,
        ),
        (vec!["start", "kitesurf", "--bind"], CliError::MissingBind),
        (
            vec!["start", "kitesurf", "--bind", "0.0.0.0:7391"],
            CliError::InvalidBind,
        ),
        (
            vec!["start", "kitesurf", "--bind", "127.0.0.1:0"],
            CliError::InvalidBind,
        ),
        (
            vec!["start", "kitesurf", "--listen", "127.0.0.1:7391"],
            CliError::UnknownOption("--listen".to_owned()),
        ),
    ];
    for (argv, expected) in cases {
        assert_eq!(parse(&args(&argv)), Err(expected), "argv={argv:?}");
    }
}

#[test]
fn unknown_subcommand_falls_through_to_serve_and_is_rejected_there() {
    let unknown = args(&["frobnicate"]);
    assert_eq!(parse(&unknown), Ok(Command::Serve(unknown.clone())));
    assert!(matches!(
        Config::parse_from(unknown, |_| None),
        Err(ConfigError::UnknownFlag)
    ));
}
