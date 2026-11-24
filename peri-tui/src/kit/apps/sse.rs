//! 最小 SSE 解码器（手写，不引入新 crate）。
//!
//! 只覆盖网关 `/view/sessions/{id}/events` 使用到的子集：`event:` 名、单行或
//! 多行 `data:`、注释行（keepalive `: keepalive`）与空行分帧。跨 chunk 边界
//! 的残片留在缓冲区，等下一次 `feed`。缓冲区有上限：一次帧事件是几百 KB 量级
//! 的 base64，超限说明对端不是本网关，直接丢弃并标记 `overflowed`。

/// 单个 SSE 事件（尚未按业务类型解码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSseEvent {
    /// `event:` 字段；缺省为 `"message"`。
    pub event: String,
    /// 多行 `data:` 以 `\n` 连接。
    pub data: String,
}

impl RawSseEvent {
    /// 网关 keepalive 注释不产生事件；这里只判断业务事件名是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.event.is_empty() && self.data.is_empty()
    }
}

/// 单帧 data 上限（含 base64 载荷）。网关单帧 JPEG 远小于此值。
const MAX_BUFFER_BYTES: usize = 16 * 1024 * 1024;

/// 增量 SSE 解析器。
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: String,
    overflowed: bool,
}

impl SseParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一个 chunk，返回本 chunk 内完成的事件（可能为空）。
    ///
    /// 注释行与未知字段按 SSE 规范忽略；只有空行才提交累积的事件。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<RawSseEvent> {
        if self.overflowed {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_BUFFER_BYTES {
            self.buffer.clear();
            self.event = None;
            self.data.clear();
            self.overflowed = true;
            return Vec::new();
        }

        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=newline).collect::<Vec<u8>>();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.is_empty() {
                if let Some(event) = self.take_complete_event() {
                    events.push(event);
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = Some(value.to_string()),
                "data" => {
                    if !self.data.is_empty() {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                }
                // id / retry 与首阶段无关：忽略，保持与 SSE 规范一致的向前兼容。
                _ => {}
            }
        }
        events
    }

    fn take_complete_event(&mut self) -> Option<RawSseEvent> {
        let event = self.event.take();
        if self.data.is_empty() {
            // 没有 data 的事件（例如只有注释）不投递。
            return None;
        }
        let data = std::mem::take(&mut self.data);
        Some(RawSseEvent {
            event: event.unwrap_or_else(|| "message".to_string()),
            data,
        })
    }

    /// 缓冲区溢出（对端不是本网关 / 单帧超过 16 MiB）。
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }
}

#[cfg(test)]
#[path = "sse_test.rs"]
mod tests;
