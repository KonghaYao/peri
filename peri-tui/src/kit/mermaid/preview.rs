//! 流式预览状态机：可信前缀、最后可信帧、80ms 提交节流与版本校验。
//!
//! 这个模块不碰线程与 UI；全局 store 负责把 [`PreviewEngine::take_due`] 的结果
//! 交给后台任务，完成后用 [`PreviewEngine::complete`] 回传。只有仍然匹配会话、
//! 图实例、前缀版本与布局参数的结果才会发布。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::markdown::{MermaidFence, MermaidFenceKey};

use super::frame::{RenderFailure, RenderOutput, render_final, render_source};
use super::stream::{DiagramMode, MermaidStream, detect_mode};

/// 连续输入时最多每 80ms 提交一次新的可信前缀。
pub(crate) const COALESCE: Duration = Duration::from_millis(80);

const MAX_STATES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    Preview,
    Final,
}

#[derive(Debug)]
pub(crate) struct RenderJob {
    pub(crate) key: MermaidFenceKey,
    pub(crate) kind: JobKind,
    pub(crate) mode: DiagramMode,
    pub(crate) source: Arc<String>,
    pub(crate) width: usize,
    version: u64,
    epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreviewNotice {
    Unclosed,
    Interrupted(String),
    Failed(String),
    UnknownStatement,
    OpenScope { label: String, statements: usize },
}

#[derive(Debug, Clone)]
pub(crate) enum PreviewContent {
    /// 没有可用帧：调用方继续展示完整源码。
    Source,
    Frame {
        lines: Arc<Vec<String>>,
        /// 尚未确认的源码尾部，显示在图下方。
        tail: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewView {
    pub(crate) content: PreviewContent,
    pub(crate) notice: Option<PreviewNotice>,
}

#[derive(Debug, Clone)]
struct FrameState {
    lines: Arc<Vec<String>>,
    width: usize,
    source: Arc<String>,
}

#[derive(Debug)]
struct PendingJob {
    due: Instant,
    kind: JobKind,
    mode: DiagramMode,
    source: Arc<String>,
    width: usize,
    version: u64,
    epoch: u64,
}

#[derive(Debug)]
struct RunningJob {
    kind: JobKind,
    version: u64,
    epoch: u64,
}

#[derive(Debug)]
struct FenceState {
    accessed: u64,
    mode: DiagramMode,
    stream: MermaidStream,
    source_seen: String,
    width: usize,
    closed: bool,
    frozen: bool,
    epoch: u64,
    version: u64,
    trusted: String,
    trusted_end: usize,
    drawable: bool,
    paused: Option<String>,
    open_scope: Option<(String, usize)>,
    frame: Option<FrameState>,
    final_frame: Option<FrameState>,
    pending: Option<PendingJob>,
    running: Option<RunningJob>,
    last_commit: Option<Instant>,
    final_input: Option<(usize, String)>,
    error: Option<String>,
    error_input: Option<(usize, String)>,
}

impl Default for FenceState {
    fn default() -> Self {
        Self {
            accessed: 0,
            mode: DiagramMode::Unknown,
            stream: MermaidStream::new(),
            source_seen: String::new(),
            width: 0,
            closed: false,
            frozen: false,
            epoch: 0,
            version: 0,
            trusted: String::new(),
            trusted_end: 0,
            drawable: false,
            paused: None,
            open_scope: None,
            frame: None,
            final_frame: None,
            pending: None,
            running: None,
            last_commit: None,
            final_input: None,
            error: None,
            error_input: None,
        }
    }
}

impl FenceState {
    fn reset(&mut self, epoch: u64) {
        let accessed = self.accessed;
        *self = Self {
            accessed,
            epoch,
            ..Self::default()
        };
    }

    fn clear_pending(&mut self) {
        self.pending = None;
    }

    fn schedule_preview(&mut self, source: &str, width: usize, now: Instant) {
        let source = Arc::new(source.to_string());
        if self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.width == width && frame.source == source)
        {
            self.pending = None;
            return;
        }
        let due = self.last_commit.map_or(now, |last| {
            last.checked_add(COALESCE).unwrap_or(now).max(now)
        });
        self.pending = Some(PendingJob {
            due,
            kind: JobKind::Preview,
            mode: self.mode,
            source,
            width,
            version: self.version,
            epoch: self.epoch,
        });
    }

    fn schedule_final(&mut self, source: &str, width: usize, now: Instant) {
        let source = Arc::new(source.to_string());
        if self
            .final_frame
            .as_ref()
            .is_some_and(|frame| frame.width == width && frame.source.as_str() == source.as_str())
        {
            self.pending = None;
            return;
        }
        self.pending = Some(PendingJob {
            due: now,
            kind: JobKind::Final,
            mode: self.mode,
            source,
            width,
            version: self.version,
            epoch: self.epoch,
        });
    }

    fn clear_stale_error(&mut self, input: &str, width: usize) {
        if self
            .error_input
            .as_ref()
            .is_some_and(|(error_width, error_source)| {
                *error_width == width && error_source == input
            })
        {
            return;
        }
        self.error = None;
        self.error_input = None;
    }

    fn set_error(&mut self, reason: String, width: usize, source: &str) {
        self.error = Some(reason);
        self.error_input = Some((width, source.to_string()));
    }

    fn view(&self, source: &str) -> PreviewView {
        let content = if let Some(frame) = &self.final_frame {
            PreviewContent::Frame {
                lines: Arc::clone(&frame.lines),
                tail: String::new(),
            }
        } else if let Some(frame) = &self.frame {
            let tail = if self.trusted_end <= source.len() {
                source[self.trusted_end..].to_string()
            } else {
                String::new()
            };
            PreviewContent::Frame {
                lines: Arc::clone(&frame.lines),
                tail,
            }
        } else {
            PreviewContent::Source
        };
        let notice = if let Some(reason) = &self.error {
            Some(PreviewNotice::Failed(reason.clone()))
        } else if self.paused.is_some() {
            Some(PreviewNotice::UnknownStatement)
        } else if !self.closed && self.frozen {
            Some(PreviewNotice::Unclosed)
        } else {
            self.open_scope
                .as_ref()
                .map(|(label, statements)| PreviewNotice::OpenScope {
                    label: label.clone(),
                    statements: *statements,
                })
        };
        PreviewView { content, notice }
    }
}

/// 预览状态机。测试可直接用注入的 `Instant` 驱动它，不依赖线程与真实时间。
#[derive(Debug, Default)]
pub(crate) struct PreviewEngine {
    states: HashMap<MermaidFenceKey, FenceState>,
    clock: u64,
    revision: u64,
}

impl PreviewEngine {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn pending_count(&self) -> usize {
        self.states
            .values()
            .filter(|state| state.pending.is_some())
            .count()
    }

