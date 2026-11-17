//! Durable stdout/stderr capture for shell tasks.
//!
//! The in-memory preview is deliberately bounded, but a promoted shell must
//! continue writing its original pipes to disk from the moment it starts.
//! This module owns the side effects and returns only typed evidence to the
//! task result.

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use peri_acp_types::event::ShellOutput;
use tokio::io::AsyncWriteExt;

#[cfg(test)]
#[path = "shell_output_test.rs"]
mod tests;

#[path = "shell_output/shell_output_capture.rs"]
mod shell_output_capture;
pub use shell_output_capture::ShellOutputCapture;
pub use shell_output_capture::ShellOutputWriter;
