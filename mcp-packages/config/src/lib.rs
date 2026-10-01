mod client;
mod server;

use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub use client::ConfigurationClient;
pub use server::ConfigurationMcpServer;

static CLIENT: OnceLock<Result<ConfigurationClient, String>> = OnceLock::new();

fn client() -> io::Result<&'static ConfigurationClient> {
    CLIENT
        .get_or_init(|| ConfigurationClient::local().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| io::Error::other(message.clone()))
}

pub fn install_client(client: ConfigurationClient) -> io::Result<()> {
    CLIENT.set(Ok(client)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "configuration data plane already initialized",
        )
    })
}

pub fn read_text(path: &Path) -> io::Result<String> {
    client()?.read_text(path)
}

pub fn read_environment(name: &str) -> io::Result<Option<String>> {
    client()?.read_environment(name)
}

pub fn write_text_atomic(path: &Path, content: &str) -> io::Result<()> {
    client()?.write_text_atomic(path, content)
}

pub fn write_text_if_unchanged(
    path: &Path,
    expected: &Option<String>,
    content: &str,
) -> io::Result<bool> {
    client()?.write_text_if_unchanged(path, expected, content)
}

pub fn exists(path: &Path) -> io::Result<bool> {
    client()?.exists(path)
}

pub fn same_file(first: &Path, second: &Path) -> io::Result<bool> {
    client()?.same_file(first, second)
}

pub fn global_config_path() -> PathBuf {
    client()
        .and_then(ConfigurationClient::paths)
        .expect("configuration data plane unavailable")
        .global_settings
}

pub fn set_global_config_path(path: Option<PathBuf>) {
    client()
        .and_then(|client| client.set_global_config_path(path))
        .expect("configuration data plane unavailable");
}

pub fn home_dir() -> Option<PathBuf> {
    client()
        .and_then(ConfigurationClient::paths)
        .expect("configuration data plane unavailable")
        .home
}

pub fn current_dir() -> io::Result<PathBuf> {
    client()?
        .paths()?
        .cwd
        .ok_or_else(|| io::Error::other("configuration working directory unavailable"))
}

#[cfg(test)]
mod cas_test;
#[cfg(test)]
mod tests;