    /// 观察一个围栏的最新正文，返回当前可展示的帧与提示。
    pub(crate) fn observe(
        &mut self,
        key: MermaidFenceKey,
        fence: &MermaidFence,
        width: usize,
        frozen: bool,
        now: Instant,
    ) -> PreviewView {
        self.clock = self.clock.wrapping_add(1);
        if self.states.len() >= MAX_STATES && !self.states.contains_key(&key) {
            self.evict_one();
        }
        let clock = self.clock;
        let state = self.states.entry(key.clone()).or_default();
        state.accessed = clock;
        state.frozen = frozen;
        let source = fence.source.as_str();
        if !state.source_seen.is_empty() && !source.starts_with(state.source_seen.as_str()) {
            let epoch = state.epoch.wrapping_add(1);
            state.reset(epoch);
        }
        if state.source_seen.as_str() != source {
            state.source_seen.clear();
            state.source_seen.push_str(source);
        }
        let closed = fence.closed();
        let reopened = state.closed && !closed;
        state.closed = closed;
        if reopened {
            // 暂定闭合被后续 token 延长时，上一张终稿仍是最后可信帧。
            state.frame = state.final_frame.take().or_else(|| state.frame.take());
            state.final_input = None;
            state.error = None;
            state.error_input = None;
            state.version = state.version.wrapping_add(1);
        }
        if state.mode == DiagramMode::Unknown {
            state.mode = detect_mode(source, closed);
        }
        let width_changed = state.width != width;
        state.width = width;
        if width_changed {
            // 布局参数变化不等 80ms 节流；它是重排，不是新的流式前缀。
            state.last_commit = None;
        }

        if state.mode.streams() {
            if closed {
                state.clear_pending();
                state.paused = None;
                state.open_scope = None;
                state.clear_stale_error(source, width);
                if state.error.is_none() {
                    if state
                        .final_input
                        .as_ref()
                        .is_none_or(|(w, s)| *w != width || s != source)
                    {
                        state.version = state.version.wrapping_add(1);
                        state.final_input = Some((width, source.to_string()));
                    }
                    state.schedule_final(source, width, now);
                }
            } else {
                let update = state.stream.observe(source, false);
                let trusted = update.trusted.to_string();
                let trusted_end = update.trusted_end;
                let drawable = update.drawable;
                let paused = update.paused.map(str::to_string);
                let open_scope = update
                    .open_scope
                    .map(|scope| (scope.label.to_string(), scope.statements));
                let is_paused = update.paused.is_some();
                let trusted_changed = trusted != state.trusted || drawable != state.drawable;
                state.trusted = trusted;
                state.trusted_end = trusted_end;
                state.drawable = drawable;
                state.paused = paused;
                state.open_scope = open_scope;
                let trusted_input = state.trusted.clone();
                state.clear_stale_error(&trusted_input, width);
                if is_paused {
                    state.clear_pending();
                } else if (trusted_changed || width_changed) && state.drawable {
                    state.version = state.version.wrapping_add(1);
                    state.schedule_preview(&trusted_input, width, now);
                }
            }
        } else if state.mode.renders_when_closed() && closed {
            state.clear_pending();
            state.paused = None;
            state.open_scope = None;
            state.clear_stale_error(source, width);
            if state.error.is_none() {
                if state
                    .final_input
                    .as_ref()
                    .is_none_or(|(w, s)| *w != width || s != source)
                {
                    state.version = state.version.wrapping_add(1);
                    state.final_input = Some((width, source.to_string()));
                }
                state.schedule_final(source, width, now);
            }
        } else {
            state.clear_pending();
        }

        state.view(source)
    }

