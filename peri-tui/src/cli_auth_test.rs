use super::AuthCommand;
use super::ChatGptAction;

use clap::Parser;

#[test]
fn test_auth_cli_parses_account_and_explicit_store() {
    let explicit_store = std::env::temp_dir().join("auth-test");
    let cli = crate::Cli::try_parse_from(vec![
        std::ffi::OsString::from("peri"),
        "auth".into(),
        "chatgpt".into(),
        "login".into(),
        "--store".into(),
        explicit_store.as_os_str().to_owned(),
        "--account".into(),
        "work".into(),
    ])
    .expect("auth cli parses account and explicit store: fixture must have the expected variant");
    let Some(crate::Commands::Auth {
        action: AuthCommand::Chatgpt {
            action: ChatGptAction::Login(options),
        },
    }) = cli.command
    else {
        panic!("应选择 ChatGPT login");
    };
    assert_eq!(options.account, "work");
    assert_eq!(options.store, Some(explicit_store));
}

#[test]
fn test_auth_store_default_tracks_selected_config_file() {
    let config = std::env::temp_dir().join("isolated/settings.json");
    let store = super::credential_store(&config, None).expect("auth store default tracks selected config file: the credential_store fixture operation uses valid inputs");
    assert_eq!(store, config.parent().expect("auth store default tracks selected config file: the fixture path must have its expected parent directory").join("credentials/chatgpt"));
}

#[test]
fn test_auth_cli_rejects_token_arguments() {
    let error = crate::Cli::try_parse_from([
        "peri",
        "auth",
        "chatgpt",
        "login",
        "--access-token",
        "not-a-token",
    ])
    .err()
    .expect("CLI 不接受 token 字面值");
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}
