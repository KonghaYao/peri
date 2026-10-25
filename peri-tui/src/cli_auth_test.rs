use super::*;
use clap::Parser;

#[test]
fn test_auth_cli_parses_account_and_explicit_store() {
    let cli = crate::Cli::try_parse_from([
        "peri",
        "auth",
        "chatgpt",
        "login",
        "--store",
        "/tmp/auth-test",
        "--account",
        "work",
    ])
    .unwrap();
    let Some(crate::Commands::Auth {
        action: AuthCommand::Chatgpt {
            action: ChatGptAction::Login(options),
        },
    }) = cli.command
    else {
        panic!("应选择 ChatGPT login");
    };
    assert_eq!(options.account, "work");
    assert_eq!(options.store, Some(PathBuf::from("/tmp/auth-test")));
}

#[test]
fn test_auth_store_default_tracks_selected_config_file() {
    let store = credential_store(Path::new("/tmp/isolated/settings.json"), None).unwrap();
    assert_eq!(store, PathBuf::from("/tmp/isolated/credentials/chatgpt"));
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
