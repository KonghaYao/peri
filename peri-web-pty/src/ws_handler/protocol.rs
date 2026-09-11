//! WebSocket/PTY 协议边界：resize、跨块 UTF-8 与一次性 DSR 应答。
use crate::pty_session::PtySession;

pub(super) fn try_handle_resize(text: &str, session: &mut PtySession) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    if parsed.get("type").and_then(|v| v.as_str()) != Some("resize") {
        return false;
    }
    let (Some(cols), Some(rows)) = (
        parsed.get("cols").and_then(|v| v.as_u64()),
        parsed.get("rows").and_then(|v| v.as_u64()),
    ) else {
        return false;
    };
    if let Err(error) = session.resize(cols as u16, rows as u16) {
        tracing::warn!("PTY resize 失败: {error}");
    }
    true
}

pub(super) fn exit_message(code: Option<i32>) -> String {
    let display = code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".into());
    format!("\r\n[process exited with code {display}]\r\n")
}

#[derive(Default)]
pub(super) struct OutputDecoder {
    leftover: Vec<u8>,
    dsr_tail: Vec<u8>,
    dsr_replied: bool,
}

impl OutputDecoder {
    /// Returns decoded output and whether the caller must send ESC[1;1R to the PTY.
    pub(super) fn push(&mut self, bytes: &[u8]) -> (String, bool) {
        let mut reply = false;
        if !self.dsr_replied {
            self.dsr_tail.extend_from_slice(bytes);
            if self.dsr_tail.windows(4).any(|window| window == b"\x1b[6n") {
                self.dsr_replied = true;
                reply = true;
            }
            let discard = self.dsr_tail.len().saturating_sub(3);
            self.dsr_tail.drain(..discard);
        }
        let mut data = std::mem::take(&mut self.leftover);
        data.extend_from_slice(bytes);
        let text = match std::str::from_utf8(&data) {
            Ok(text) => text.to_owned(),
            Err(error) if error.error_len().is_some() => {
                String::from_utf8_lossy(&data).into_owned()
            }
            Err(error) => {
                self.leftover
                    .extend_from_slice(&data[error.valid_up_to()..]);
                // valid_up_to is the UTF-8 boundary returned by the decoder.
                String::from_utf8_lossy(&data[..error.valid_up_to()]).into_owned()
            }
        };
        (text, reply)
    }

    pub(super) fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.leftover)).into_owned()
    }
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
