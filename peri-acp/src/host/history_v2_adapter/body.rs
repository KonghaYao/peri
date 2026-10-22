//! opaque 正文读取：有界拼接 canonical payload → 展示投影 → 范围切片。
//!
//! 设计 §8.3/§10.4：canonical 只作为解码输入，wire 上只出现投影后的纯文本。单条
//! payload 上限 [`MAX_CANONICAL_BODY_BYTES`]，读取按 64 KiB 分块推进，达到上限立即
//! 失败，不静默截断后冒充完整正文。

use base64::Engine as _;
use peri_acp_types::history_v2::{
    DecimalU64, HistoryBackendError, HistoryBodySlice, HistoryEncoding, HistoryRepresentation,
};
use peri_acp_types::history_window::{HistoryCommittedTail, HistorySource as WindowSource};
use peri_acp_types::messages::MessageId;
use peri_acp_types::store::deserialize_persisted_payload;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::PeriCaps;

use super::display;
use super::HistoryV2BackendAdapter;

/// 单条 canonical payload 的读取上限（设计 §8.3 的 8 MiB 上限）。
pub(super) const MAX_CANONICAL_BODY_BYTES: usize = 8 * 1024 * 1024;

/// 展示正文的媒体类型；与描述符的 representation 必须同时改变。
pub(super) const DISPLAY_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

impl HistoryV2BackendAdapter {
    /// 按块读取完整 canonical payload；任何超过上限的情况都返回 `budget_exceeded`。
    pub(super) async fn read_canonical_payload(
        &self,
        thread_id: &ThreadId,
        source: &WindowSource,
        entry_id: MessageId,
        tail: Option<&HistoryCommittedTail>,
        advertised_total: u64,
    ) -> Result<Vec<u8>, HistoryBackendError> {
        if advertised_total > MAX_CANONICAL_BODY_BYTES as u64 {
            // 描述符已给出真实总长：不读一个注定拒绝的单条正文。
            return Err(HistoryBackendError::BudgetExceeded);
        }
        let mut payload = Vec::with_capacity(advertised_total as usize);
        loop {
            let remaining = MAX_CANONICAL_BODY_BYTES.saturating_sub(payload.len());
            if remaining == 0 {
                // 描述符低估了真实长度：读到硬上限仍未 EOF。
                return Err(HistoryBackendError::BudgetExceeded);
            }
            let requested = (self.chunk_bytes() as usize).min(remaining) as u32;
            #[cfg(test)]
            self.payload_reads.lock().push(requested);
            let chunk = self
                .load_body(
                    thread_id,
                    source,
                    entry_id,
                    payload.len() as u64,
                    requested,
                    tail,
                )
                .await?;
            if chunk.bytes.len() > requested as usize {
                // 存储层违反块大小契约，不能把超量字节并入正文。
                return Err(HistoryBackendError::BackendUnavailable);
            }
            if chunk.bytes.is_empty() && !chunk.complete {
                return Err(HistoryBackendError::SnapshotInvalidated);
            }
            payload.extend_from_slice(&chunk.bytes);
            if chunk.complete {
                if payload.len() as u64 != advertised_total {
                    return Err(HistoryBackendError::SnapshotInvalidated);
                }
                return Ok(payload);
            }
            if payload.len() as u64 >= advertised_total {
                return Err(HistoryBackendError::SnapshotInvalidated);
            }
        }
    }

    /// 把 canonical payload 投影成展示文本，并按请求范围切出 wire 块。
    pub(super) fn body_slice(
        &self,
        body_ref: &str,
        offset: u64,
        max_bytes: u64,
        canonical: &[u8],
    ) -> Result<HistoryBodySlice, HistoryBackendError> {
        let text = display_text_from_canonical(canonical, &self.caps)?;
        let total = text.len() as u64;
        if offset > total {
            return Err(HistoryBackendError::InvalidCursor);
        }
        let max_bytes = u32::try_from(max_bytes.min(u64::from(self.max_body_chunk_bytes)))
            .map_err(|_| HistoryBackendError::BudgetExceeded)?;
        if max_bytes == 0 {
            return Err(HistoryBackendError::InvalidCursor);
        }
        let start = usize::try_from(offset).map_err(|_| HistoryBackendError::InvalidCursor)?;
        let end = start.saturating_add(max_bytes as usize).min(text.len());
        let data = &text.as_bytes()[start..end];
        let complete = end == text.len();
        Ok(HistoryBodySlice {
            body_ref: body_ref.to_owned(),
            content_version: display::content_version(canonical),
            representation: HistoryRepresentation::DisplayText,
            encoding: HistoryEncoding::Base64,
            media_type: DISPLAY_MEDIA_TYPE.to_owned(),
            offset: DecimalU64::new(offset),
            data: base64::engine::general_purpose::STANDARD.encode(data),
            next_offset: (!complete).then(|| DecimalU64::new(end as u64)),
            complete,
            total_bytes: DecimalU64::new(total),
        })
    }
}

/// canonical 字节 → 展示文本。解码失败不泄露原文，统一 `source_unavailable`；
/// 解码成功但没有可见展示项时按 `not_found` 处理，不返回空串冒充成功。
fn display_text_from_canonical(
    canonical: &[u8],
    caps: &PeriCaps,
) -> Result<String, HistoryBackendError> {
    let raw = std::str::from_utf8(canonical).map_err(|_| HistoryBackendError::SourceUnavailable)?;
    let payload =
        deserialize_persisted_payload(raw).map_err(|_| HistoryBackendError::SourceUnavailable)?;
    if !payload.displayable_to_client() {
        return Err(HistoryBackendError::NotFound);
    }
    display::display_text(&payload, caps).ok_or(HistoryBackendError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::messages::MessageContent;
    use peri_acp_types::store::serialize_persisted_payload;

    #[test]
    fn invalid_canonical_does_not_leak_and_maps_to_source_unavailable() {
        let error = display_text_from_canonical(b"{\"broken\":", &PeriCaps::default())
            .expect_err("invalid envelope");
        assert_eq!(error, HistoryBackendError::SourceUnavailable);
        let error = display_text_from_canonical(&[0xff, 0xfe], &PeriCaps::default())
            .expect_err("invalid utf-8");
        assert_eq!(error, HistoryBackendError::SourceUnavailable);
    }

    #[test]
    fn payload_without_visible_updates_maps_to_not_found() {
        let payload = peri_acp_types::store::PersistedPayload::Message(BaseMessage::ai(
            MessageContent::raw(vec![serde_json::json!({
                "type": "vendor_native",
                "text": "not a projected block"
            })]),
        ));
        let canonical = serialize_persisted_payload(&payload).unwrap();
        let error = display_text_from_canonical(canonical.as_bytes(), &PeriCaps::default())
            .expect_err("no visible updates");
        assert_eq!(error, HistoryBackendError::NotFound);
    }
}