    /// 取走一个到期任务并标记为运行中。运行中的图实例不会再入队。
    pub(crate) fn take_due(&mut self, now: Instant) -> Option<RenderJob> {
        let mut chosen: Option<(MermaidFenceKey, Instant)> = None;
        for (key, state) in &self.states {
            if state.running.is_some() {
                continue;
            }
            if let Some(pending) = &state.pending
                && pending.due <= now
                && chosen
                    .as_ref()
                    .is_none_or(|(_, earliest)| pending.due < *earliest)
            {
                chosen = Some((key.clone(), pending.due));
            }
        }
        let (key, _) = chosen?;
        let state = self
            .states
            .get_mut(&key)
            .expect("the selected key belongs to the engine state map");
        let pending = state
            .pending
            .take()
            .expect("the selected state has a pending job");
        state.running = Some(RunningJob {
            kind: pending.kind,
            version: pending.version,
            epoch: pending.epoch,
        });
        Some(RenderJob {
            key,
            kind: pending.kind,
            mode: pending.mode,
            source: pending.source,
            width: pending.width,
            version: pending.version,
            epoch: pending.epoch,
        })
    }

    /// 距离下一个到期任务还有多久；无任务时返回 `None`。
    #[must_use]
    pub(crate) fn wait_duration(&self, now: Instant) -> Option<Duration> {
        self.states
            .values()
            .filter(|state| state.running.is_none())
            .filter_map(|state| state.pending.as_ref())
            .map(|pending| pending.due.saturating_duration_since(now))
            .min()
    }

