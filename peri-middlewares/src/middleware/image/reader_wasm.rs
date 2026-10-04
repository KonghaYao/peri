//! The Workspace image custom request is absent when builtin MCP is disabled.

use crate::mcp::McpClientPool;

pub(super) struct ImageBytes {
    pub data: Vec<u8>,
    pub media_type: String,
}

pub(super) async fn read_image(
    _pool: Option<&McpClientPool>,
    _session_id: Option<&str>,
    _path: &str,
    _max_size: usize,
) -> Result<ImageBytes, String> {
    Err("Workspace image capability unavailable".into())
}
