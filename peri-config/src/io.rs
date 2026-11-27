//! Deployment-specific configuration data plane.
//! All hosts use the MCP configuration service.

pub use peri_mcp_config::{
    canonicalize, current_dir, exists, global_config_path, home_dir, read_environment, read_text,
    same_file, set_global_config_path, write_text_atomic, write_text_if_unchanged,
};
