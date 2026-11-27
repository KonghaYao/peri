mod client;
mod server;

pub mod model_auth;

pub use client::OAuthCredentialClient;
pub use server::OAuthCredentialMcpServer;

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
