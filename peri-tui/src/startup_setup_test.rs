//! `--setup` / `PERI_ACP_SETUP` 入口与参数互斥的单元测试。

use clap::Parser;

use super::{Cli, Commands, setup_conflicts_with_settings_stdin, setup_requested, validate_cli};

#[test]
fn setup_requested_from_cli_flag() {
    assert!(setup_requested(true, None));
    assert!(
        setup_requested(true, Some("0")),
        "explicit --setup must win over an arbitrary environment value"
    );
}

#[test]
fn setup_requested_accepts_documented_env_values() {
    for value in ["1", "true", "on", "yes"] {
        assert!(
            setup_requested(false, Some(value)),
            "PERI_ACP_SETUP={value:?} must request setup"
        );
    }
}

#[test]
fn setup_requested_normalizes_whitespace_and_case() {
    for value in [" 1 ", "\ttrue\n", " ON ", "Yes"] {
        assert!(
            setup_requested(false, Some(value)),
            "PERI_ACP_SETUP={value:?} must be trimmed and compared case-insensitively"
        );
    }
}

#[test]
fn setup_requested_rejects_other_values() {
    for value in ["", " ", "0", "false", "off", "no", "2", "setup"] {
        assert!(
            !setup_requested(false, Some(value)),
            "PERI_ACP_SETUP={value:?} must not be guessed as enabled"
        );
    }
    assert!(!setup_requested(false, None));
}

#[test]
fn cli_parses_setup_flag_for_default_and_acp_commands() {
    let top_level =
        Cli::try_parse_from(["peri", "--setup"]).expect("peri --setup: top-level setup must parse");
    assert!(top_level.setup);
    assert!(top_level.command.is_none());

    let acp = Cli::try_parse_from(["peri", "acp", "--setup"])
        .expect("peri acp --setup: global setup flag must parse after the subcommand");
    assert!(acp.setup);
    assert!(matches!(acp.command, Some(Commands::Acp { .. })));
}

#[test]
fn cli_defaults_setup_to_false() {
    let cli = Cli::try_parse_from(["peri"]).expect("peri: defaults must parse");
    assert!(!cli.setup);
}

#[test]
fn validate_cli_rejects_setup_with_settings_stdin() {
    let cli = Cli::try_parse_from(["peri", "acp", "--settings-stdin", "--setup"])
        .expect("the conflict must reach validate_cli, not clap");
    assert_eq!(
        validate_cli(&cli).expect_err("--setup with --settings-stdin must be rejected"),
        "--setup cannot be combined with --settings-stdin"
    );
}

#[test]
fn validate_cli_accepts_setup_without_settings_stdin() {
    let cli = Cli::try_parse_from(["peri", "acp", "--setup"])
        .expect("peri acp --setup: valid setup invocation");
    assert!(validate_cli(&cli).is_ok());
}

#[test]
fn setup_conflict_applies_to_both_sources() {
    let plain = Cli::try_parse_from(["peri", "acp"]).expect("peri acp: valid invocation");
    assert!(!setup_conflicts_with_settings_stdin(false, &plain.command));
    assert!(
        !setup_conflicts_with_settings_stdin(true, &plain.command),
        "a whitelisted ACP invocation without --settings-stdin must not conflict"
    );

    let stdin = Cli::try_parse_from(["peri", "acp", "--settings-stdin"])
        .expect("peri acp --settings-stdin: valid invocation");
    assert!(setup_conflicts_with_settings_stdin(true, &stdin.command));
    assert!(!setup_conflicts_with_settings_stdin(false, &stdin.command));
}
