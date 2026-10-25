use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub(super) enum AuthCommand {
    /// Sign in with ChatGPT for an authorized ChatGPT plan grant
    Chatgpt {
        #[command(subcommand)]
        action: ChatGptAction,
    },
}

#[derive(Subcommand)]
pub(super) enum ChatGptAction {
    /// Print an authorization URL, then wait for the local OAuth callback
    Login(ChatGptOptions),
    /// Show account identity and grant status without exposing tokens
    Status(ChatGptOptions),
    /// Revoke this account grant and remove its local credentials
    Logout(ChatGptOptions),
}

#[derive(Args)]
pub(super) struct ChatGptOptions {
    /// Credential directory; defaults beside the selected settings file
    #[arg(long)]
    store: Option<PathBuf>,
    /// Local account reference used in provider.auth.account
    #[arg(long, default_value = "default")]
    account: String,
}

fn credential_store(config_path: &Path, explicit: Option<PathBuf>) -> Result<PathBuf> {
    let path = explicit.unwrap_or_else(|| {
        config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("credentials/chatgpt")
    });
    std::path::absolute(path).context("resolve credentials directory")
}

pub(super) async fn run(action: AuthCommand, config_path: PathBuf) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (action, config_path);
        anyhow::bail!("ChatGPT login requires a native credentials MCP capability");
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        use peri_acp::provider::auth::ChatGptAuthManager;
        use tokio_util::sync::CancellationToken;
        let AuthCommand::Chatgpt { action } = action;
        let options = match &action {
            ChatGptAction::Login(options)
            | ChatGptAction::Status(options)
            | ChatGptAction::Logout(options) => options,
        };
        let store = credential_store(&config_path, options.store.clone())?;
        let manager = ChatGptAuthManager::new(store.clone(), options.account.clone())?;
        match action {
            ChatGptAction::Login(options) => {
                let pending = manager.begin_login().await?;
                eprintln!(
                    "Open this URL in your browser and choose your ChatGPT account:\n{}",
                    pending.authorization_url()
                );
                let cancellation = CancellationToken::new();
                let finish = manager.finish_login(pending, cancellation.clone());
                tokio::pin!(finish);
                let status = tokio::select! {
                    result = &mut finish => result?,
                    signal = tokio::signal::ctrl_c() => {
                        signal?;
                        cancellation.cancel();
                        finish.await?
                    }
                };
                println!("{}", serde_json::to_string_pretty(&status)?);
                eprintln!(
                    "Use this auth reference in an openai_responses provider:\n{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "auth": {"kind": "chatgpt_plan", "store": store, "account": options.account,
                            "subject": status.subject, "clientId": status.client_id}
                    }))?
                );
            }
            ChatGptAction::Status(options) => {
                let status = manager.status().await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "account": options.account, "authenticated": status.is_some(), "status": status
                    }))?
                );
            }
            ChatGptAction::Logout(_) => {
                let outcome = manager.logout(CancellationToken::new()).await?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "cli_auth_test.rs"]
mod tests;
