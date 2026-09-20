//! Transcript 水平网格（规格 §3.1）——所有 entry 共享的左对齐时间轴。
//!
//! ```text
//! |←— left_pad —→|←————————— band_width —————————→|← 余量 →|
//!                outer  accent  gap   content   gutter  scroll
//!                  1      1      2   flexible   ≤ 12      1
//! ```
//!
//! - `outer`：selection border / 安全区，固定 1 cell（渲染层每行首列的空 cell，
//!   焦点条在其上叠加，不造成内容列位移）。
//! - `accent`：固定 1 cell，块首行放类型/状态符号，续行放 dim 竖线。
//! - `gap`：默认 2 cells；Compact/Narrow 缩为 1（§11）。
//! - `content`：所有消息共享左起点；最大可读宽度 100 cells。
//! - `gutter`：metadata（duration / 计数）右对齐列，最多 12 cells。
//! - `scroll`：最右 1 cell 留给滚动条 thumb（文本不进该列）。
//! - 断点（§11）：Wide ≥ 100 / Standard 60–99 / Compact 40–59 / Narrow < 40。
//!
//! 整条带在终端内水平居中：content 触顶后余量由「全部堆在内容与 metadata 之间」
//! 改为左右均分（`left_pad`），内容与 metadata 不再一个贴左缘、一个贴右缘。
//! 带宽不超过终端宽度——窄终端 `left_pad = 0`，逐列行为与居中前一致。

/// content 列宽上限——可读行宽（§3.1）。
pub const MAX_CONTENT_WIDTH: u16 = 100;

/// metadata gutter 上限：最长 duration `123min 45s`（10 列）+ 与 content 的 2 列间隔。
/// 取满此上限才能保证「summary ≤ content」时 metadata 恒可右对齐（不因收窄而丢失）。
pub const MAX_META_GUTTER: u16 = 12;

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
    /// 居中带宽度（消息区 / 输入区实际区域宽度，含最右滚动条列）——
    /// 组件的区域宽度（`MsgAreaTracker` / `AreaTracker`）与 `line_width()` 由它派生。
    pub band_width: u16,
    /// 居中带左偏移（终端列坐标）——余量左右均分，奇数余量留右侧。
    pub left_pad: u16,
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
    /// 按终端宽度计算网格：`content = min(term - 6, 100)`，整条带水平居中。
    ///
    /// 带宽 = outer + accent + gap + content + metadata gutter + 滚动条列，
    /// 以终端宽度封顶：窄终端（带宽 ≥ 终端宽）`left_pad = 0`，与居中前逐列一致。
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
        let content = (term_width.saturating_sub(6)).clamp(1, MAX_CONTENT_WIDTH);
        let band_width = (1 + 1 + gap + content + MAX_META_GUTTER + 1).min(term_width);
        Self {
            outer: 1,
            accent: 1,
            gap,
            content,
            band_width,
            left_pad: term_width.saturating_sub(band_width) / 2,
            bp,
        }
    }

    /// 直接指定 content 列宽的构造器（测试 / 嵌套渲染用），断点按宽度归类。
    /// 嵌套面板（subagent 详情）不参与居中：`left_pad = 0`，
    /// band_width 取 `content + 2`（outer 1 + 滚动条 1）——嵌套面板的实际宽度即此值，
    /// metadata 右对齐到面板右缘前 1 列。
    pub fn with_content(content: u16) -> Self {
        let content = content.max(1);
        let mut g = Self::grid_for(content.saturating_add(6).max(7));
        g.content = content;
        g.band_width = content.saturating_add(2);
        g.left_pad = 0;
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

    /// 单行最大宽度 = band_width - 1（最右 1 列留给滚动条 thumb）。
    ///
    /// 这是 metadata 右对齐的落点，也是渲染换行宽度——`Paragraph` 换行宽度、
    /// slot wrap_map 与选区列映射必须同取此值，否则 metadata 行会在视觉行计数上二次折行。
    pub fn line_width(&self) -> u16 {
        self.band_width.saturating_sub(1)
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

    /// content = min(term - 6, 100)；Narrow 也有 ≥1 的 content。
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

    /// 未触顶（content < 100）：带宽 = 终端宽、不居中、满行宽度 = 终端宽 - 1——
    /// 与居中前的逐列行为完全一致（窄终端不受本次改动影响）。
    #[test]
    fn band_fills_terminal_before_content_caps() {
        for term in [30u16, 40, 60, 80, 100, 106] {
            let g = GridSpec::grid_for(term);
            assert_eq!(g.band_width, term, "term={term}：带宽应铺满终端");
            assert_eq!(g.left_pad, 0, "term={term}：不居中");
            assert_eq!(g.line_width(), term - 1, "term={term}：满行宽度");
            assert_eq!(g.content, term - 6, "term={term}：content 未触顶");
        }
    }

    /// 触顶（content = 100）：带宽固定 117，余量左右均分（奇数余量留右侧）。
    #[test]
    fn band_centers_after_content_caps() {
        let g = GridSpec::grid_for(120);
        assert_eq!(g.content, 100);
        assert_eq!(g.band_width, 117);
        assert_eq!(g.left_pad, 1);

        let g = GridSpec::grid_for(200);
        assert_eq!(g.band_width, 117);
        assert_eq!(g.left_pad, 41);
        assert_eq!(
            g.left_pad as usize * 2 + g.band_width as usize,
            199,
            "余量均分，奇数余 1 列留在右侧"
        );

        // 触顶起点：带被终端宽度封顶，居中从 0 连续过渡
        let g = GridSpec::grid_for(117);
        assert_eq!(g.band_width, 117);
        assert_eq!(g.left_pad, 0);
    }

    /// 带（含滚动条列）不超出终端宽度——行渲染器按 line_width 保证不换行。
    #[test]
    fn band_within_terminal() {
        for w in [1u16, 6, 40, 60, 80, 100, 106, 117, 120, 200] {
            let g = GridSpec::grid_for(w);
            assert!(
                g.band_width <= w,
                "term={w}: band_width {} 超出终端宽度",
                g.band_width
            );
            assert!(g.line_width() <= w.saturating_sub(1), "term={w}: 满行宽度");
        }
    }

    /// metadata gutter：content 触顶后恒能容纳最长 duration（summary ≤ content），
    /// 不因居中收窄而丢失右对齐 metadata。
    #[test]
    fn meta_gutter_fits_longest_metadata() {
        let longest = unicode_width::UnicodeWidthStr::width("123min 45s");
        for term in [117u16, 120, 160, 240] {
            let g = GridSpec::grid_for(term);
            assert_eq!(
                g.band_width,
                (g.first_prefix_width() as u16) + g.content + MAX_META_GUTTER + 1,
                "term={term}: gutter 应为上限值"
            );
            assert!(
                g.first_prefix_width() + g.content_width() + 2 + longest <= g.line_width() as usize,
                "term={term}: 最长 metadata 应可右对齐"
            );
        }
    }

    /// 嵌套面板（with_content）不居中：带宽 = content + 2（outer + 滚动条），
    /// 满行宽度 = content + 1。
    #[test]
    fn nested_grid_is_not_centered() {
        let nested = GridSpec::with_content(100);
        assert_eq!(nested.band_width, 102, "嵌套面板宽 = content + 2");
        assert_eq!(nested.line_width(), 101);
        assert_eq!(nested.left_pad, 0);
        assert_eq!(nested.content, 100);
    }
}
