pub mod animation;
pub mod verb;

use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, WidgetRef},
};

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpinnerMode {
    Thinking,
    ToolUse,
    Responding,
    Idle,
}

pub struct SpinnerState {
    mode: SpinnerMode,
    verb: String,
    start_time: Instant,
    token_count: usize,
    /// 最后一次从非 Idle 切换到 Idle 时捕获的耗时（ms），0 表示无记录
    last_summary_elapsed_ms: u64,
}

impl SpinnerState {
    pub fn new(mode: SpinnerMode) -> Self {
        Self {
            mode,
            verb: verb::pick_verb(None),
            start_time: Instant::now(),
            token_count: 0,
            last_summary_elapsed_ms: 0,
        }
    }

    pub fn set_mode(&mut self, mode: SpinnerMode) {
        let was_active = self.mode != SpinnerMode::Idle;
        self.mode = mode;
        self.verb = match &self.mode {
            SpinnerMode::Thinking => "思考中…".to_string(),
            SpinnerMode::ToolUse => "执行工具…".to_string(),
            SpinnerMode::Responding => "正在生成回复…".to_string(),
            SpinnerMode::Idle => String::new(),
        };
        // 从活跃状态切换到 Idle 时，记录耗时用于总结行
        if was_active && self.mode == SpinnerMode::Idle {
            self.last_summary_elapsed_ms = self.elapsed_ms();
        }
        // 从 Idle 切换到活跃状态时，重置计时器和总结记录
        if !was_active && self.mode != SpinnerMode::Idle {
            self.start_time = Instant::now();
            self.last_summary_elapsed_ms = 0;
        }
    }

    pub fn set_verb(&mut self, active_form: Option<&str>) {
        self.verb = verb::pick_verb(active_form);
    }

    pub fn set_token_count(&mut self, count: usize) {
        self.token_count = count;
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn verb(&self) -> &str {
        &self.verb
    }

    pub fn mode(&self) -> &SpinnerMode {
        &self.mode
    }

    pub fn last_summary_elapsed_ms(&self) -> u64 {
        self.last_summary_elapsed_ms
    }

    /// 当前原始 token 计数（未经平滑追赶，等于最近一次 set_token_count 的值）。
    pub fn token_count(&self) -> usize {
        self.token_count
    }

    /// 重置所有字段到初始状态
    pub fn reset(&mut self) {
        self.mode = SpinnerMode::Idle;
        self.verb = String::new();
        self.start_time = Instant::now();
        self.token_count = 0;
        self.last_summary_elapsed_ms = 0;
    }

    /// 将 spinner 渲染为 Vec<Line>，供 TUI 消息区直接追加到 all_lines 中。
    ///
    /// 与 WidgetRef::render_ref 渲染逻辑一致，但不依赖 Buffer——产出纯数据 Line。
    ///
    /// `token_count` 由调用方从外部 atom（如 SPINNER_TOKEN_COUNT）读取后传入。
    /// 本方法完全无副作用——frame 索引基于 `start_time.elapsed()` 纯计算，
    /// 不读取也不写入任何动画驱动 state，可在 render body 中安全调用。
    pub fn render_to_lines(
        &self,
        primary: Color,
        secondary: Color,
        show_elapsed: bool,
        show_tokens: bool,
        token_count: usize,
    ) -> Vec<Line<'static>> {
        // 帧索引纯计算：50ms 一个 raw tick，每 2 raw tick 推进一帧。
        // 保留原 advance_tick 节奏（每帧 ~100ms）。
        let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
        let raw_tick = elapsed_ms / 50;
        let frame_tick = raw_tick / 2;
        let frame = animation::tick_to_frame(frame_tick);
        let mut spans: Vec<Span<'static>> = vec![];

        spans.push(Span::styled(
            format!("{} ", frame),
            Style::default().fg(primary),
        ));
        spans.push(Span::styled(
            self.verb().to_string(),
            Style::default().fg(primary),
        ));

        let mut suffix_parts = Vec::new();
        if show_elapsed {
            suffix_parts.push(animation::format_elapsed(self.elapsed_ms()));
        }
        if show_tokens && token_count > 0 {
            suffix_parts.push(format!(
                "↓ {} tokens",
                animation::format_tokens(token_count)
            ));
        }
        if !suffix_parts.is_empty() {
            spans.push(Span::styled(
                format!(" ({}", suffix_parts.join(" · ")),
                Style::default().fg(secondary),
            ));
            spans.push(Span::styled(")", Style::default().fg(secondary)));
        }

        vec![Line::from(spans)]
    }
}

pub struct SpinnerWidget<'a> {
    state: &'a SpinnerState,
    show_elapsed: bool,
    show_tokens: bool,
    token_count: usize,
    primary_color: Color,
    secondary_color: Color,
}

impl<'a> SpinnerWidget<'a> {
    pub fn new(state: &'a SpinnerState) -> Self {
        Self {
            state,
            show_elapsed: true,
            show_tokens: true,
            token_count: 0,
            primary_color: Color::Rgb(215, 119, 87), // ACCENT #D77757
            secondary_color: Color::Rgb(153, 153, 153), // MUTED #999999
        }
    }

    pub fn show_elapsed(mut self, show: bool) -> Self {
        self.show_elapsed = show;
        self
    }

    pub fn show_tokens(mut self, show: bool) -> Self {
        self.show_tokens = show;
        self
    }

    /// 设置当前 token 计数（替代旧的 SpinnerState::set_token_count）。
    /// Widget 在 render 时把这个值传给 render_to_lines。
    pub fn token_count(mut self, count: usize) -> Self {
        self.token_count = count;
        self
    }

    pub fn theme_colors(mut self, primary: Color, secondary: Color) -> Self {
        self.primary_color = primary;
        self.secondary_color = secondary;
        self
    }

    /// 从 `Theme` trait 派生 spinner 颜色，替代硬编码默认值。
    pub fn with_theme(mut self, theme: &dyn Theme) -> Self {
        self.primary_color = theme.accent();
        self.secondary_color = theme.muted();
        self
    }
}

impl WidgetRef for SpinnerWidget<'_> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.state.render_to_lines(
            self.primary_color,
            self.secondary_color,
            self.show_elapsed,
            self.show_tokens,
            self.token_count,
        );
        Paragraph::new(lines.into_iter().next().unwrap_or_default()).render(area, buf);
    }
}

impl Widget for SpinnerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_ref(area, buf);
    }
}
