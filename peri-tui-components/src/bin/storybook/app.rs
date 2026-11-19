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
    story_rows: Rect,
    variant_rows: Rect,
    story_area: Rect,
}

impl Default for LayoutMetrics {
    fn default() -> Self {
        Self {
            story_rows: Rect::new(0, 0, 0, 0),
            variant_rows: Rect::new(0, 0, 0, 0),
            story_area: Rect::new(0, 0, 0, 0),
        }
    }
}

struct App {
    story: usize,
    variant: usize,
    state: StoryState,
    metrics: LayoutMetrics,
}

impl App {
    fn new(story: usize) -> Self {
        let story = story.min(STORIES.len().saturating_sub(1));
        Self {
            story,
            variant: 0,
            state: (STORIES[story].variants[0].initial)(),
            metrics: LayoutMetrics::default(),
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
        self.reset_state();
    }

    fn select_variant(&mut self, index: usize) {
        if index >= self.current().variants.len() {
            return;
        }
        self.variant = index;
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

fn draw_stories(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let block = Block::bordered()
        .border_style(BORDER)
        .title(" Stories ")
        .title_style(accent_style());
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.width == 0 || inner.height == 0 {
        app.metrics.story_rows = Rect::new(0, 0, 0, 0);
        app.metrics.variant_rows = Rect::new(0, 0, 0, 0);
        return;
    }

    let story_rows_height = (STORIES.len() as u16).min(inner.height);
    app.metrics.story_rows = Rect::new(inner.x, inner.y, inner.width, story_rows_height);
    let buf = frame.buffer_mut();
    for (index, story) in STORIES.iter().enumerate().take(story_rows_height as usize) {
        let selected = index == app.story;
        let marker = if selected { "▸ " } else { "  " };
        let text = format!("{marker}{:<18} {:>2}", story.title, story.variants.len());
        let style = if selected {
            selected_style()
        } else {
            normal_style()
        };
        let row = Rect::new(inner.x, inner.y + index as u16, inner.width, 1);
        buf.set_style(row, style);
        buf.set_string(row.x, row.y, &text, style);
    }

    app.metrics.variant_rows = Rect::new(0, 0, 0, 0);
    let title_y = inner.y.saturating_add(story_rows_height).saturating_add(1);
    if title_y >= inner.bottom() {
        return;
    }
    Line::from(Span::styled(" Variants", accent_style()))
        .render(Rect::new(inner.x, title_y, inner.width, 1), buf);
    let variants_y = title_y.saturating_add(1);
    if variants_y >= inner.bottom() {
        return;
    }
    let variants = app.current().variants;
    let variants_height = (variants.len() as u16).min(inner.bottom().saturating_sub(variants_y));
    if variants_height == 0 {
        return;
    }
    app.metrics.variant_rows = Rect::new(inner.x, variants_y, inner.width, variants_height);
    for (index, variant) in variants.iter().enumerate().take(variants_height as usize) {
        let selected = index == app.variant;
        let style = if selected {
            selected_style()
        } else {
            normal_style()
        };
        let row = Rect::new(inner.x, variants_y + index as u16, inner.width, 1);
        buf.set_style(row, style);
        buf.set_string(
            row.x,
            row.y,
            format!(" {} {}", index + 1, variant.key),
            style,
        );
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
    let right = " wheel/click in preview · q quit ";
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

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if let Some(index) = row_index(app.metrics.story_rows, mouse.column, mouse.row) {
            app.select_story(index);
            return;
        }
        if let Some(index) = row_index(app.metrics.variant_rows, mouse.column, mouse.row) {
            app.select_variant(index);
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
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
    let _guard = RestoreGuard;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(start_story);
    app.select_variant(start_variant);

    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;
        // 只有 `[ auto ]` 播放中才缩短轮询间隔；进度完全由 wall clock 决定。
        let timeout = if crate::stories::streaming::is_animating(&app.state) {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(200)
        };
        if !event::poll(timeout)? {
            crate::stories::streaming::tick_at(&mut app.state, Instant::now());
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
