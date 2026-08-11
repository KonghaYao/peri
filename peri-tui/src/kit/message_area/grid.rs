//! Transcript 水平网格（规格 §3.1）——所有 entry 共享的左对齐时间轴。
//!
//! ```text
//! outer  accent  gap   content                          gap  scroll
//!  1      1      2      flexible                        1      1
//! ```
//!
//! - `outer`：selection border / 安全区，固定 1 cell（渲染层每行首列的空 cell，
//!   焦点条在其上叠加，不造成内容列位移）。
//! - `accent`：固定 1 cell，块首行放类型/状态符号，续行放 dim 竖线。
//! - `gap`：默认 2 cells；Compact/Narrow 缩为 1（§11）。
//! - `content`：所有消息共享左起点；最大可读宽度 100 cells，更宽时余量留右侧。
//! - 断点（§11）：Wide ≥ 100 / Standard 60–99 / Compact 40–59 / Narrow < 40。

/// 响应式断点（§11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    /// `>= 100`：content 最大 100 cells；metadata 可右对齐。
    Wide,
    /// `60–99`：默认布局；metadata 紧跟 summary。
    Standard,
    /// `40–59`：accent gap 缩为 1；隐藏非关键 duration。
    Compact,
    /// `< 40`：accent 线退化为 bullet；无 metadata 列。
    Narrow,
}

/// 网格规格——渲染层所有行渲染器统一消费。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSpec {
    /// selection border / 安全区列宽（固定 1）。
    pub outer: u16,
    /// accent 符号列宽（固定 1）。
    pub accent: u16,
    /// accent 与 content 之间的 gap（Wide/Standard=2，Compact/Narrow=1）。
    pub gap: u16,
    /// content 列宽（所有 entry 正文共享左起点；≤ 100）。
    pub content: u16,
    /// 当前断点。
    pub bp: Breakpoint,
}

impl Default for GridSpec {
    /// 默认 120 列 Wide 网格（未指定宽度时的安全兜底）。
    fn default() -> Self {
        Self::grid_for(120)
    }
}

impl GridSpec {
    /// 按终端宽度计算网格：content = min(term - 6, 100)，余量留右侧。
    /// `term_width` 为终端总列数（MessageArea 区域宽度）。
    pub fn grid_for(term_width: u16) -> Self {
        let bp = match term_width {
            w if w >= 100 => Breakpoint::Wide,
            w if w >= 60 => Breakpoint::Standard,
            w if w >= 40 => Breakpoint::Compact,
            _ => Breakpoint::Narrow,
        };
        let gap = if matches!(bp, Breakpoint::Compact | Breakpoint::Narrow) {
            1
        } else {
            2
        };
        let content = (term_width.saturating_sub(6)).clamp(1, 100);
        Self {
            outer: 1,
            accent: 1,
            gap,
            content,
            bp,
        }
    }

    /// 直接指定 content 列宽的构造器（测试 / 嵌套渲染用），断点按宽度归类。
    pub fn with_content(content: u16) -> Self {
        let mut g = Self::grid_for(content.saturating_add(6).max(7));
        g.content = content.max(1);
        g
    }

    /// content 列宽（usize 形式，渲染层主要使用）。
    pub fn content_width(&self) -> usize {
        self.content as usize
    }

    /// 块首行前缀总宽度 = outer + accent + gap（符号 + gap 前的 1 列 outer 空 cell）。
    pub fn first_prefix_width(&self) -> usize {
        (self.outer + self.accent + self.gap) as usize
    }

    /// 续行前缀总宽度（outer 空 cell + dim 竖线 + gap）。
    pub fn cont_prefix_width(&self) -> usize {
        (self.outer + self.accent + self.gap) as usize
    }

    /// 整行最大宽度（outer + accent + gap + content + 1 滚动条列）。
    pub fn total_width(&self) -> usize {
        (self.outer + self.accent + self.gap + self.content) as usize + 1
    }

    /// Narrow 断点：accent 符号退化为 bullet（§11）。
    pub fn is_narrow(&self) -> bool {
        self.bp == Breakpoint::Narrow
    }

    /// Wide 断点：metadata 可右对齐。
    pub fn is_wide(&self) -> bool {
        self.bp == Breakpoint::Wide
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断点矩阵（§11）：宽度边界值 39/40/59/60/99/100/120。
    #[test]
    fn breakpoint_matrix() {
        assert_eq!(GridSpec::grid_for(120).bp, Breakpoint::Wide);
        assert_eq!(GridSpec::grid_for(100).bp, Breakpoint::Wide);
        assert_eq!(GridSpec::grid_for(99).bp, Breakpoint::Standard);
        assert_eq!(GridSpec::grid_for(80).bp, Breakpoint::Standard);
        assert_eq!(GridSpec::grid_for(60).bp, Breakpoint::Standard);
        assert_eq!(GridSpec::grid_for(59).bp, Breakpoint::Compact);
        assert_eq!(GridSpec::grid_for(40).bp, Breakpoint::Compact);
        assert_eq!(GridSpec::grid_for(39).bp, Breakpoint::Narrow);
        assert_eq!(GridSpec::grid_for(20).bp, Breakpoint::Narrow);
    }

    /// content = min(term - 6, 100)，更宽时余量留右侧；Narrow 也有 ≥1 的 content。
    #[test]
    fn content_caps_at_100_and_min_term_minus_6() {
        assert_eq!(GridSpec::grid_for(120).content, 100);
        assert_eq!(GridSpec::grid_for(100).content, 94);
        assert_eq!(GridSpec::grid_for(80).content, 74);
        assert_eq!(GridSpec::grid_for(60).content, 54);
        assert_eq!(GridSpec::grid_for(40).content, 34);
        assert_eq!(GridSpec::grid_for(30).content, 24);
        assert_eq!(GridSpec::grid_for(6).content, 1);
    }

    /// gap：Wide/Standard = 2，Compact/Narrow = 1（§11）。
    #[test]
    fn gap_by_breakpoint() {
        assert_eq!(GridSpec::grid_for(120).gap, 2);
        assert_eq!(GridSpec::grid_for(60).gap, 2);
        assert_eq!(GridSpec::grid_for(59).gap, 1);
        assert_eq!(GridSpec::grid_for(39).gap, 1);
    }

    /// 整行宽度（含前缀与滚动条列）不超过终端宽度——行渲染器按此保证不换行。
    #[test]
    fn line_width_within_terminal() {
        for w in [40u16, 60, 80, 100, 120, 200] {
            let g = GridSpec::grid_for(w);
            assert!(
                g.total_width() <= w as usize,
                "term={w}: total_width {} 超出终端宽度",
                g.total_width()
            );
        }
    }
}
