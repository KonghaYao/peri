use ratatui::widgets::{Block, Borders};
use ratatui::{layout::Rect, style::Style, Frame};
use tui_textarea::TextArea;

use crate::ui::theme;

/// 基于 `tui_textarea::TextArea` 的统一文本输入组件，
/// 替代所有 `String + usize` 和 `InputState` 用法。
#[derive(Debug)]
pub struct FieldTextarea {
    inner: TextArea<'static>,
    max_lines: u16,
}

/// 公共样式配置：无边框、无光标行高亮、主题文字色 + 纯黑背景
///
/// 透明背景（Color::Reset）在 form overlay 场景中会让底层静态文字穿透。
/// 若 textarea 因光标位置触发水平滚动（top_col > 0），文字起始列偏离底层
/// 静态文字位置，两个文字不同步但都可见，形成双重影。
/// 不透明背景（POPUP_BG = #000000）确保 textarea 渲染区域完全覆盖底层。
fn configure_style(inner: &mut TextArea<'static>) {
    inner.set_block(Block::default().borders(Borders::NONE));
    inner.set_cursor_line_style(Style::default());
    inner.set_style(Style::default().fg(theme::TEXT).bg(theme::POPUP_BG));
}

impl FieldTextarea {
    /// 单行输入框（max_lines=1）
    pub fn single_line() -> Self {
        let mut inner = TextArea::default();
        configure_style(&mut inner);
        Self {
            inner,
            max_lines: 1,
        }
    }

    /// 多行输入框（max_lines ≥ 1）
    pub fn multi_line(max: u16) -> Self {
        let mut inner = TextArea::default();
        configure_style(&mut inner);
        Self {
            inner,
            max_lines: max.max(1),
        }
    }

    /// 处理输入按键，返回 true 表示已消费
    pub fn input(&mut self, key: tui_textarea::Input) -> bool {
        self.inner.input(key)
    }

    /// 所有行用 "\n" 连接
    pub fn value(&self) -> String {
        self.inner.lines().join("\n")
    }

    /// 所有行用空格连接（单行模式下使用）
    pub fn single_line_value(&self) -> String {
        self.inner.lines().join(" ")
    }

    /// 设置值并移动光标到末尾
    pub fn set_value(&mut self, s: &str) {
        let lines: Vec<String> = if s.is_empty() {
            vec![String::new()]
        } else {
            s.split('\n').map(|l| l.to_string()).collect()
        };
        let mut new_inner = TextArea::new(lines);
        configure_style(&mut new_inner);
        new_inner.move_cursor(tui_textarea::CursorMove::End);
        self.inner = new_inner;
    }

    /// 所有行均为空
    pub fn is_empty(&self) -> bool {
        self.inner.lines().iter().all(|l| l.is_empty())
    }

    /// 渲染高度：行数 clamp 到 [1, max_lines]
    pub fn render_height(&self) -> u16 {
        let lines = self.inner.lines().len();
        lines.clamp(1, self.max_lines as usize) as u16
    }

    /// 光标移到末尾
    pub fn move_cursor_end(&mut self) {
        self.inner.move_cursor(tui_textarea::CursorMove::End);
    }

    /// 光标移到开头
    pub fn move_cursor_home(&mut self) {
        self.inner.move_cursor(tui_textarea::CursorMove::Head);
    }

    /// 清空内容
    pub fn clear(&mut self) {
        self.set_value("");
    }

    /// 在光标位置插入文本（用于粘贴）
    pub fn insert_text(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                self.inner.insert_newline();
            } else {
                self.inner.insert_char(c);
            }
        }
    }

    /// 渲染到 Frame
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(&self.inner, area);
    }

    /// 获取内部 TextArea 的可变引用（自定义样式用）
    pub fn inner_mut(&mut self) -> &mut TextArea<'static> {
        &mut self.inner
    }
}

impl Clone for FieldTextarea {
    fn clone(&self) -> Self {
        let lines: Vec<String> = self.inner.lines().iter().map(|s| s.to_string()).collect();
        let mut new_inner = TextArea::new(lines);
        configure_style(&mut new_inner);
        Self {
            inner: new_inner,
            max_lines: self.max_lines,
        }
    }
}

#[cfg(test)]
#[path = "field_textarea_test.rs"]
mod tests;
