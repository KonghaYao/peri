#[cfg(not(target_os = "emscripten"))]
mod client;
#[cfg(not(target_os = "emscripten"))]
mod server;
#[cfg(target_os = "emscripten")]
mod wasm;

#[cfg(not(target_os = "emscripten"))]
use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[cfg(not(target_os = "emscripten"))]
pub use client::ConfigurationClient;
#[cfg(not(target_os = "emscripten"))]
pub use server::ConfigurationMcpServer;
#[cfg(target_os = "emscripten")]
pub use wasm::*;

#[cfg(not(target_os = "emscripten"))]
static CLIENT: OnceLock<Result<ConfigurationClient, String>> = OnceLock::new();

#[cfg(not(target_os = "emscripten"))]
fn client() -> io::Result<&'static ConfigurationClient> {
    CLIENT
        .get_or_init(|| ConfigurationClient::local().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| io::Error::other(message.clone()))
}

#[cfg(not(target_os = "emscripten"))]
pub fn install_client(client: ConfigurationClient) -> io::Result<()> {
    CLIENT.set(Ok(client)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "configuration data plane already initialized",
        )
    })
}

#[cfg(not(target_os = "emscripten"))]
pub fn read_text(path: &Path) -> io::Result<String> {
    client()?.read_text(path)
}

#[cfg(not(target_os = "emscripten"))]
pub fn read_environment(name: &str) -> io::Result<Option<String>> {
    client()?.read_environment(name)
}

#[cfg(not(target_os = "emscripten"))]
pub fn write_text_atomic(path: &Path, content: &str) -> io::Result<()> {
    client()?.write_text_atomic(path, content)
}

#[cfg(not(target_os = "emscripten"))]
pub fn write_text_if_unchanged(
    path: &Path,
    expected: &Option<String>,
    content: &str,
) -> io::Result<bool> {
    client()?.write_text_if_unchanged(path, expected, content)
}

#[cfg(not(target_os = "emscripten"))]
pub fn exists(path: &Path) -> io::Result<bool> {
    client()?.exists(path)
}

#[cfg(not(target_os = "emscripten"))]
pub fn same_file(first: &Path, second: &Path) -> io::Result<bool> {
    client()?.same_file(first, second)
}

#[cfg(not(target_os = "emscripten"))]
pub fn global_config_path() -> PathBuf {
    client()
        .and_then(ConfigurationClient::paths)
        .expect("configuration data plane unavailable")
        .global_settings
}

#[cfg(not(target_os = "emscripten"))]
pub fn set_global_config_path(path: Option<PathBuf>) {
    client()
        .and_then(|client| client.set_global_config_path(path))
        .expect("configuration data plane unavailable");
}

#[cfg(not(target_os = "emscripten"))]
pub fn home_dir() -> Option<PathBuf> {
    client()
        .and_then(ConfigurationClient::paths)
        .expect("configuration data plane unavailable")
        .home
}

#[cfg(not(target_os = "emscripten"))]
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
