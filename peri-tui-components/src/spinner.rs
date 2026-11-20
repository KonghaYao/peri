//! 无状态活动指示器——宿主提供帧集、帧序号与已格式化文字。
//!
//! [Why] 消息区底部与运行中工具图标各自按壁钟算帧、各自拼「图标 + 空格 + 文字」；
//! 组件只回答「这一帧是哪个字」并原样保留宿主写入的 span 样式，因此计时器、
//! 随机动词、耗时与 token 文案仍留在应用，固定输入即可复现同一行输出。

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// 单行活动指示器：`[帧图标][空格][宿主文字]`。
pub struct Spinner<'a> {
    frames: &'a [char],
    frame: u64,
    icon_style: Style,
    text: Line<'a>,
}

impl<'a> Spinner<'a> {
    /// `frame` 是宿主按时钟推进的帧序号，组件只做取模。
    #[must_use]
    pub fn new(frames: &'a [char], frame: u64) -> Self {
        Self {
            frames,
            frame,
            icon_style: Style::default(),
            text: Line::default(),
        }
    }

    /// 图标样式（宿主解析主题后传入）。
    #[must_use]
    pub fn icon_style(mut self, style: Style) -> Self {
        self.icon_style = style;
        self
    }

    /// 宿主已格式化的文字（动词、耗时、token 等），样式原样保留。
    #[must_use]
    pub fn text(mut self, text: Line<'a>) -> Self {
        self.text = text;
        self
    }

    /// 当前帧字符；帧集为空时没有图标（调用方自行降级）。
    #[must_use]
    pub fn glyph(&self) -> Option<char> {
        if self.frames.is_empty() {
            return None;
        }
        Some(self.frames[(self.frame % self.frames.len() as u64) as usize])
    }

    /// 拼装后的单行；没有帧集时只输出宿主文字。
    #[must_use]
    pub fn line(&self) -> Line<'a> {
        let Some(glyph) = self.glyph() else {
            return self.text.clone();
        };
        if self.text.spans.is_empty() {
            return Line::from(Span::styled(glyph.to_string(), self.icon_style));
        }
        let mut spans = Vec::with_capacity(self.text.spans.len() + 1);
        spans.push(Span::styled(format!("{glyph} "), self.icon_style));
        spans.extend(self.text.spans.iter().cloned());
        let mut line = Line::from(spans);
        line.style = self.text.style;
        line.alignment = self.text.alignment;
        line
    }
}

#[cfg(test)]
#[path = "spinner_test.rs"]
mod tests;
