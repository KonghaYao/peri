pub mod canonical;
pub mod channel_flow;
pub mod crypto;
pub mod device;
pub mod device_cli;
pub mod http_client;
pub mod keystore;
pub mod limits;
pub mod noise_session;
pub mod packer;
pub mod protocol;
pub mod receiver;
pub mod scanner;
pub mod sender;
pub mod sync_code;
pub mod ui;
pub mod writer;

pub use channel_flow::{run_receive_cli, run_send_cli};
pub use device_cli::dispatch as run_device_command;
pub use receiver::run_sync_receiver;
pub use sender::run_sync_sender;

#[cfg(test)]
mod canonical_test;
#[cfg(test)]
mod channel_flow_test;
#[cfg(test)]
mod crypto_test;
#[cfg(test)]
mod device_cli_test;
#[cfg(test)]
mod device_test;
#[cfg(test)]
mod http_client_test;
#[cfg(test)]
mod keystore_test;
#[cfg(test)]
mod limits_test;
#[cfg(test)]
mod noise_session_test;
#[cfg(test)]
mod packer_test;
#[cfg(test)]
mod protocol_test;
#[cfg(test)]
mod scanner_test;
#[cfg(test)]
mod sync_code_test;
#[cfg(test)]
mod ui_test;
#[cfg(test)]
mod writer_test;
