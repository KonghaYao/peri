//! Mermaid 终稿渲染：纯函数，输入是整份源码与正文列宽。
//!
//! [Why 每次整份输入] 上游 0.58.0 没有增量解析；增量识别由 [`super::stream`] 持有，
//! 这里只负责把「已经可信的整段前缀」确定性排版成字符图。

use std::sync::Arc;

use mermaid_text::RenderOptions;

use super::stream::{DiagramMode, validate_final};

/// 排版失败。`reason` 已压缩成单行短文案，直接展示给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderFailure {
    reason: String,
}

impl RenderFailure {
    #[must_use]
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        let reason: String = reason.into();
        Self {
            reason: truncate_reason(&reason),
        }
    }

    #[must_use]
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
}

impl std::fmt::Display for RenderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

/// 一份可发布的字符图。行文本保持上游输出，不做额外着色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderOutput {
    lines: Arc<Vec<String>>,
}

impl RenderOutput {
    #[must_use]
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    #[must_use]
    pub(crate) fn from_lines(lines: Arc<Vec<String>>) -> Self {
        Self { lines }
    }

    #[must_use]
    pub(crate) fn into_lines(self) -> Arc<Vec<String>> {
        self.lines
    }
}

/// 渲染整份 Mermaid 源码。宽度作为软预算交给上游压缩，超宽由消息区折行兜底。
pub(crate) fn render_source(source: &str, width: usize) -> Result<RenderOutput, RenderFailure> {
    let options = RenderOptions {
        max_width: (width > 0).then_some(width),
        ..RenderOptions::default()
    };
    let rendered = mermaid_text::render_with_options(source, &options)
        .map_err(|error| RenderFailure::new(error.to_string()))?;
    let trimmed = rendered.trim_end_matches('\n');
    let lines: Vec<String> = trimmed.lines().map(str::to_string).collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return Err(RenderFailure::new("rendered diagram is empty"));
    }
    Ok(RenderOutput {
        lines: Arc::new(lines),
    })
}

/// 闭合终稿：先按图型验收完整语义，再排版。验收失败时不产生可能误导的图。
pub(crate) fn render_final(
    source: &str,
    mode: DiagramMode,
    width: usize,
) -> Result<RenderOutput, RenderFailure> {
    if let Err(reason) = validate_final(source, mode) {
        return Err(RenderFailure::new(reason));
    }
    render_source(source, width)
}

fn truncate_reason(reason: &str) -> String {
    let single_line = reason.replace(['\n', '\r'], " ");
    let mut truncated: String = single_line.chars().take(160).collect();
    if single_line.chars().count() > 160 {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
#[path = "frame_test.rs"]
mod tests;
