//! H5b：正文分块 reducer（设计 §7.4、§8.3、§12.2）。
//!
//! 正文只从当前窗口**已交付的 descriptor** 读取：bodyRef 是 opaque 令牌，客户端
//! 不拼 URL、不猜范围。块必须从 `nextOffset` 连续推进；`complete=true` 后停止。
//! 失败/取消只释放该正文的在途标记并保留占位，下一次请求从已确认的 offset 重试。

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use peri_acp_types::history_v2::{
    DecimalU64, HistoryBodyChunk, HistoryEncoding, HistoryLimits, HistoryRepresentation,
    HistorySource,
};

use super::HistoryV2Window;
use super::request::HistoryV2BodyRequest;
use super::validate::{entry_identity_key, sub_item_descriptor, sub_item_id};

/// 定位一个可外置正文的展示子项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2BodyLocator {
    pub source: HistorySource,
    pub entry_id: String,
    pub sub_item_id: String,
}

/// 单个正文的客户端状态；描述符本身来自已交付页面。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2BodyState {
    pub body_ref: String,
    pub source: HistorySource,
    pub entry_id: String,
    pub sub_item_id: String,
    pub content_version: String,
    pub representation: HistoryRepresentation,
    pub total_bytes: u64,
    pub chunk_bytes: u32,
    pub data: Vec<u8>,
    pub next_offset: DecimalU64,
    pub complete: bool,
    /// 首块到达后 `total_bytes` 已是服务端权威展示长度。
    total_authoritative: bool,
    /// 失败原因；占位仍在，下一次 `request_body` 会清除它。
    pub failed: Option<String>,
    pub in_flight: Option<HistoryV2BodyInFlight>,
}

impl HistoryV2BodyState {
    /// 已拼接的 UTF-8 文本；二进制表示返回 `None`。
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2BodyInFlight {
    pub request_key: DecimalU64,
    pub offset: DecimalU64,
    pub max_bytes: u32,
    pub view_epoch: u64,
}

/// 正文块被拒绝的类别；不包含 bodyRef/正文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryV2BodyChunkRejection {
    OffsetMismatch,
    ContentVersionMismatch,
    RepresentationMismatch,
    InvalidEncoding,
    InvalidLength,
    Malformed,
}

impl HistoryV2BodyChunkRejection {
    fn label(self) -> &'static str {
        match self {
            Self::OffsetMismatch => "history body chunk offset is not contiguous",
            Self::ContentVersionMismatch => "history body contentVersion changed",
            Self::RepresentationMismatch => {
                "history body representation does not match its descriptor"
            }
            Self::InvalidEncoding => "history body chunk is not valid base64",
            Self::InvalidLength => "history body chunk length violates the descriptor",
            Self::Malformed => "history body chunk shape is invalid",
        }
    }
}

/// 正文结算结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2BodySettlement {
    Accepted {
        received: usize,
        complete: bool,
    },
    /// 旧 requestKey/旧代次/已无在途请求：不写入任何字节。
    Stale,
    Rejected(HistoryV2BodyChunkRejection),
}

