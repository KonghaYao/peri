//! Synchronous configuration port for an Emscripten deployment's virtual FS.
//!
//! The native bootstrap MCP client owns a runtime thread and a local file-locking
//! server. Neither exists in a browser worker. Session persistence is supplied
//! separately; this port reads the deployment's injected configuration files.

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

static GLOBAL_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn read_text(path: &Path) -> io::Result<String> {
    std::fs::read_to_string(path)
}

pub fn read_environment(name: &str) -> io::Result<Option<String>> {
    if name.is_empty() || name.contains(['=', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid configuration environment variable name",
        ));
    }
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration environment variable is not valid Unicode",
        )),
    }
}

pub fn write_text_atomic(path: &Path, content: &str) -> io::Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    write_atomic_locked(path, content)
}

pub fn write_text_if_unchanged(
    path: &Path,
    expected: &Option<String>,
    content: &str,
) -> io::Result<bool> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let current = match std::fs::read(path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if current.as_deref() != expected.as_deref().map(str::as_bytes) {
        return Ok(false);
    }
    write_atomic_locked(path, content)?;
    Ok(true)
}

fn write_atomic_locked(path: &Path, content: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".peri-config-{}.tmp", uuid::Uuid::new_v4()));
    let outcome = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        std::fs::rename(&temporary, path)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

pub fn exists(path: &Path) -> io::Result<bool> {
    path.try_exists()
}

pub fn same_file(first: &Path, second: &Path) -> io::Result<bool> {
    let first = canonicalize(first)?;
    let second = canonicalize(second)?;
    Ok(first.is_some() && first == second)
}

pub fn canonicalize(path: &Path) -> io::Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn global_config_path() -> PathBuf {
    GLOBAL_PATH
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or_else(|| {
            home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".peri/settings.json")
        })
}

pub fn set_global_config_path(path: Option<PathBuf>) {
    let path = path.map(|path| {
        if path.is_relative() {
            std::env::current_dir()
                .map(|cwd| cwd.join(&path))
                .unwrap_or(path)
        } else {
            path
        }
    });
    *GLOBAL_PATH
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = path;
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(dirs_next::home_dir)
}

pub fn current_dir() -> io::Result<PathBuf> {
    std::env::current_dir()
}