    /// 回传排版结果。版本、列宽、闭合状态或实例已变化时返回 `false` 并丢弃。
    pub(crate) fn complete(
        &mut self,
        job: RenderJob,
        result: Result<RenderOutput, RenderFailure>,
        now: Instant,
    ) -> bool {
        let Some(state) = self.states.get_mut(&job.key) else {
            return false;
        };
        let Some(running) = state.running.as_ref() else {
            return false;
        };
        if running.version != job.version || running.epoch != job.epoch || running.kind != job.kind
        {
            return false;
        }
        state.running = None;
        let current = state.epoch == job.epoch
            && state.width == job.width
            && state.version == job.version
            && match job.kind {
                JobKind::Preview => !state.closed,
                JobKind::Final => state.closed && state.source_seen.as_str() == job.source.as_str(),
            };
        if !current {
            return false;
        }
        match result {
            Ok(output) => {
                let frame = FrameState {
                    lines: output.into_lines(),
                    width: job.width,
                    source: Arc::clone(&job.source),
                };
                match job.kind {
                    JobKind::Preview => {
                        state.frame = Some(frame);
                        state.last_commit = Some(now);
                        if let Some(pending) = &mut state.pending {
                            let earliest = now.checked_add(COALESCE).unwrap_or(now);
                            if pending.due < earliest {
                                pending.due = earliest;
                            }
                        }
                    }
                    JobKind::Final => {
                        state.final_frame = Some(frame);
                        state.pending = None;
                    }
                }
                state.error = None;
                state.error_input = None;
                self.revision = self.revision.wrapping_add(1);
                true
            }
            Err(error) => {
                let reason = error.reason().to_string();
                if job.kind == JobKind::Final {
                    state.frame = None;
                    state.final_frame = None;
                    state.clear_pending();
                }
                state.set_error(reason, job.width, job.source.as_str());
                self.revision = self.revision.wrapping_add(1);
                true
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn view(&self, key: &MermaidFenceKey) -> Option<PreviewView> {
        let state = self.states.get(key)?;
        Some(state.view(&state.source_seen))
    }

    fn evict_one(&mut self) {
        let key = self
            .states
            .iter()
            .filter(|(_, state)| state.running.is_none())
            .min_by_key(|(_, state)| state.accessed)
            .map(|(key, _)| key.clone());
        if let Some(key) = key {
            self.states.remove(&key);
        }
    }
}

/// 历史重建：闭合正文复用实时终稿的纯函数；前半段重建最后可信帧并标记未闭合。
pub(crate) struct HistoryReplay {
    #[cfg(test)]
    pub(crate) mode: DiagramMode,
    pub(crate) content: PreviewContent,
    pub(crate) notice: Option<PreviewNotice>,
}

pub(crate) fn rebuild_history(
    fence: &MermaidFence,
    width: usize,
    interrupted: Option<&str>,
) -> HistoryReplay {
    let source = fence.source.as_str();
    let mode = detect_mode(source, fence.closed());
    let mut replay = HistoryReplay {
        #[cfg(test)]
        mode,
        content: PreviewContent::Source,
        notice: None,
    };
    if !mode.renders_when_closed() {
        return replay;
    }
    if fence.closed() {
        match render_final(source, mode, width) {
            Ok(output) => {
                replay.content = PreviewContent::Frame {
                    lines: output.into_lines(),
                    tail: String::new(),
                };
            }
            Err(error) => {
                replay.notice = Some(PreviewNotice::Failed(error.reason().to_string()));
            }
        }
        return replay;
    }
    if !mode.streams() {
        return replay;
    }
    let mut stream = MermaidStream::new();
    let update = stream.observe(source, false);
    replay.notice = Some(match interrupted {
        Some(reason) => PreviewNotice::Interrupted(reason.to_string()),
        None => PreviewNotice::Unclosed,
    });
    if update.paused.is_some() {
        replay.notice = Some(PreviewNotice::UnknownStatement);
        return replay;
    }
    if !update.drawable {
        return replay;
    }
    match render_source(update.trusted, width) {
        Ok(output) => {
            let tail = source[update.trusted_end..].to_string();
            replay.content = PreviewContent::Frame {
                lines: output.into_lines(),
                tail,
            };
        }
        Err(error) => {
            replay.notice = Some(PreviewNotice::Failed(error.reason().to_string()));
        }
    }
    replay
}

#[cfg(test)]
#[path = "preview_test.rs"]
mod tests;
