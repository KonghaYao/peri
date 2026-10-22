//! S7.3 超大条目详情窗口：有界分块、4 MiB 预算、UTF-8 重组与流式导出。

use std::collections::BTreeMap;
use std::sync::Arc;

use peri_acp_types::history_window::HistoryBodyChunk;
use ratatui_kit::ratatui::style::Stylize;

use crate::acp_client::AcpTuiClient;

use crate::kit::history_paging::HistoryBodyRequest;
use crate::kit::history_window::{DetailDescriptor, HistoryV2BodyBinding};

pub const DETAIL_WINDOW_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const DETAIL_MAX_CHUNK_BYTES: u32 = 256 * 1024;
const V2_DETAIL_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailChunk {
    pub offset: u64,
    pub bytes: Arc<Vec<u8>>,
    pub complete: bool,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct HistoryDetailWindow {
    pub descriptor: DetailDescriptor,
    pub scroll_offset: u64,
    chunks: BTreeMap<u64, DetailChunk>,
    retained_bytes: usize,
    max_bytes: usize,
    pending_offset: Option<u64>,
    complete: bool,
    /// V2：首块到达后 `descriptor.total_bytes` 已换成服务端的权威展示长度。
    v2_total_authoritative: bool,
    pub fetch_calls: usize,
    pub exported_bytes: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailError {
    InvalidDescriptor,
    IdentityMismatch,
    OffsetMismatch,
    InvalidChunk,
    Budget,
    Utf8,
    ExportGap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub calls: usize,
    pub bytes: u64,
}

/// 面板可见文本与前方缺口说明。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisibleText {
    pub text: String,
    /// 最早驻留块不从 0 开始：前方正文已被预算淘汰。
    pub prefix_evicted: bool,
    /// 首块起点落在多字节字符中间时跳过的字节数（1..=3）。
    pub skipped_fragment_bytes: usize,
    /// 末尾停在多字节字符中途的字节数（1..=3）：不属于损坏，等下一块补齐。
    pub trailing_fragment_bytes: usize,
}

impl HistoryDetailWindow {
    pub fn open(descriptor: DetailDescriptor) -> Result<Self, DetailError> {
        if descriptor.total_bytes == 0
            || descriptor.chunk_size == 0
            || descriptor.chunk_size > DETAIL_MAX_CHUNK_BYTES
        {
            return Err(DetailError::InvalidDescriptor);
        }
        Ok(Self {
            descriptor,
            scroll_offset: 0,
            chunks: BTreeMap::new(),
            retained_bytes: 0,
            max_bytes: DETAIL_WINDOW_MAX_BYTES,
            pending_offset: None,
            complete: false,
            v2_total_authoritative: false,
            fetch_calls: 0,
            exported_bytes: 0,
            last_error: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn chunk_arc(&self, offset: u64) -> Option<Arc<Vec<u8>>> {
        self.chunks
            .get(&offset)
            .map(|chunk| Arc::clone(&chunk.bytes))
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn pending_offset(&self) -> Option<u64> {
        self.pending_offset
    }

    /// 登记一次区间请求。`complete` 只表示已顺序读到 EOF，不表示任意区间仍驻留；
    /// 已淘汰区间仍可重新请求（幂等，服务端按偏移返回）。
    pub fn request(&mut self, offset: u64, max_bytes: u32) -> Option<HistoryBodyRequest> {
        if self.pending_offset.is_some() || offset >= self.descriptor.total_bytes {
            return None;
        }
        let max_bytes = max_bytes
            .min(self.descriptor.chunk_size)
            .min(DETAIL_MAX_CHUNK_BYTES);
        if max_bytes == 0 {
            return None;
        }
        self.pending_offset = Some(offset);
        self.fetch_calls = self.fetch_calls.saturating_add(1);
        Some(HistoryBodyRequest {
            entry_id: self.descriptor.key.entry_id.clone(),
            source: self.descriptor.key.source.clone(),
            snapshot_token: self.descriptor.snapshot_token.clone(),
            read_ticket: self.descriptor.read_ticket.clone(),
            offset,
            max_bytes,
        })
    }

    pub fn next_request(&mut self) -> Option<HistoryBodyRequest> {
        if self.complete {
            return None;
        }
        let offset = self
            .pending_offset
            .or_else(|| {
                self.chunks
                    .last_key_value()
                    .map(|(_, chunk)| chunk.offset.saturating_add(chunk.bytes.len() as u64))
            })
            .unwrap_or(0);
        self.request(offset, DETAIL_MAX_CHUNK_BYTES)
    }

    pub fn apply_chunk(
        &mut self,
        chunk: HistoryBodyChunk,
        max_bytes: u32,
    ) -> Result<(), DetailError> {
        if chunk.entry_id != self.descriptor.key.entry_id {
            self.last_error = Some("正文块身份不匹配".into());
            return Err(DetailError::IdentityMismatch);
        }
        let expected = self.pending_offset.unwrap_or_else(|| {
            self.chunks.last_key_value().map_or(0, |(_, previous)| {
                previous.offset.saturating_add(previous.bytes.len() as u64)
            })
        });
        if chunk.offset != expected {
            self.last_error = Some("正文块偏移不连续".into());
            return Err(DetailError::OffsetMismatch);
        }
        chunk
            .validate(
                &self.descriptor.key.entry_id,
                chunk.offset,
                max_bytes.min(DETAIL_MAX_CHUNK_BYTES),
            )
            .map_err(|_| {
                self.last_error = Some("正文块校验失败".into());
                DetailError::InvalidChunk
            })?;
        self.pending_offset = None;
        self.insert_chunk(chunk.offset, chunk.bytes, chunk.complete, chunk.next_offset)
    }

    /// V2 正文块：身份来自 descriptor 的 binding，wire 字段必须逐项匹配当前
    /// 窗口；base64 解码后按与 V1 相同的连续偏移规则并入。
    pub fn apply_v2_chunk(
        &mut self,
        chunk: &peri_acp_types::history_v2::HistoryBodyChunk,
        max_bytes: u32,
    ) -> Result<(), DetailError> {
        use base64::Engine as _;
        let Some(binding) = self.descriptor.v2.as_ref() else {
            return Err(DetailError::InvalidDescriptor);
        };
        if chunk.session_id != binding.session_id
            || chunk.window_id != binding.window_id
            || chunk.snapshot_token != binding.snapshot_token
            || chunk.body_ref != binding.body_ref
        {
            self.last_error = Some("正文块身份不匹配".into());
            return Err(DetailError::IdentityMismatch);
        }
        chunk.validate_shape().map_err(|_| {
            self.last_error = Some("正文块校验失败".into());
            DetailError::InvalidChunk
        })?;
        let expected = self.pending_offset.unwrap_or_else(|| {
            self.chunks.last_key_value().map_or(0, |(_, previous)| {
                previous.offset.saturating_add(previous.bytes.len() as u64)
            })
        });
        if chunk.offset.get() != expected {
            self.last_error = Some("正文块偏移不连续".into());
            return Err(DetailError::OffsetMismatch);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(chunk.data.as_bytes())
            .map_err(|_| {
                self.last_error = Some("正文块校验失败".into());
                DetailError::InvalidChunk
            })?;
        if bytes.len() > max_bytes.min(DETAIL_MAX_CHUNK_BYTES) as usize {
            self.last_error = Some("正文块校验失败".into());
            return Err(DetailError::InvalidChunk);
        }
        // 首块可以用服务端的权威展示长度修正描述符里的 canonical 预估；
        // 之后每块必须与已确认总长一致，避免把不同正文的块拼在一起。
        let total = chunk.total_bytes.get();
        if !self.v2_total_authoritative {
            if total == 0 {
                self.last_error = Some("正文块校验失败".into());
                return Err(DetailError::InvalidChunk);
            }
            self.descriptor.total_bytes = total;
            self.v2_total_authoritative = true;
            if self.scroll_offset >= total {
                self.scroll_offset = total.saturating_sub(1);
            }
        } else if total != self.descriptor.total_bytes {
            self.last_error = Some("正文总长度与首块不一致".into());
            return Err(DetailError::InvalidChunk);
        }
        self.pending_offset = None;
        let next_offset = chunk.next_offset.map(|offset| offset.get());
        self.insert_chunk(chunk.offset.get(), bytes, chunk.complete, next_offset)
    }

    fn insert_chunk(
        &mut self,
        offset: u64,
        bytes: Vec<u8>,
        complete: bool,
        next_offset: Option<u64>,
    ) -> Result<(), DetailError> {
        let bytes = Arc::new(bytes);
        self.retained_bytes = self.retained_bytes.saturating_add(bytes.capacity());
        self.complete |= complete;
        self.chunks.insert(
            offset,
            DetailChunk {
                offset,
                bytes,
                complete,
                next_offset,
            },
        );
        self.enforce_budget()
    }

    fn enforce_budget(&mut self) -> Result<(), DetailError> {
        while self.retained_bytes > self.max_bytes {
            // 顺序加载以最新块为锚（保持偏移连续），同时永不淘汰视口所在块：
            // 向下滚动后预算压力先清远处旧块；滚回开头补读的首块也不会被
            // 下一轮预算立刻淘汰。
            let anchor = self
                .chunks
                .last_key_value()
                .map(|(offset, chunk)| offset.saturating_add(chunk.bytes.len() as u64))
                .unwrap_or(self.scroll_offset)
                .max(self.scroll_offset);
            let viewport_chunk = self.chunks.iter().find_map(|(offset, chunk)| {
                let start = *offset;
                let end = start.saturating_add(chunk.bytes.len() as u64);
                (start..end)
                    .contains(&self.scroll_offset)
                    .then_some(*offset)
            });
            let farthest = self
                .chunks
                .iter()
                .filter(|(offset, _)| Some(**offset) != viewport_chunk)
                .max_by_key(|(offset, chunk)| {
                    let start = **offset;
                    let end = start.saturating_add(chunk.bytes.len() as u64);
                    distance_to(anchor, start, end)
                })
                .map(|(offset, _)| *offset);
            let Some(offset) = farthest else {
                self.last_error = Some("详情预算无法释放".into());
                return Err(DetailError::Budget);
            };
            if self.chunks.len() == 1 {
                self.last_error = Some("单块正文超过详情预算".into());
                return Err(DetailError::Budget);
            }
            if let Some(chunk) = self.chunks.remove(&offset) {
                self.retained_bytes = self.retained_bytes.saturating_sub(chunk.bytes.capacity());
            }
        }
        Ok(())
    }

    pub fn scroll_to(&mut self, offset: u64) -> Option<HistoryBodyRequest> {
        self.scroll_offset = offset.min(self.descriptor.total_bytes.saturating_sub(1));
        let covered = self.chunks.values().any(|chunk| {
            let start = chunk.offset;
            let end = start.saturating_add(chunk.bytes.len() as u64);
            (start..end).contains(&self.scroll_offset)
        });
        if covered {
            return None;
        }
        self.request(
            self.scroll_offset,
            self.descriptor.chunk_size.min(DETAIL_MAX_CHUNK_BYTES),
        )
    }

    /// 已加载的连续前缀文本。跨块 UTF-8 在这里整体解码，避免按块截断乱码。
    ///
    /// 首块不从 0 开始（或尚未加载任何块）时不存在完整前缀，导出必须显式报
    /// [`DetailError::ExportGap`]，不能返回空串冒充完整正文。
    pub fn loaded_text(&self) -> Result<String, DetailError> {
        if self
            .chunks
            .first_key_value()
            .is_none_or(|(offset, _)| *offset != 0)
        {
            return Err(DetailError::ExportGap);
        }
        let mut cursor = 0u64;
        let mut bytes = Vec::new();
        for chunk in self.chunks.values() {
            if chunk.offset != cursor {
                break;
            }
            bytes.extend_from_slice(&chunk.bytes);
            cursor = cursor.saturating_add(chunk.bytes.len() as u64);
        }
        String::from_utf8(bytes).map_err(|_| DetailError::Utf8)
    }

    /// 面板展示文本：从**最早驻留块**开始拼接连续区间。
    ///
    /// 超过 4 MiB 预算后首块会被淘汰；此时展示仍必须从首个驻留块开始。块边界可能
    /// 落在多字节字符中间：
    ///
    /// - 开头最多跳过 3 个多字节字符的续字节（跨块残片），找到可解码的起点；
    /// - 末尾若只差后续字节（`Utf8Error::error_len` 为 `None`），保留有效前缀，把
    ///   未完成残片计入 [`VisibleText::trailing_fragment_bytes`]，等下一块拼接；
    /// - 只有连续区间已覆盖 EOF、或序列在中间真实非法时才返回 [`DetailError::Utf8`]。
    pub fn visible_text(&self) -> Result<VisibleText, DetailError> {
        let Some((start, _)) = self.chunks.first_key_value() else {
            return Ok(VisibleText::default());
        };
        let mut cursor = *start;
        let mut bytes = Vec::new();
        for chunk in self.chunks.values() {
            if chunk.offset != cursor {
                break;
            }
            bytes.extend_from_slice(&chunk.bytes);
            cursor = cursor.saturating_add(chunk.bytes.len() as u64);
        }
        if bytes.is_empty() {
            return Ok(VisibleText {
                prefix_evicted: *start > 0,
                ..VisibleText::default()
            });
        }
        let skipped = bytes
            .iter()
            .take(3)
            .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
            .count();
        let candidate = &bytes[skipped..];
        let text = match std::str::from_utf8(candidate) {
            Ok(text) => text,
            Err(error) => {
                // `error_len() == None` 只表示输入在末尾提前结束，即末尾停在多字节
                // 字符中途。连续区间尚未覆盖 EOF 时这是正常的半块，下一块会补齐；
                // 已到 EOF 仍不完整才是真实损坏。
                if error.error_len().is_some() || cursor >= self.descriptor.total_bytes {
                    return Err(DetailError::Utf8);
                }
                std::str::from_utf8(&candidate[..error.valid_up_to()])
                    .map_err(|_| DetailError::Utf8)?
            }
        };
        Ok(VisibleText {
            text: text.to_string(),
            prefix_evicted: *start > 0,
            skipped_fragment_bytes: skipped,
            trailing_fragment_bytes: candidate.len().saturating_sub(text.len()),
        })
    }

    /// 流式导出：每个块直接交给 sink，不构造完整正文副本。
    pub fn export_stream(
        &mut self,
        mut sink: impl FnMut(&[u8]) -> bool,
    ) -> Result<ExportReport, DetailError> {
        let mut report = ExportReport::default();
        let mut cursor = 0u64;
        for chunk in self.chunks.values() {
            if chunk.offset != cursor {
                self.last_error = Some("详情存在未加载缺口".into());
                return Err(DetailError::ExportGap);
            }
            if !sink(&chunk.bytes) {
                break;
            }
            report.calls += 1;
            report.bytes = report.bytes.saturating_add(chunk.bytes.len() as u64);
            cursor = cursor.saturating_add(chunk.bytes.len() as u64);
        }
        self.exported_bytes = self.exported_bytes.saturating_add(report.bytes);
        Ok(report)
    }

    /// 失效/错误：记录明确原因并释放已加载正文（占位仍在 committed）。
    pub fn fail(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
        self.close();
    }

    pub fn close(&mut self) {
        self.chunks.clear();
        self.retained_bytes = 0;
        self.pending_offset = None;
        self.complete = false;
    }
}

/// 从窗口模型打开详情；只使用 descriptor，不触发整条正文读取。
pub fn open_history_detail(
    window: &crate::kit::history_window::HistoryWindow,
    key: &crate::kit::history_window::PersistentEntryKey,
) -> Result<(), DetailError> {
    let descriptor = window
        .detail_descriptor(key)
        .cloned()
        .ok_or(DetailError::InvalidDescriptor)?;
    let detail = HistoryDetailWindow::open(descriptor)?;
    crate::kit::atoms::HISTORY_DETAIL.set(Some(detail));
    Ok(())
}

pub fn close_history_detail() {
    crate::kit::atoms::HISTORY_DETAIL.set(None);
}

/// 导航代次切换（seek/reopen）或窗口清除时收起 V2 详情。
///
/// 旧 descriptor 的 window/snapshot/bodyRef 属于被替换的窗口，继续读取只会得到
/// 失效结果或旧位置内容；V1 详情不受 V2 代次影响，保持原状。
pub(crate) fn close_v2_history_detail() {
    let is_v2 = {
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let guard = handle.read();
        guard
            .as_ref()
            .is_some_and(|detail| detail.descriptor.v2.is_some())
    };
    if is_v2 {
        crate::kit::atoms::HISTORY_DETAIL.set(None);
    }
}

pub async fn fetch_detail_chunk(
    client: &AcpTuiClient,
    window: &mut HistoryDetailWindow,
    offset: u64,
    max_bytes: u32,
) -> Result<(), DetailError> {
    let Some(request) = window.request(offset, max_bytes) else {
        return Ok(());
    };
    let raw = client
        .load_history_body(&window.descriptor.key.entry_id_session(), &request)
        .await
        .map_err(|error| {
            window.fail(error.to_string());
            DetailError::InvalidChunk
        })?;
    let chunk: HistoryBodyChunk = serde_json::from_value(raw).map_err(|_| {
        window.fail("详情响应形状错误");
        DetailError::InvalidChunk
    })?;
    window.apply_chunk(chunk, max_bytes)
}

fn distance_to(anchor: u64, start: u64, end: u64) -> u64 {
    if anchor < start {
        start.saturating_sub(anchor)
    } else if anchor >= end {
        anchor.saturating_sub(end)
    } else {
        0
    }
}

#[cfg(test)]
#[path = "history_detail_test.rs"]
mod tests;

// ── UI 接线：占位 note → 详情面板；body 分块异步拉取 ─────────────────────────

/// 事件层：聚焦/点击后的占位 note（按 content_hash 关联 descriptor）。
pub fn open_detail_from_slot(slot: usize) -> bool {
    let hash = {
        let handle = crate::kit::atoms::VIEW_MODELS.state();
        handle
            .read()
            .items
            .get(slot)
            .map(|unit| unit.content_hash())
    };
    let Some(hash) = hash else {
        return false;
    };
    let target = {
        let handle = crate::kit::atoms::HISTORY_DETAIL_TARGETS.state();
        handle
            .read()
            .iter()
            .find(|target| target.content_hash == hash)
            .map(|target| target.descriptor.clone())
    };
    let Some(descriptor) = target else {
        return false;
    };
    match HistoryDetailWindow::open(descriptor) {
        Ok(window) => {
            crate::kit::atoms::HISTORY_DETAIL.set(Some(window));
            crate::kit::panel_registry::open_panel(
                crate::app::panel_types::PanelKind::HistoryDetail,
            );
            request_detail_chunk(0);
            true
        }
        Err(_) => false,
    }
}

/// 拉取/续拉一个块。单在途由详情模型保证；响应只在同一 descriptor 时应用。
pub fn request_detail_chunk(offset: u64) {
    let prepared = {
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let mut guard = handle.write();
        guard.as_mut().and_then(|detail| {
            let request = detail.request(
                offset,
                detail.descriptor.chunk_size.min(DETAIL_MAX_CHUNK_BYTES),
            )?;
            Some((request, detail.descriptor.v2.clone()))
        })
    };
    let Some((request, v2)) = prepared else {
        return;
    };
    dispatch_detail_request(request, v2);
}

/// V1/V2 共用入口：V2 只换传输，不改变面板的登记/在途/预算语义。
fn dispatch_detail_request(request: HistoryBodyRequest, v2: Option<HistoryV2BodyBinding>) {
    match v2 {
        Some(binding) => spawn_v2_detail_chunk(binding, request.offset, request.max_bytes),
        None => spawn_detail_chunk(request),
    }
}

/// 发送一个**已登记**的详情请求。调用方必须持有 `HistoryDetailWindow::request`
/// 返回的请求；否则 `pending_offset` 与真实 RPC 会脱节，后续请求被自己阻断。
fn spawn_detail_chunk(request: HistoryBodyRequest) {
    let Some(client) = crate::kit::atoms::ACP_CLIENT_HANDLE.get().cloned() else {
        if let Some(detail) = crate::kit::atoms::HISTORY_DETAIL.state().write().as_mut() {
            detail.fail("ACP client unavailable");
        }
        return;
    };
    let session_id = request.entry_id.rsplit_once(':').map_or_else(
        || request.entry_id.clone(),
        |(session, _)| session.to_string(),
    );
    let key = crate::kit::history_window::PersistentEntryKey::new(
        request.source.clone(),
        request.entry_id.clone(),
    );
    tokio::spawn(async move {
        let result = client.load_history_body(&session_id, &request).await;
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let mut guard = handle.write();
        let Some(detail) = guard.as_mut() else {
            return;
        };
        if detail.descriptor.key != key {
            return;
        }
        match result {
            Ok(raw) => match serde_json::from_value::<HistoryBodyChunk>(raw) {
                Ok(chunk) => {
                    let max_bytes = request.max_bytes;
                    if let Err(error) = detail.apply_chunk(chunk, max_bytes) {
                        detail.fail(format!("详情读取失败：{error:?}"));
                    }
                }
                Err(_) => detail.fail("详情响应形状错误"),
            },
            Err(error) => detail.fail(error.to_string()),
        }
    });
}

/// V2 正文块：严格解码 `_peri/history` 的 body 结果，只应用到同一 binding。
fn spawn_v2_detail_chunk(binding: HistoryV2BodyBinding, offset: u64, max_bytes: u32) {
    let Some(client) = crate::kit::atoms::ACP_CLIENT_HANDLE.get().cloned() else {
        if let Some(detail) = crate::kit::atoms::HISTORY_DETAIL.state().write().as_mut() {
            detail.fail("ACP client unavailable");
        }
        return;
    };
    tokio::spawn(async move {
        let request = crate::kit::history_v2::consumer::body_request(&binding, offset, max_bytes);
        let result = peri_time::timeout(
            V2_DETAIL_FETCH_TIMEOUT,
            client.history_v2_request(request.params()),
        )
        .await;
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let mut guard = handle.write();
        let Some(detail) = guard.as_mut() else {
            return;
        };
        if detail.descriptor.v2.as_ref() != Some(&binding) {
            return;
        }
        match result {
            Ok(Ok(raw)) => match crate::kit::history_v2::request::parse_body_result(&raw) {
                Ok(chunk) => {
                    if let Err(error) = detail.apply_v2_chunk(&chunk, max_bytes) {
                        detail.fail(format!("详情读取失败：{error:?}"));
                    }
                }
                Err(_) => detail.fail("详情响应形状错误"),
            },
            Ok(Err(error)) => {
                let failure =
                    crate::kit::history_v2::error::HistoryV2WireFailure::from_error(&error);
                detail.fail(format!("详情读取失败：{}", failure.label()));
            }
            Err(_) => detail.fail("详情请求超时"),
        }
    });
}

fn scroll_detail(delta: i64) {
    // `scroll_to` 已登记 pending；这里只发送它返回的那一个请求。再调用
    // `request_detail_chunk` 会因 `pending_offset.is_some()` 被拒绝，第二个 RPC
    // 根本不会发出。
    let v2 = {
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let guard = handle.read();
        guard
            .as_ref()
            .and_then(|detail| detail.descriptor.v2.clone())
    };
    if let Some(request) = scroll_detail_request(delta) {
        dispatch_detail_request(request, v2);
    }
}

/// 计算一次滚动是否触发新块请求（已登记 `pending_offset`）。
fn scroll_detail_request(delta: i64) -> Option<HistoryBodyRequest> {
    let handle = crate::kit::atoms::HISTORY_DETAIL.state();
    let mut guard = handle.write();
    let detail = guard.as_mut()?;
    let step = i64::from(detail.descriptor.chunk_size.min(DETAIL_MAX_CHUNK_BYTES));
    let next = (detail.scroll_offset as i64 + delta.saturating_mul(step))
        .max(0)
        .min(detail.descriptor.total_bytes.saturating_sub(1) as i64) as u64;
    detail.scroll_to(next)
}

fn export_detail_to_clipboard() {
    let outcome = {
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let guard = handle.read();
        guard.as_ref().map(HistoryDetailWindow::loaded_text)
    };
    match outcome {
        Some(Ok(text)) => {
            let _ = crate::kit::clipboard::copy_text(&text);
        }
        Some(Err(error)) => {
            // 前缀缺口不是“空正文”：必须显式失败，不能静默复制空串。
            let message = match error {
                DetailError::ExportGap => "正文前缀已释放，无法复制完整内容；请先向上滚动重新加载",
                _ => "正文不是完整 UTF-8，无法复制",
            };
            let handle = crate::kit::atoms::HISTORY_DETAIL.state();
            if let Some(detail) = handle.write().as_mut() {
                detail.last_error = Some(message.to_string());
            }
        }
        None => {}
    }
}

pub(crate) fn build_history_detail_lines(
    detail: Option<&HistoryDetailWindow>,
    inherited_notice: Option<&str>,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Vec<ratatui_kit::ratatui::text::Line<'static>> {
    use ratatui_kit::ratatui::{
        style::Style,
        text::{Line, Span},
    };
    let mut lines = Vec::new();
    if let Some(notice) = inherited_notice {
        lines.push(Line::from(Span::styled(
            notice.to_string(),
            Style::new().fg(theme.semantic.status.warning),
        )));
        lines.push(Line::from(""));
    }
    let Some(detail) = detail else {
        lines.push(Line::from("详情已关闭"));
        return lines;
    };
    lines.push(Line::from(format!(
        "{} · {} · {:.1} KiB",
        detail.descriptor.key.entry_id,
        if detail.descriptor.key.source.is_own() {
            "own"
        } else {
            "inherited"
        },
        detail.descriptor.total_bytes as f64 / 1024.0
    )));
    if let Some(error) = &detail.last_error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::new().fg(theme.semantic.status.error),
        )));
    }
    if detail.pending_offset().is_some() {
        lines.push(Line::from("加载中…"));
    } else if !detail.is_complete() {
        lines.push(Line::from("↓ 继续加载更多内容"));
    }
    lines.push(Line::from(""));
    match detail.visible_text() {
        Ok(visible) => {
            if visible.prefix_evicted {
                lines.push(Line::from("（更早内容已从窗口释放；向上滚动可重新加载）"));
            }
            if visible.skipped_fragment_bytes > 0 {
                lines.push(Line::from(format!(
                    "（前 {} 字节为跨块字符残片，已略过）",
                    visible.skipped_fragment_bytes
                )));
            }
            if visible.trailing_fragment_bytes > 0 {
                lines.push(Line::from(format!(
                    "（末尾 {} 字节为未完成字符，待下一块拼接）",
                    visible.trailing_fragment_bytes
                )));
            }
            if !visible.text.is_empty() {
                lines.extend(
                    visible
                        .text
                        .lines()
                        .map(|line| Line::from(line.to_string())),
                );
            } else if !visible.prefix_evicted {
                lines.push(Line::from("（尚无已加载正文）"));
            }
        }
        Err(_) => lines.push(Line::from("正文片段不是完整 UTF-8；请继续加载相邻块")),
    }
    lines.push(Line::from(""));
    lines.push(Line::from("↑/↓ 滚动 · e 复制已加载正文 · Esc/q 关闭"));
    lines
}

#[ratatui_kit::prelude::component]
pub fn HistoryDetailPanel(
    mut hooks: ratatui_kit::prelude::Hooks,
) -> impl Into<ratatui_kit::prelude::AnyElement<'static>> {
    use crate::kit::atoms;
    use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind};
    use ratatui_kit::prelude::*;
    use ratatui_kit::ratatui::layout::Constraint;

    let detail_store = hooks.use_atom(&atoms::HISTORY_DETAIL);
    let notice_store = hooks.use_atom(&atoms::HISTORY_INHERITED_NOTICE);
    let theme_store = hooks.use_atom(&peri_theme::atoms::THEME_ATOM);
    let sv = hooks.use_state(ScrollViewState::default);
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                close_history_detail();
                crate::kit::panel_registry::close_active_panel();
                EventResult::Consumed
            }
            KeyCode::Down => {
                scroll_detail(1);
                // 不消费：让内部 ScrollView 继续做视觉滚动。
                EventResult::Ignored
            }
            KeyCode::Up => {
                scroll_detail(-1);
                EventResult::Ignored
            }
            KeyCode::PageDown => {
                scroll_detail(4);
                EventResult::Ignored
            }
            KeyCode::PageUp => {
                scroll_detail(-4);
                EventResult::Ignored
            }
            KeyCode::Char('e') => {
                export_detail_to_clipboard();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });
    let area = hooks.use_previous_size();
    let detail = detail_store.read().clone();
    let notice = notice_store.read().clone();
    let theme = theme_store.read();
    let lines = build_history_detail_lines(detail.as_ref(), notice.as_deref(), &theme);
    let content = ratatui::widgets::Paragraph::new(ratatui::text::Text::from(lines));
    crate::kit::panel_scroll::register_panel_scroll(
        crate::app::panel_types::PanelKind::HistoryDetail,
        area,
        sv,
    );
    panel_shell!(crate::app::panel_types::PanelKind::HistoryDetail, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}
