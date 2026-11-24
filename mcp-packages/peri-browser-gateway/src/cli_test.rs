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
        "peri-browser update",
        "-V|--version",
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
fn update_takes_no_arguments() {
    assert_eq!(parse(&args(&["update"])), Ok(Command::Update));
    for extra in ["--force", "kitesurf"] {
        assert_eq!(
            parse(&args(&["update", extra])),
            Err(CliError::UnexpectedUpdateArgument(extra.to_owned())),
            "argv={extra:?}"
        );
    }
    assert_eq!(parse(&args(&["update", "--help"])), Ok(Command::Help));
}

#[test]
fn help_describes_update_as_rerunning_the_install_script() {
    assert!(
        USAGE.contains("重新运行官方安装脚本"),
        "help 未说明 update 复用安装脚本：{USAGE}"
    );
    assert!(
        USAGE.contains("默认同时处理 kitesurf"),
        "help 未说明 update 默认处理 kitesurf：{USAGE}"
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
