//! Storybook app shell: layout, mouse routing, terminal lifecycle and dumps.

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::crossterm::cursor;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::stories::{
    ACCENT, BG, BORDER, STORIES, StoryState, accent_style, handle_story_mouse, muted_style,
    normal_style, render_story, selected_style,
};

#[derive(Clone, Copy)]
struct LayoutMetrics {
    /// 可见 story 行（含首行对应的 story 下标偏移）。
    story_rows: Rect,
    story_first: usize,
    /// 可见 variant 行（含首行对应的 variant 下标偏移）。
    variant_rows: Rect,
    variant_first: usize,
    story_area: Rect,
    /// Stories 面板整体（含边框），滚轮命中范围。
    nav_area: Rect,
    /// 最近一次绘制时 Stories 面板内容区高度与最大滚动行数。
    nav_height: u16,
    nav_max_scroll: usize,
}

impl Default for LayoutMetrics {
    fn default() -> Self {
        Self {
            story_rows: Rect::new(0, 0, 0, 0),
            story_first: 0,
            variant_rows: Rect::new(0, 0, 0, 0),
            variant_first: 0,
            story_area: Rect::new(0, 0, 0, 0),
            nav_area: Rect::new(0, 0, 0, 0),
            nav_height: 0,
            nav_max_scroll: 0,
        }
    }
}

struct App {
    story: usize,
    variant: usize,
    state: StoryState,
    metrics: LayoutMetrics,
    /// Stories 面板内容的滚动行数：面板放不下全部 story + variant 时由滚轮移动。
    nav_scroll: usize,
    /// 选择变化后请求下一次绘制把当前 variant 滚进视口（只揭示一次，避免抢走滚轮）。
    pending_reveal: bool,
}

impl App {
    fn new(story: usize) -> Self {
        let story = story.min(STORIES.len().saturating_sub(1));
        Self {
            story,
            variant: 0,
            state: (STORIES[story].variants[0].initial)(),
            metrics: LayoutMetrics::default(),
            nav_scroll: 0,
            pending_reveal: false,
        }
    }

    fn current(&self) -> &'static crate::stories::Story {
        &STORIES[self.story]
    }

    fn current_variant(&self) -> &'static crate::stories::Variant {
        &self.current().variants[self.variant]
    }

    fn reset_state(&mut self) {
        self.state = (self.current().variants[self.variant].initial)();
    }

    fn select_story(&mut self, index: usize) {
        if index >= STORIES.len() {
            return;
        }
        self.story = index;
        self.variant = 0;
        self.pending_reveal = true;
        self.reset_state();
    }

    fn select_variant(&mut self, index: usize) {
        if index >= self.current().variants.len() {
            return;
        }
        self.variant = index;
        self.pending_reveal = true;
        self.reset_state();
    }
}

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    let area = frame.area();
    if area.width < 40 || area.height < 10 {
        app.metrics = LayoutMetrics::default();
        Paragraph::new("Terminal too small: need at least 40x10")
            .style(accent_style())
            .render(area, frame.buffer_mut());
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, rows[0], app);
    let nav_width = area.width.saturating_sub(20).clamp(20, 30);
    let columns =
        Layout::horizontal([Constraint::Length(nav_width), Constraint::Min(0)]).split(rows[1]);
    draw_stories(frame, columns[0], app);
    draw_preview(frame, columns[1], app);
    draw_footer(frame, rows[2], app);
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(BG));
    Line::from(vec![
        Span::styled(
            " peri-tui storybook ",
            Style::new()
                .fg(BG)
                .bg(ACCENT)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} / {} ", app.story + 1, STORIES.len()),
            muted_style(),
        ),
    ])
    .render(area, buf);
    if area.width > 32 {
        Paragraph::new(" peri-tui-components ")
            .alignment(Alignment::Right)
            .style(muted_style())
            .render(area, buf);
    }
}

/// Stories 面板的虚拟行布局：story 行 → 空行 → `Variants` 标题 → variant 行。
struct NavLayout {
    story_count: usize,
    variant_title: usize,
    variant_first: usize,
    total: usize,
}

fn nav_layout(app: &App) -> NavLayout {
    let story_count = STORIES.len();
    let variant_title = story_count + 1;
    let variant_first = story_count + 2;
    NavLayout {
        story_count,
        variant_title,
        variant_first,
        total: variant_first + app.current().variants.len(),
    }
}

/// 把当前 variant 滚进视口。只在选择变化后执行一次，之后的滚轮位置由用户决定。
fn reveal_variant(app: &mut App, nav: &NavLayout) {
    let height = app.metrics.nav_height as usize;
    if height == 0 {
        return;
    }
    let target = nav.variant_first + app.variant;
    if target < app.nav_scroll {
        app.nav_scroll = target;
    } else if target >= app.nav_scroll + height {
        app.nav_scroll = target + 1 - height;
    }
}

