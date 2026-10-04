//! Type-level cleanup seam for transports that cannot exist on Emscripten.
//!
//! No constructor is exposed: the stdio MCP admission paths reject this target.

pub(crate) struct McpProcessOwner;

impl McpProcessOwner {
    pub(crate) fn begin_close(&self) {}

    pub(crate) async fn close(&self) -> std::io::Result<()> {
        Ok(())
    }
}
