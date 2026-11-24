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
        "start kitesurf",
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
        (vec!["start"], CliError::MissingStartTarget),
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