impl HistoryV2Window {
    /// 为下一个连续块发出请求；offset 只取本地已确认的 `nextOffset`。
    pub fn request_body(
        &mut self,
        request_key: DecimalU64,
        locator: &HistoryV2BodyLocator,
        max_bytes: u32,
        limits: &HistoryLimits,
    ) -> Result<HistoryV2BodyRequest, super::paging::HistoryV2RequestRejection> {
        self.ensure_paging_ready()?;
        self.ensure_request_key(request_key)?;
        if max_bytes == 0 || max_bytes > limits.max_body_chunk_bytes {
            return Err(super::paging::HistoryV2RequestRejection::InvalidBodyChunk);
        }
        let (body_key, body_ref, content_version, descriptor) = self
            .locate_body_descriptor(locator, limits)
            .ok_or(super::paging::HistoryV2RequestRejection::BodyUnavailable)?;

        if let Some(state) = self.paging.bodies.get(&body_key) {
            if state.complete {
                return Err(super::paging::HistoryV2RequestRejection::BodyComplete);
            }
            if state.in_flight.is_some() {
                return Err(super::paging::HistoryV2RequestRejection::BodyInFlight);
            }
        }
        if self
            .paging
            .retained_body_bytes
            .saturating_add(max_bytes as usize)
            > self.paging.retention.max_body_bytes
        {
            return Err(super::paging::HistoryV2RequestRejection::BudgetExceeded);
        }

        let context = self.base_context(request_key);
        let state = self
            .paging
            .bodies
            .entry(body_key.clone())
            .or_insert_with(|| HistoryV2BodyState {
                body_ref: body_ref.clone(),
                source: locator.source.clone(),
                entry_id: locator.entry_id.clone(),
                sub_item_id: locator.sub_item_id.clone(),
                content_version,
                representation: descriptor.representation,
                total_bytes: descriptor.total_bytes.get(),
                chunk_bytes: descriptor.chunk_bytes,
                data: Vec::new(),
                next_offset: DecimalU64::ZERO,
                complete: false,
                total_authoritative: false,
                failed: None,
                in_flight: None,
            });
        let offset = state.next_offset;
        state.failed = None;
        state.in_flight = Some(HistoryV2BodyInFlight {
            request_key,
            offset,
            max_bytes,
            view_epoch: self.paging.view_epoch,
        });
        self.paging.last_request_key = Some(request_key);

        Ok(HistoryV2BodyRequest {
            context,
            body_key,
            body_ref,
            offset,
            max_bytes,
        })
    }

    /// 结算一个正文块。旧 requestKey/旧窗口/旧代次整体丢弃。
    pub fn settle_body(
        &mut self,
        request: &HistoryV2BodyRequest,
        chunk: &HistoryBodyChunk,
    ) -> HistoryV2BodySettlement {
        if request.context.view_epoch != self.paging.view_epoch {
            return HistoryV2BodySettlement::Stale;
        }
        if request.context.window_id != self.window_id
            || request.context.session_id != self.session_id
            || request.context.snapshot_token != self.snapshot_token
        {
            return HistoryV2BodySettlement::Stale;
        }
        let Some(existing) = self.paging.bodies.get(&request.body_key) else {
            return HistoryV2BodySettlement::Stale;
        };
        let Some(in_flight) = existing.in_flight.as_ref() else {
            return HistoryV2BodySettlement::Stale;
        };
        if in_flight.request_key != request.context.request_key
            || in_flight.offset != request.offset
            || in_flight.max_bytes != request.max_bytes
            || in_flight.view_epoch != request.context.view_epoch
        {
            return HistoryV2BodySettlement::Stale;
        }

        let mut state = self
            .paging
            .bodies
            .remove(&request.body_key)
            .expect("checked body state above");
        let before_len = state.data.len();
        let outcome = apply_body_chunk(&mut state, request, chunk);
        let settlement = match outcome {
            Ok(received) => HistoryV2BodySettlement::Accepted {
                received,
                complete: state.complete,
            },
            Err(reason) => HistoryV2BodySettlement::Rejected(reason),
        };
        self.paging.retained_body_bytes = self
            .paging
            .retained_body_bytes
            .saturating_sub(before_len)
            .saturating_add(state.data.len());
        self.paging.bodies.insert(request.body_key.clone(), state);
        settlement
    }

    pub(crate) fn cancel_body(&mut self, request_key: DecimalU64) -> bool {
        let Some(state) = self.paging.bodies.values_mut().find(|state| {
            state
                .in_flight
                .as_ref()
                .is_some_and(|in_flight| in_flight.request_key == request_key)
        }) else {
            return false;
        };
        state.in_flight = None;
        true
    }

    pub(crate) fn fail_body(&mut self, request_key: DecimalU64, reason: &str) -> bool {
        let Some(state) = self.paging.bodies.values_mut().find(|state| {
            state
                .in_flight
                .as_ref()
                .is_some_and(|in_flight| in_flight.request_key == request_key)
        }) else {
            return false;
        };
        state.in_flight = None;
        state.failed = Some(reason.to_owned());
        true
    }

