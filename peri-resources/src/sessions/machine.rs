use anyhow::{Context, Result};
use std::{fs, path::Path, sync::OnceLock};
use uuid::Uuid;

static MACHINE_ID: OnceLock<String> = OnceLock::new();

pub(crate) async fn initialize() -> Result<&'static str> {
    if let Some(identity) = MACHINE_ID.get() {
        return Ok(identity);
    }
    let identity = tokio::task::spawn_blocking(|| {
        if let Ok(identity) = std::env::var("PERI_MACHINE_ID") {
            return Ok(Uuid::parse_str(&identity)
                .context("PERI_MACHINE_ID must be a UUID")?
                .to_string());
        }
        let home = dirs_next::home_dir().context("machine identity requires a home directory")?;
        load_or_create(&home.join(".peri/machine-id"))
    })
    .await??;
    let _ = MACHINE_ID.set(identity);
    current()
}

pub(crate) fn current() -> Result<&'static str> {
    MACHINE_ID
        .get()
        .map(String::as_str)
        .context("machine identity is not initialized")
}

fn read_identity(path: &Path) -> Result<String> {
    let identity = fs::read_to_string(path).context("cannot read machine identity")?;
    Ok(Uuid::parse_str(identity.trim())
        .context("stored machine identity is invalid")?
        .to_string())
}

fn load_or_create(path: &Path) -> Result<String> {
    match fs::metadata(path) {
        Ok(_) => return read_identity(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot inspect machine identity"),
    }
    let parent = path.parent().context("machine identity has no parent")?;
    fs::create_dir_all(parent).context("cannot create machine identity directory")?;
    let identity = Uuid::new_v4().to_string();
    let staging = parent.join(format!(".machine-id-{}", Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        use std::io::Write;
        let mut file = options.open(&staging)?;
        file.write_all(identity.as_bytes())?;
        file.sync_all()?;
        match fs::hard_link(&staging, path) {
            Ok(()) => Ok(identity),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_identity(path),
            Err(error) => Err(error).context("cannot publish machine identity"),
        }
    })();
    let _ = fs::remove_file(staging);
    result
}

#[cfg(test)]
#[path = "machine_test.rs"]
mod tests;
