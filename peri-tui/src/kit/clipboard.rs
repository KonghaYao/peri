use std::io::Write;

use anyhow::Context;
use base64::Engine;

#[derive(Debug, PartialEq, Eq)]
enum ClipboardBackend {
    Native,
    Terminal,
}

fn backend_for(is_set: impl Fn(&str) -> bool) -> ClipboardBackend {
    if [
        "SSH_CONNECTION",
        "SSH_CLIENT",
        "SSH_TTY",
        "TMUX",
        "HERDR_ENV",
    ]
    .into_iter()
    .any(is_set)
    {
        ClipboardBackend::Terminal
    } else {
        ClipboardBackend::Native
    }
}

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    match backend_for(|name| std::env::var_os(name).is_some()) {
        ClipboardBackend::Native => arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text))
            .context("Failed to write native clipboard"),
        ClipboardBackend::Terminal => write_osc52(&mut std::io::stdout().lock(), text)
            .context("Failed to send terminal clipboard write"),
    }
}

fn write_osc52(writer: &mut impl Write, text: &str) -> std::io::Result<()> {
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    writer.write_all(format!("\x1b]52;c;{payload}\x07").as_bytes())?;
    writer.flush()
}

#[cfg(test)]
#[path = "clipboard_test.rs"]
mod tests;