    pub(crate) fn cancel_all_bodies(&mut self) {
        for state in self.paging.bodies.values_mut() {
            state.in_flight = None;
        }
    }

    pub(crate) fn clear_bodies(&mut self) {
        self.paging.bodies.clear();
        self.paging.retained_body_bytes = 0;
    }

    /// 页面淘汰后释放不再驻留条目的正文，保证翻页不会无限增长内存。
    pub(crate) fn prune_bodies(&mut self) {
        let keys: BTreeSet<String> = self
            .items
            .iter()
            .flat_map(|entry| {
                entry
                    .sub_items
                    .iter()
                    .filter_map(|sub_item| {
                        sub_item_descriptor(sub_item).map(|descriptor| {
                            body_key(
                                &entry.source,
                                &entry.entry_id,
                                sub_item_id(sub_item),
                                &entry.content_version,
                                &descriptor.body_ref,
                            )
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        self.paging.bodies.retain(|key, _| keys.contains(key));
        self.paging.retained_body_bytes = self
            .paging
            .bodies
            .values()
            .map(|state| state.data.len())
            .sum();
    }

    /// 已物化正文的本地状态；未请求过或已随淘汰释放时为 `None`。
    ///
    /// 查找只使用窗口内已交付的 descriptor 与本地 identity，不接触 wire 令牌。
    pub fn body_for(&self, locator: &HistoryV2BodyLocator) -> Option<&HistoryV2BodyState> {
        let (body_key, _, _, _) = self.locate_body_descriptor_unchecked(locator)?;
        self.paging.bodies.get(&body_key)
    }

    /// 占位展示所需的描述符（总字节/块大小），不要求正文已请求。
    pub fn body_descriptor(
        &self,
        locator: &HistoryV2BodyLocator,
    ) -> Option<peri_acp_types::history_v2::HistoryBodyDescriptor> {
        self.locate_body_descriptor_unchecked(locator)
            .map(|(_, _, _, descriptor)| descriptor)
    }

    fn locate_body_descriptor(
        &self,
        locator: &HistoryV2BodyLocator,
        limits: &HistoryLimits,
    ) -> Option<(
        String,
        String,
        String,
        peri_acp_types::history_v2::HistoryBodyDescriptor,
    )> {
        let (body_key, body_ref, content_version, descriptor) =
            self.locate_body_descriptor_unchecked(locator)?;
        descriptor.validate(limits).ok()?;
        Some((body_key, body_ref, content_version, descriptor))
    }

    fn locate_body_descriptor_unchecked(
        &self,
        locator: &HistoryV2BodyLocator,
    ) -> Option<(
        String,
        String,
        String,
        peri_acp_types::history_v2::HistoryBodyDescriptor,
    )> {
        let entry = self
            .items
            .iter()
            .find(|entry| entry.entry_id == locator.entry_id && entry.source == locator.source)?;
        let sub_item = entry
            .sub_items
            .iter()
            .find(|item| sub_item_id(item) == locator.sub_item_id)?;
        let descriptor = sub_item_descriptor(sub_item)?;
        Some((
            body_key(
                &entry.source,
                &entry.entry_id,
                locator.sub_item_id.as_str(),
                &entry.content_version,
                &descriptor.body_ref,
            ),
            descriptor.body_ref.clone(),
            entry.content_version.clone(),
            descriptor.clone(),
        ))
    }
}

fn body_key(
    source: &HistorySource,
    entry_id: &str,
    sub_item_id: &str,
    content_version: &str,
    body_ref: &str,
) -> String {
    format!(
        "{}|{}|{}|{}",
        entry_identity_key(source, entry_id),
        length_prefixed(sub_item_id),
        length_prefixed(content_version),
        length_prefixed(body_ref)
    )
}

fn length_prefixed(value: &str) -> String {
    format!("{}:{value}", value.len())
}

fn apply_body_chunk(
    state: &mut HistoryV2BodyState,
    request: &HistoryV2BodyRequest,
    chunk: &HistoryBodyChunk,
) -> Result<usize, HistoryV2BodyChunkRejection> {
    if chunk.validate_shape().is_err() {
        return Err(fail(state, HistoryV2BodyChunkRejection::Malformed));
    }
    if chunk.session_id != request.context.session_id
        || chunk.window_id != request.context.window_id
        || chunk.snapshot_token != request.context.snapshot_token
    {
        return Err(fail(state, HistoryV2BodyChunkRejection::Malformed));
    }
    if chunk.body_ref != state.body_ref {
        state.data.clear();
        state.next_offset = DecimalU64::ZERO;
        state.complete = false;
        return Err(fail(state, HistoryV2BodyChunkRejection::Malformed));
    }
    // 页面条目的 contentVersion 在 opaque 路径上只是预估值；正文首块携带服务端对
    // canonical 字节的稳定摘要，作为本次正文流的权威版本。已有字节后必须逐块一致，
    // 避免把不同正文的块拼接起来。
    if state.data.is_empty() && state.next_offset == DecimalU64::ZERO {
        state.content_version = chunk.content_version.clone();
    } else if chunk.content_version != state.content_version {
        state.data.clear();
        state.next_offset = DecimalU64::ZERO;
        state.complete = false;
        return Err(fail(
            state,
            HistoryV2BodyChunkRejection::ContentVersionMismatch,
        ));
    }
    if chunk.representation != state.representation || chunk.encoding != HistoryEncoding::Base64 {
        return Err(fail(
            state,
            HistoryV2BodyChunkRejection::RepresentationMismatch,
        ));
    }
    if chunk.offset != state.next_offset || chunk.offset != request.offset {
        return Err(fail(state, HistoryV2BodyChunkRejection::OffsetMismatch));
    }
    // 描述符的 totalBytes 只是 canonical 预估；首块用服务端权威展示长度修正，
    // 后续块必须一致，否则拒绝拼接。校验全部通过后才写入 state。
    let total = chunk.total_bytes.get();
    if total == 0 {
        return Err(fail(state, HistoryV2BodyChunkRejection::InvalidLength));
    }
    if state.total_authoritative && total != state.total_bytes {
        return Err(fail(state, HistoryV2BodyChunkRejection::InvalidLength));
    }

    let decoded = STANDARD
        .decode(chunk.data.as_bytes())
        .map_err(|_| fail(state, HistoryV2BodyChunkRejection::InvalidEncoding))?;
    if decoded.len() > request.max_bytes as usize {
        return Err(fail(state, HistoryV2BodyChunkRejection::InvalidLength));
    }
    let length = u64::try_from(decoded.len())
        .map_err(|_| fail(state, HistoryV2BodyChunkRejection::InvalidLength))?;
    let end = chunk
        .offset
        .get()
        .checked_add(length)
        .ok_or_else(|| fail(state, HistoryV2BodyChunkRejection::InvalidLength))?;

    if chunk.complete {
        if end != total || chunk.next_offset.is_some() {
            return Err(fail(state, HistoryV2BodyChunkRejection::InvalidLength));
        }
    } else {
        let next = chunk
            .next_offset
            .ok_or_else(|| fail(state, HistoryV2BodyChunkRejection::InvalidLength))?;
        if decoded.is_empty() || next.get() != end || end >= total {
            return Err(fail(state, HistoryV2BodyChunkRejection::InvalidLength));
        }
    }

    state.data.extend_from_slice(&decoded);
    state.next_offset = DecimalU64::new(end);
    state.complete = chunk.complete;
    state.total_bytes = total;
    state.total_authoritative = true;
    state.in_flight = None;
    state.failed = None;
    Ok(decoded.len())
}

fn fail(
    state: &mut HistoryV2BodyState,
    reason: HistoryV2BodyChunkRejection,
) -> HistoryV2BodyChunkRejection {
    state.in_flight = None;
    state.failed = Some(reason.label().to_owned());
    reason
}
