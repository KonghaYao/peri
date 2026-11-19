use crate::{ModelError, ModelResult, ProtocolErrorKind};

/// SSE 解码预算。单行与单事件独立计费，避免异常 provider 用超长行或永不结束的
/// 多行事件无界占用内存；事件预算按 data 行累计，并计入每行结构开销。
pub(crate) const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// 已完成的 SSE event。`data` 保持 provider 原文，JSON 解码由 adapter 负责。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

/// 字节级 SSE parser，安全保留跨 chunk 的 UTF-8 与未完成行。
pub(crate) struct SseParser {
    pending_bytes: Vec<u8>,
    event: Option<String>,
    data_lines: Vec<String>,
    data_bytes: usize,
    saw_data_field: bool,
    done: bool,
    failed: bool,
}

impl SseParser {
    pub(crate) fn new() -> Self {
        Self {
            pending_bytes: Vec::new(),
            event: None,
            data_lines: Vec::new(),
            data_bytes: 0,
            saw_data_field: false,
            done: false,
            failed: false,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<ModelResult<SseEvent>> {
        if self.done || self.failed {
            return Vec::new();
        }
        self.pending_bytes.extend_from_slice(bytes);
        let Some(complete_end) = self
            .pending_bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
        else {
            if self.pending_line_exceeds_budget() {
                return vec![Err(self.budget_exceeded("line", MAX_SSE_LINE_BYTES))];
            }
            return Vec::new();
        };

        let remaining = self.pending_bytes.split_off(complete_end);
        let complete = std::mem::replace(&mut self.pending_bytes, remaining);
        let mut events = Vec::new();
        // 按行解码，后续坏字节不能撤销已经收齐的事件；HTTP 分块不改变事件顺序。
        for raw_line in complete.split_inclusive(|byte| *byte == b'\n') {
            // 先按字节判定，避免超大坏行在 UTF-8 诊断时再分配一份等长缓冲。
            let terminator_len = if raw_line.ends_with(b"\r\n") { 2 } else { 1 };
            if raw_line.len() - terminator_len > MAX_SSE_LINE_BYTES {
                events.push(Err(self.budget_exceeded("line", MAX_SSE_LINE_BYTES)));
                break;
            }
            let raw_line = match std::str::from_utf8(raw_line) {
                Ok(line) => line,
                Err(error) => {
                    events.push(Err(ModelError::protocol(
                        crate::ProtocolErrorKind::Provider,
                    )
                    .with_error(&error)
                    .with_body(String::from_utf8_lossy(raw_line))));
                    break;
                }
            };
            let line = raw_line
                .strip_suffix('\n')
                .expect("split_inclusive always includes newline")
                .strip_suffix('\r')
                .unwrap_or(raw_line.strip_suffix('\n').expect("newline removed"));
            if line.is_empty() {
                if self.saw_data_field {
                    events.push(Ok(SseEvent {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data_lines).join("\n"),
                    }));
                    self.data_bytes = 0;
                    self.saw_data_field = false;
                } else {
                    self.event = None;
                }
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                let value = value.strip_prefix(' ').unwrap_or(value);
                if value == "[DONE]" {
                    self.done = true;
                    break;
                }
                let separator = usize::from(!self.data_lines.is_empty());
                // 空 data 行也持有 String 结构；计入结构大小，避免靠空行绕过预算。
                let line_cost = value.len() + separator + std::mem::size_of::<String>();
                if self.data_bytes + line_cost > MAX_SSE_EVENT_BYTES {
                    events.push(Err(self.budget_exceeded("event", MAX_SSE_EVENT_BYTES)));
                    break;
                }
                self.data_bytes += line_cost;
                self.saw_data_field = true;
                self.data_lines.push(value.to_owned());
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = Some(value.strip_prefix(' ').unwrap_or(value).to_owned());
            }
        }
        if !self.done && !self.failed && self.pending_line_exceeds_budget() {
            events.push(Err(self.budget_exceeded("line", MAX_SSE_LINE_BYTES)));
        }
        events
    }

    /// 行尾 `\r` 可能属于尚未收齐的 CRLF 终止符，不计入内容预算，保证判定与分块无关。
    fn pending_line_exceeds_budget(&self) -> bool {
        let content_bytes =
            self.pending_bytes.len() - usize::from(self.pending_bytes.ends_with(b"\r"));
        content_bytes > MAX_SSE_LINE_BYTES
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// 预算错误后立即释放半成品缓冲，避免调用方尚未 drop parser 时继续占用内存。
    fn budget_exceeded(&mut self, unit: &str, limit: usize) -> ModelError {
        self.release_buffers();
        self.failed = true;
        ModelError::protocol(ProtocolErrorKind::SseDecodeBudgetExceeded)
            .with_message(format!("SSE {unit} exceeds the {limit}-byte decode limit"))
    }

    fn release_buffers(&mut self) {
        self.pending_bytes = Vec::new();
        self.event = None;
        self.data_lines = Vec::new();
        self.data_bytes = 0;
        self.saw_data_field = false;
    }

    #[cfg(test)]
    pub(crate) fn buffered_bytes(&self) -> usize {
        self.pending_bytes.len()
            + self.event.as_deref().map_or(0, str::len)
            + self
                .data_lines
                .iter()
                .map(|line| line.len() + std::mem::size_of::<String>())
                .sum::<usize>()
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}
