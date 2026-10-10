//! Deployment-specific configuration data plane.
//! Native hosts use the MCP configuration service; Emscripten uses its virtual FS.

#[cfg(not(target_os = "emscripten"))]
pub use peri_mcp_config::{
    canonicalize, current_dir, exists, global_config_path, home_dir, read_environment, read_text,
    same_file, set_global_config_path, write_text_atomic, write_text_if_unchanged,
};

#[cfg(target_os = "emscripten")]
#[path = "io/wasm.rs"]
mod wasm;
#[cfg(target_os = "emscripten")]
pub use wasm::*;
