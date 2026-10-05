use anyhow::{Context, Result};
use std::sync::OnceLock;
#[cfg(not(target_os = "emscripten"))]
use std::{fs, path::Path};
use uuid::Uuid;

static MACHINE_ID: OnceLock<String> = OnceLock::new();

#[cfg(target_os = "emscripten")]
pub(crate) fn set_explicit(value: &str) -> Result<()> {
    let identity = Uuid::parse_str(value)
        .context("WASM machine identity must be a UUID")?
        .to_string();
    if let Some(current) = MACHINE_ID.get() {
        anyhow::ensure!(
            current == &identity,
            "WASM machine identity changed within one instance"
        );
    } else {
        let _ = MACHINE_ID.set(identity.clone());
        anyhow::ensure!(
            MACHINE_ID.get() == Some(&identity),
            "WASM machine identity changed within one instance"
        );
    }
    Ok(())
}

#[cfg(target_os = "emscripten")]
pub(crate) async fn initialize() -> Result<&'static str> {
    current()
}

#[cfg(not(target_os = "emscripten"))]
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

pub fn current() -> Result<&'static str> {
    MACHINE_ID
        .get()
        .map(String::as_str)
        .context("machine identity is not initialized")
}

#[cfg(not(target_os = "emscripten"))]
fn read_identity(path: &Path) -> Result<String> {
    let identity = fs::read_to_string(path).context("cannot read machine identity")?;
    Ok(Uuid::parse_str(identity.trim())
        .context("stored machine identity is invalid")?
        .to_string())
}

#[cfg(not(target_os = "emscripten"))]
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

/// Explicitly adopt a known Machine identity after the caller has reviewed the
/// target catalog and stopped active executions. The current process keeps its
/// cached identity; the new value takes effect only after restart.
#[cfg(not(target_os = "emscripten"))]
pub fn adopt_file_identity(expected: &str, target: &str) -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("PERI_MACHINE_ID").is_none(),
        "PERI_MACHINE_ID override is active; change that deployment value instead"
    );
    let expected = Uuid::parse_str(expected)
        .context("current Machine ID must be a UUID")?
        .to_string();
    let target = Uuid::parse_str(target)
        .context("target Machine ID must be a UUID")?
        .to_string();
    let home = dirs_next::home_dir().context("machine identity requires a home directory")?;
    let path = home.join(".peri/machine-id");
    adopt_identity_at(&path, &expected, &target)
}

#[cfg(not(target_os = "emscripten"))]
fn adopt_identity_at(path: &Path, expected: &str, target: &str) -> Result<()> {
    let parent = path.parent().context("machine identity has no parent")?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        // 锁文件内容不参与判定，只借它的 inode 做互斥。
        .truncate(false)
        .open(parent.join("machine-id.adopt.lock"))?;
    lock.lock()
        .context("cannot lock machine identity adoption")?;
    anyhow::ensure!(
        read_identity(path)? == expected,
        "current Machine ID changed"
    );
    if expected == target {
        return Ok(());
    }
    let staging = parent.join(format!(".machine-id-adopt-{}", Uuid::new_v4()));
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
        file.write_all(target.as_bytes())?;
        file.sync_all()?;
        drop(file);
        anyhow::ensure!(
            read_identity(path)? == expected,
            "current Machine ID changed"
        );
        fs::rename(&staging, path).context("cannot replace machine identity")?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(staging);
    result
}

#[cfg(test)]
#[path = "machine_test.rs"]
mod tests;
