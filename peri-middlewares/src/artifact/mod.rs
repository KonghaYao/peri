//! Artifact 上传能力及其独立装配 middleware。

// `builtin/artifact.rs` 的 handler 需要构造可注入 base url / token 的客户端
// （无网络测试）；生产路径仍只经 `ArtifactTool::new`。
pub(crate) mod client;
mod tool;

use peri_agent::{middleware::r#trait::Middleware, tools::BaseTool};

pub use tool::ArtifactTool;

/// 独立承载 Artifact 工具，使 MetaHarness 可单独关闭公开上传能力。
pub struct ArtifactMiddleware;

impl ArtifactMiddleware {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ArtifactMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Middleware for ArtifactMiddleware {
    fn name(&self) -> &str {
        "ArtifactMiddleware"
    }

    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(ArtifactTool::new(cwd.to_string()))]
    }
}