fn draw_stories(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let block = Block::bordered()
        .border_style(BORDER)
        .title(" Stories ")
        .title_style(accent_style());
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    let nav = nav_layout(app);
    let height = inner.height as usize;
    let max_scroll = nav.total.saturating_sub(height);
    app.metrics.nav_area = area;
    app.metrics.nav_height = inner.height;
    app.metrics.nav_max_scroll = max_scroll;
    app.metrics.story_rows = Rect::new(0, 0, 0, 0);
    app.metrics.variant_rows = Rect::new(0, 0, 0, 0);
    if inner.width == 0 || height == 0 {
        return;
    }
    if app.pending_reveal {
        app.pending_reveal = false;
        reveal_variant(app, &nav);
    }
    app.nav_scroll = app.nav_scroll.min(max_scroll);

    let buf = frame.buffer_mut();
    let first = app.nav_scroll;
    let last = (first + height).min(nav.total);

    let story_end = last.min(nav.story_count);
    if first < story_end {
        app.metrics.story_rows =
            Rect::new(inner.x, inner.y, inner.width, (story_end - first) as u16);
        app.metrics.story_first = first;
        for (index, story) in STORIES.iter().enumerate().take(story_end).skip(first) {
            let selected = index == app.story;
            let marker = if selected { "▸ " } else { "  " };
            let text = format!("{marker}{:<18} {:>2}", story.title, story.variants.len());
            let style = if selected {
                selected_style()
            } else {
                normal_style()
            };
            let row = Rect::new(inner.x, inner.y + (index - first) as u16, inner.width, 1);
            buf.set_style(row, style);
            buf.set_string(row.x, row.y, &text, style);
        }
    }

    if first <= nav.variant_title && nav.variant_title < last {
        let y = inner.y + (nav.variant_title - first) as u16;
        Line::from(Span::styled(" Variants", accent_style()))
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
    }

    let variant_start = first.max(nav.variant_first);
    if variant_start < last {
        let variants = app.current().variants;
        app.metrics.variant_rows = Rect::new(
            inner.x,
            inner.y + (variant_start - first) as u16,
            inner.width,
            (last - variant_start) as u16,
        );
        app.metrics.variant_first = variant_start - nav.variant_first;
        for virtual_row in variant_start..last {
            let index = virtual_row - nav.variant_first;
            let selected = index == app.variant;
            let style = if selected {
                selected_style()
            } else {
                normal_style()
            };
            let row = Rect::new(
                inner.x,
                inner.y + (virtual_row - first) as u16,
                inner.width,
                1,
            );
            buf.set_style(row, style);
            buf.set_string(
                row.x,
                row.y,
                format!(" {} {}", index + 1, variants[index].key),
                style,
            );
        }
    }
}

fn draw_preview(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let story = app.current();
    let variant = app.current_variant();
    let block = Block::bordered()
        .border_style(BORDER)
        .title(format!(" {} · {} ", story.title, variant.key))
        .title_style(accent_style());
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let buf = frame.buffer_mut();
    buf.set_style(inner, Style::new().bg(BG).fg(crate::stories::FG));
    if inner.width == 0 || inner.height == 0 {
        app.metrics.story_area = Rect::new(0, 0, 0, 0);
        return;
    }
    Line::from(Span::styled(format!(" {}", variant.note), muted_style()))
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
    if inner.height < 2 {
        app.metrics.story_area = Rect::new(0, 0, 0, 0);
        return;
    }
    let story_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    app.metrics.story_area = story_area;
    render_story(&mut app.state, story_area, buf);
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(BG));
    let story = app.current();
    let variant = app.current_variant();
    let left = format!(
        " {}/{} · variant {}/{} ",
        story.key,
        variant.key,
        app.variant + 1,
        story.variants.len()
    );
    let right = " wheel: stories/preview · q quit ";
    let show_right = left.len() + right.len() <= area.width as usize;
    Paragraph::new(left).style(muted_style()).render(area, buf);
    if show_right {
        Paragraph::new(right)
            .alignment(Alignment::Right)
            .style(muted_style())
            .render(area, buf);
    }
}

fn row_index(rows: Rect, column: u16, row: u16) -> Option<usize> {
    if rows.width == 0 || rows.height == 0 {
        return None;
    }
    if column < rows.x || column >= rows.right() || row < rows.y || row >= rows.bottom() {
        return None;
    }
    Some((row - rows.y) as usize)
}

