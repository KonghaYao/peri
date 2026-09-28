//! Artifact 上传能力的内部实现（工具 + 客户端）。

// `builtin/artifact.rs` 的 handler 需要构造可注入 base url / token 的客户端
// （无网络测试）；生产路径仍只经 `ArtifactTool::new`。
pub(crate) mod client;
mod tool;

pub use tool::ArtifactTool;
