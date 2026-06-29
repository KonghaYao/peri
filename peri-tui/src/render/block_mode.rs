//! Markdown 围栏块模式（渲染层内部细节）。
//!
//! 设计目标（参见 `docs/design/peri-tui-architecture.md` §4.6）：
//!
//! 流式输出进入 Markdown 代码围栏（```）时，渲染层检测围栏边界，在围栏
//! 内部缓冲段落，等闭合标记到达后一次性渲染。避免代码块逐字出现造成
//! 的闪烁。
//!
//! **此逻辑是渲染层内部实现，状态机不感知。**
//!
//! ## 当前状态（P5 骨架）
//!
//! 本模块目前是占位骨架。完整实现需要：
//! - 检测流式 chunk 中的 `` ``` `` 边界
//! - 在围栏打开期间累积 chunks 到 buffer
//! - 围栏闭合时一次性 flush 到输出
//!
//! 见 `docs/design/peri-tui-architecture.md` Phase 5。

/// 围栏块模式状态机。
///
/// 跟踪当前是否处于 Markdown 代码围栏内部。在围栏打开期间，
/// 调用方应缓冲 chunks 而非立即渲染。
#[derive(Debug, Clone, Default)]
pub struct BlockMode {
    /// 当前已嵌套的围栏层数（同类型围栏可嵌套）。
    /// 0 = 不在任何围栏内部。
    fence_depth: usize,
}

impl BlockMode {
    /// 创建新的块模式状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前是否处于代码围栏内部（应缓冲）。
    pub fn is_inside_fence(&self) -> bool {
        self.fence_depth > 0
    }

    /// 重置状态。
    pub fn reset(&mut self) {
        self.fence_depth = 0;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_not_inside_fence() {
        let bm = BlockMode::default();
        assert!(!bm.is_inside_fence());
    }

    #[test]
    fn test_new_equals_default() {
        let bm = BlockMode::new();
        assert!(!bm.is_inside_fence());
    }

    #[test]
    fn test_reset_clears_state() {
        let mut bm = BlockMode::new();
        bm.reset();
        assert!(!bm.is_inside_fence());
    }
}
