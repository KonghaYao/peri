use std::{mem::ManuallyDrop, sync::Arc};

use crate::tools::BaseTool;

/// 将 Box<dyn BaseTool> 转换为 Arc<dyn BaseTool>
///
/// Rust 标准库不直接支持 `Box<dyn Trait>` → `Arc<dyn Trait>` 转换。
/// 通过一个中间 wrapper struct 持有 `ManuallyDrop<Box<dyn BaseTool>>`，
/// 实现 `BaseTool` trait 透传所有调用，再用 `Arc::from()` 创建 trait object。
pub(crate) fn box_to_arc(tool: Box<dyn BaseTool>) -> Arc<dyn BaseTool> {
    struct ToolWrapper(ManuallyDrop<Box<dyn BaseTool>>);

    #[async_trait::async_trait]
    impl BaseTool for ToolWrapper {
        fn name(&self) -> &str {
            self.0.name()
        }
        fn description(&self) -> &str {
            self.0.description()
        }
        fn parameters(&self) -> serde_json::Value {
            self.0.parameters()
        }
        async fn invoke(
            &self,
            input: serde_json::Value,
            ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.invoke(input, ctx).await
        }
    }

    Arc::new(ToolWrapper(ManuallyDrop::new(tool)))
}
