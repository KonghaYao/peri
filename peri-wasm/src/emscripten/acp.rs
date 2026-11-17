//! JavaScript port for the existing ACP Host. Protocol methods remain in peri-acp.

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp::host;
use peri_acp::host::assemble;
use peri_acp::host::assemble::WasmHostAssemblyInput;
use peri_acp::host::AcpHostHandle;
use peri_acp::provider::ConfigSource;
use peri_acp::provider::LlmProvider;
use peri_acp::transport::mpsc;
use peri_acp::transport::wire_bridge::WireBridge;
use peri_acp::transport::AcpTransport;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use tokio::sync::Mutex;

#[path = "acp/peri_wasm_acp.rs"]
mod peri_wasm_acp;