const NAV_WHEEL_ROWS: usize = 3;

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    // Stories 面板放不下全部条目时，滚轮移动面板内容。
    if crate::stories::rect_contains(app.metrics.nav_area, mouse.column, mouse.row) {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                // 指针在侧栏内，预览只清除按下态、不会滚动；先转发拿到取消语义，
                // 否则被消费的滚轮会让文档按钮停在按下样式。
                handle_story_mouse(&mut app.state, &mouse, app.metrics.story_area);
                app.nav_scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    app.nav_scroll.saturating_sub(NAV_WHEEL_ROWS)
                } else {
                    (app.nav_scroll + NAV_WHEEL_ROWS).min(app.metrics.nav_max_scroll)
                };
                return;
            }
            _ => {}
        }
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if let Some(offset) = row_index(app.metrics.story_rows, mouse.column, mouse.row) {
            app.select_story(app.metrics.story_first + offset);
            return;
        }
        if let Some(offset) = row_index(app.metrics.variant_rows, mouse.column, mouse.row) {
            app.select_variant(app.metrics.variant_first + offset);
            return;
        }
    }
    // 文档需要收到预览外的 Up 和点击；各 story 自己判断区域。
    handle_story_mouse(&mut app.state, &mouse, app.metrics.story_area);
}

/// Storybook navigation is mouse-only; keys either exit or edit the focused
/// text area, mirroring the host TUI's input behaviour.
fn handle_key(state: &mut StoryState, key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return true;
    }
    if key.code == KeyCode::Esc {
        return true;
    }
    if matches!(state, StoryState::TextArea { .. }) {
        forward_textarea_key(state, key);
        return false;
    }
    key.code == KeyCode::Char('q')
}

fn forward_textarea_key(state: &mut StoryState, key: KeyEvent) {
    let StoryState::TextArea { editor, .. } = state else {
        return;
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            'a' => editor.cursor_line_home(),
            'e' => editor.cursor_line_end(),
            'w' => editor.delete_word_backward(),
            'z' => {
                editor.undo();
            }
            'y' => {
                editor.redo();
            }
            _ => {}
        },
        KeyCode::Char(c) => editor.insert_char(c),
        KeyCode::Enter => editor.insert_char('\n'),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete_forward(),
        KeyCode::Left if ctrl => editor.cursor_word_left(),
        KeyCode::Right if ctrl => editor.cursor_word_right(),
        KeyCode::Left => editor.cursor_left(),
        KeyCode::Right => editor.cursor_right(),
        KeyCode::Up => {
            editor.cursor_line_up();
        }
        KeyCode::Down => {
            editor.cursor_line_down();
        }
        KeyCode::Home => editor.cursor_line_home(),
        KeyCode::End => editor.cursor_line_end(),
        KeyCode::Tab => editor.insert_str("    "),
        _ => {}
    }
}

struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            cursor::Show
        );
    }
}

pub(crate) fn run(start_story: usize, start_variant: usize) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    // 守卫必须早于任何可能失败的终端操作：`execute!` 失败时也要恢复
    // raw mode 与备用屏，否则 shell 会停留在异常终端状态。
    let _guard = RestoreGuard;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(start_story);
    app.select_variant(start_variant);

    loop {
        // 动画进度由 wall clock 决定；每轮都推进，否则连续到达的鼠标事件会让
        // `poll` 一直返回 true，播放表停在原地。
        crate::stories::streaming::tick_at(&mut app.state, Instant::now());
        terminal.draw(|frame| draw(frame, &mut app))?;
        // 只有 `[ auto ]` 播放中才缩短轮询间隔；进度完全由 wall clock 决定。
        let timeout = if crate::stories::streaming::is_animating(&app.state) {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(200)
        };
        if !event::poll(timeout)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if handle_key(&mut app.state, key) {
                    break;
                }
            }
            Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
            _ => {}
        }
    }
    Ok(())
}

fn dump_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("in-memory terminal");
    terminal
        .draw(|frame| draw(frame, app))
        .expect("in-memory draw");
    terminal.backend().buffer().clone()
}

fn dump_screen(app: &mut App, width: u16, height: u16) -> String {
    let buffer = dump_buffer(app, width, height);
    let mut lines = Vec::with_capacity(buffer.area.height as usize);
    for y in 0..buffer.area.height {
        let mut line = String::new();
        let mut wide_cells = 0u16;
        for x in 0..buffer.area.width {
            if wide_cells > 0 {
                wide_cells -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            wide_cells = UnicodeWidthStr::width(symbol).saturating_sub(1) as u16;
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

pub(crate) fn dump_one(story: usize, variant: usize, width: u16, height: u16) {
    let mut app = App::new(story);
    app.select_variant(variant);
    println!("{}", dump_screen(&mut app, width, height));
}

pub(crate) fn dump_all(width: u16, height: u16) {
    for (story_index, story) in STORIES.iter().enumerate() {
        for (variant_index, variant) in story.variants.iter().enumerate() {
            println!(
                "=== {} / {} ({}/{}, {}/{}) ===\n",
                story.key,
                variant.key,
                story_index + 1,
                STORIES.len(),
                variant_index + 1,
                story.variants.len()
            );
            dump_one(story_index, variant_index, width, height);
            if variant_index + 1 < story.variants.len() {
                println!();
            }
        }
    }
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
