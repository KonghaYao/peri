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

/// 渲染整份 Mermaid 源码。小图先用紧凑间距与严格列宽，超宽时退回常规压缩
/// 一次；大图先给语义概览，避免先排一遍注定放不下的全图。
pub(crate) fn render_source(source: &str, width: usize) -> Result<RenderOutput, RenderFailure> {
    let overview = super::overview::build_unchecked(source).ok();
    if let Some(overview) = overview.as_ref()
        && overview.prefers_overview()
    {
        return Ok(overview_output(overview, width));
    }
    let full = render_layout(source, width)?;
    if let Some(overview) = overview.as_ref()
        && full.lines().len() > super::overview::OVERVIEW_ROW_BUDGET
    {
        // 排版后才发现高出内容区：交给同源的语义摘要，不在消息里铺 500 行。
        return Ok(overview_output(overview, width));
    }
    Ok(full)
}

fn overview_output(overview: &super::overview::Overview, width: usize) -> RenderOutput {
    RenderOutput {
        lines: Arc::new(overview.lines(width, super::overview::OVERVIEW_ROW_BUDGET)),
    }
}

fn render_layout(source: &str, width: usize) -> Result<RenderOutput, RenderFailure> {
    let options = RenderOptions {
        max_width: (width > 0).then_some(width),
        max_width_strict: true,
        gaps_override: Some((1, 0)),
        ..RenderOptions::default()
    };
    let rendered = match mermaid_text::render_with_options(source, &options) {
        Ok(rendered) => rendered,
        Err(mermaid_text::Error::TooWide { .. }) => {
            // 紧凑间距仍放不下：常规压缩只尝试一次，之后交给消息区折行。
            let compressed = RenderOptions {
                max_width: (width > 0).then_some(width),
                max_width_strict: false,
                gaps_override: None,
                ..RenderOptions::default()
            };
            mermaid_text::render_with_options(source, &compressed)
                .map_err(|error| RenderFailure::new(error.to_string()))?
        }
        Err(error) => return Err(RenderFailure::new(error.to_string())),
    };
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

/// 详情面板用的完整布局：不经过概览裁剪，保留原始宽高供双轴滚动。
pub(crate) fn render_full(source: &str, width: usize) -> Result<RenderOutput, RenderFailure> {
    render_layout(source, width)
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
