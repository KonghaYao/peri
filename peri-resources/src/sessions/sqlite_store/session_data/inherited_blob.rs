//! `threads.inherited_context` 的增量读取：SQLite blob I/O + JSON 字符串流式解码。
//!
//! 为什么不用 SQL 的 `json_each`/`substr`：`inherited_context` 的 payload 是 JSON 数组里的
//! **字符串**，JSON1 必须把整条元素反序列化后才能产出 `je.value`；bundled SQLite 3.46 的
//! `substr()` 也会先物化整个 value。16 MiB 正文的 64 KiB 块读取因此在 SQLite 内产生约
//! 64 MiB 额外分配。这里只在需要时用 `sqlite3_blob_read` 读取列的一个字节区间，并在 Rust
//! 侧按 JSON 转义规则流式解码：内存上界只与请求块大小（及解码检查点密度）相关。
//!
//! 只识别 canonical 信封形状（`serde_json` 的紧凑输出）：
//!
//! ```text
//! {"version":1,"payloads":["<escaped payload>",...],"flags":{...}}
//! ```
//!
//! 任何结构异常都以 [`BlobReadError::Unsupported`] 返回，由调用方回退到原有 SQL 路径：
//! 这是读取优化，不是第二份 JSON 语义权威。payload 身份仍取序列化原文里的首个 `"id"`，
//! 与 SQL 路径的 `$.message.id` / `$.id` 口径一致（两类 payload 的 id 都紧跟在 type 之后）。
//!
//! 校验边界：布局扫描只验证 canonical 信封结构与身份前缀，**不**解析 payload 正文的 JSON
//! 语义（不反序列化、不校验正文转义）。正文合法性由消费/解析路径负责；非 canonical 或
//! 结构异常的信封回退 SQL，后者按原有语义反序列化首尾 payload。

use peri_acp_types::history_window::HistoryWindowError;
use peri_acp_types::messages::MessageId;
use std::sync::Arc;

use super::blob_reader::{
    is_transient_blob_error, is_unsupported_blob_error, open_blob, BlobReadError, BlobReader,
};
use super::history::HISTORY_WINDOW_INHERITED_HEADER_SQL;
use super::history_window::inherited_snapshot_of;
use super::inherited_facts::{
    scan_layout, scan_summary, CachedInheritedFacts, InheritedCacheLookup, InheritedLayout,
    PayloadSpan,
};
use super::*;

/// 扫描/解码窗口。读放大只受 blob 窗口余量约束，与正文总大小无关。
const SCAN_WINDOW: usize = 16 * 1024;
/// 解码检查点间隔（逻辑字节）。随机块读取最多回退这么多逻辑字节重新解码。
const CHECKPOINT_BYTES: u64 = 64 * 1024;
/// 完整正文解码的块大小：与页读取的单条上限无关，只约束解码中间缓冲。
const BODY_DECODE_CHUNK: u32 = 256 * 1024;
/// 身份前缀：canonical 序列化把 `id` 放在 payload 的前几个字段。
const ID_PREFIX_LIMIT: usize = 2048;
const ID_PREFIX_RAW_LIMIT: usize = 8 * 1024;

impl InheritedLayout {
    /// 数组下标处 payload 的**逻辑**序列化长度（JSON 解码后的字节数）。
    ///
    /// 与 SQL 路径 `length(CAST(je.value AS BLOB))` 同口径，但只流式扫描该条 payload，
    /// 不把转义原文或整条正文物化到 SQLite 堆。结果随布局缓存（`OnceLock`）。
    pub(super) fn logical_len(
        &self,
        reader: &BlobReader,
        index: usize,
    ) -> Result<u64, BlobReadError> {
        self.ensure_total(reader)?;
        let payload = self
            .payloads
            .get(index)
            .ok_or(BlobReadError::Corrupt("inherited payload index is missing"))?;
        // 派生索引在写入时记录了逻辑长度：读取候选页时不必为每条正文建解码视图。
        // 索引与列总字节数一致时这一事实随快照 write-once；legacy 回填没有它，
        // 落到按需解码，口径与全列扫描完全相同。
        if let Some(len) = payload.indexed_logical_len {
            return Ok(len);
        }
        Ok(self.decoding_of(reader, index)?.logical_len)
    }

    /// 完整解码一条 payload 的逻辑字节（调用方只在预算允许时使用）。
    pub(super) fn read_body(
        &self,
        reader: &BlobReader,
        index: usize,
    ) -> Result<Vec<u8>, BlobReadError> {
        self.ensure_total(reader)?;
        let decoded = self.decoding_of(reader, index)?;
        let logical_len = usize::try_from(decoded.logical_len)
            .map_err(|_| BlobReadError::Corrupt("inherited payload is too large to decode"))?;
        let mut bytes = Vec::with_capacity(logical_len);
        while (bytes.len() as u64) < decoded.logical_len {
            let chunk = self.read_chunk(reader, index, bytes.len() as u64, BODY_DECODE_CHUNK)?;
            if chunk.bytes.is_empty() {
                return Err(BlobReadError::Corrupt(
                    "inherited payload decoder made no progress",
                ));
            }
            bytes.extend_from_slice(&chunk.bytes);
        }
        if bytes.len() != logical_len {
            return Err(BlobReadError::Corrupt(
                "inherited payload length changed while it was being read",
            ));
        }
        Ok(bytes)
    }

    /// blob 总长是布局的身份：变化说明列被并发改写，不能继续使用该布局。
    fn ensure_total(&self, reader: &BlobReader) -> Result<(), BlobReadError> {
        if reader.total() != self.total_bytes {
            return Err(BlobReadError::Transient(
                "inherited context changed while it was being read",
            ));
        }
        Ok(())
    }

    /// 读取一条 payload 的正文块；`offset` 与请求块都是**字节**，不按字符裁断。
    pub(super) fn read_chunk(
        &self,
        reader: &BlobReader,
        index: usize,
        offset: u64,
        max_bytes: u32,
    ) -> Result<PayloadChunk, BlobReadError> {
        let payload = self
            .payloads
            .get(index)
            .ok_or(BlobReadError::Corrupt("inherited payload index is missing"))?;
        if reader.total() != self.total_bytes {
            return Err(BlobReadError::Transient(
                "inherited context changed while it was being read",
            ));
        }
        let decoded = self.decoding_of(reader, index)?;
        if offset > decoded.logical_len {
            return Err(BlobReadError::OffsetBeyondLength);
        }
        if offset == decoded.logical_len || max_bytes == 0 {
            return Ok(PayloadChunk {
                offset,
                bytes: Vec::new(),
                next_offset: None,
                complete: true,
            });
        }
        let checkpoint = decoded.checkpoint_for(offset);
        let mut raw = checkpoint.raw;
        let mut skip = offset - checkpoint.logical;
        let end = payload.span.raw_start + payload.span.raw_len;
        let mut bytes =
            Vec::with_capacity((max_bytes as u64).min(decoded.logical_len - offset) as usize);
        'window: while raw < end {
            let want = ((end - raw) as usize).min(SCAN_WINDOW);
            let window = reader.window(raw, want, end)?;
            let mut pos = 0usize;
            while pos < window.len() {
                let Some(unit) = next_unit(&window, &mut pos)? else {
                    break;
                };
                let unit_len = unit.decoded_len;
                if skip >= unit_len as u64 {
                    skip -= unit_len as u64;
                    continue;
                }
                let from = skip as usize;
                skip = 0;
                let available = unit_len - from;
                let room = max_bytes as usize - bytes.len();
                if available > room {
                    // 字节块允许切在多字节符/转义中间：本块取满，剩余部分由下一次请求
                    // 从检查点重新解码后跳过 `offset + bytes.len()` 个逻辑字节取到。
                    bytes.extend_from_slice(&unit.bytes[from..from + room]);
                    break 'window;
                }
                bytes.extend_from_slice(&unit.bytes[from..unit_len]);
            }
            if pos == 0 {
                break;
            }
            raw += pos as u64;
            if bytes.len() >= max_bytes as usize {
                break;
            }
        }
        let end_offset = offset + bytes.len() as u64;
        let complete = end_offset >= decoded.logical_len;
        if !complete && bytes.is_empty() {
            return Err(BlobReadError::Corrupt(
                "inherited payload decoder made no progress",
            ));
        }
        Ok(PayloadChunk {
            offset,
            bytes,
            next_offset: (!complete).then_some(end_offset),
            complete,
        })
    }

    fn decoding_of(
        &self,
        reader: &BlobReader,
        index: usize,
    ) -> Result<Arc<PayloadDecoding>, BlobReadError> {
        let payload = &self.payloads[index];
        if let Some(decoded) = payload.decoded.get() {
            return Ok(Arc::clone(decoded));
        }
        let decoded = Arc::new(decode_payload(reader, payload.span)?);
        let _ = payload.decoded.set(Arc::clone(&decoded));
        Ok(decoded)
    }
}

/// 已解码 payload 的字节长度与检查点（`(逻辑偏移, 原始偏移)`，按逻辑偏移升序）。
pub(super) struct PayloadDecoding {
    logical_len: u64,
    checkpoints: Vec<Checkpoint>,
}

#[derive(Clone, Copy)]
struct Checkpoint {
    logical: u64,
    raw: u64,
}

impl PayloadDecoding {
    fn checkpoint_for(&self, offset: u64) -> Checkpoint {
        let index = self
            .checkpoints
            .partition_point(|checkpoint| checkpoint.logical <= offset);
        self.checkpoints[index.saturating_sub(1)]
    }
}

pub(super) struct PayloadChunk {
    pub(super) offset: u64,
    pub(super) bytes: Vec<u8>,
    pub(super) next_offset: Option<u64>,
    pub(super) complete: bool,
}

/// 从 payload 原文的前缀里取首个 `"id":"..."`（两类 payload 的 id 都在正文之前）。
pub(super) fn read_payload_id(
    reader: &BlobReader,
    span: PayloadSpan,
) -> Result<Option<MessageId>, BlobReadError> {
    let raw_len = (span.raw_len as usize).min(ID_PREFIX_RAW_LIMIT);
    let raw = reader.read(span.raw_start, raw_len)?;
    let decoded = decode_prefix(&raw, ID_PREFIX_LIMIT)?;
    let Some(marker) = find_subslice(&decoded, b"\"id\":\"") else {
        return Ok(None);
    };
    let rest = &decoded[marker + 6..];
    if rest.len() < 36 {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(&rest[..36]) else {
        return Ok(None);
    };
    let Ok(uuid) = uuid::Uuid::parse_str(text) else {
        return Ok(None);
    };
    Ok(Some(MessageId::from(uuid)))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(super) fn decode_prefix(raw: &[u8], limit: usize) -> Result<Vec<u8>, BlobReadError> {
    let mut decoded = Vec::with_capacity(limit.min(raw.len()));
    let mut pos = 0usize;
    while pos < raw.len() && decoded.len() < limit {
        match next_unit(raw, &mut pos)? {
            Some(unit) => {
                let room = limit - decoded.len();
                let take = room.min(unit.decoded_len);
                decoded.extend_from_slice(&unit.bytes[..take]);
                if take < unit.decoded_len {
                    break;
                }
            }
            None => break,
        }
    }
    Ok(decoded)
}

/// 为一条 payload 建立完整解码视图：逻辑长度 + 检查点。流式读取，内存与正文大小无关。
fn decode_payload(
    reader: &BlobReader,
    span: PayloadSpan,
) -> Result<PayloadDecoding, BlobReadError> {
    let end = span.raw_start + span.raw_len;
    let mut raw = span.raw_start;
    let mut logical = 0u64;
    let mut next_checkpoint = CHECKPOINT_BYTES;
    let mut checkpoints = vec![Checkpoint {
        logical: 0,
        raw: span.raw_start,
    }];
    while raw < end {
        let want = ((end - raw) as usize).min(SCAN_WINDOW);
        let window = reader.window(raw, want, end)?;
        let mut pos = 0usize;
        while pos < window.len() {
            let Some(unit) = next_unit(&window, &mut pos)? else {
                break;
            };
            logical += unit.decoded_len as u64;
            if logical >= next_checkpoint {
                checkpoints.push(Checkpoint {
                    logical,
                    raw: raw + pos as u64,
                });
                next_checkpoint = logical + CHECKPOINT_BYTES;
            }
        }
        if pos == 0 {
            return Err(BlobReadError::Corrupt(
                "inherited payload decoder made no progress",
            ));
        }
        raw += pos as u64;
    }
    Ok(PayloadDecoding {
        logical_len: logical,
        checkpoints,
    })
}

pub(super) struct DecodedUnit {
    pub(super) bytes: [u8; 4],
    pub(super) decoded_len: usize,
}

/// 解码一个 JSON 字符串单元（普通字节或完整转义）。
///
/// `None` 表示当前窗口在转义中间截断；调用方只在文档末尾接受这种情况（并视为损坏）。
pub(super) fn next_unit(raw: &[u8], pos: &mut usize) -> Result<Option<DecodedUnit>, BlobReadError> {
    let Some(&byte) = raw.get(*pos) else {
        return Ok(None);
    };
    if byte != b'\\' {
        *pos += 1;
        return Ok(Some(DecodedUnit {
            bytes: [byte, 0, 0, 0],
            decoded_len: 1,
        }));
    }
    let Some(&escape) = raw.get(*pos + 1) else {
        return Ok(None);
    };
    let simple = match escape {
        b'"' => Some(b'"'),
        b'\\' => Some(b'\\'),
        b'/' => Some(b'/'),
        b'b' => Some(0x08),
        b'f' => Some(0x0c),
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        _ => None,
    };
    if let Some(byte) = simple {
        *pos += 2;
        return Ok(Some(DecodedUnit {
            bytes: [byte, 0, 0, 0],
            decoded_len: 1,
        }));
    }
    if escape != b'u' {
        return Err(BlobReadError::Corrupt(
            "inherited payload has an invalid escape",
        ));
    }
    let Some(first_digits) = raw.get(*pos + 2..*pos + 6) else {
        return Ok(None);
    };
    let Some(first) = parse_hex4(first_digits) else {
        return Err(BlobReadError::Corrupt(
            "inherited payload has an invalid unicode escape",
        ));
    };
    if (0xD800..=0xDBFF).contains(&first) {
        let Some(&next_escape) = raw.get(*pos + 6) else {
            return Ok(None);
        };
        let Some(&low) = raw.get(*pos + 7) else {
            return Ok(None);
        };
        if next_escape != b'\\' || low != b'u' {
            return Err(BlobReadError::Corrupt(
                "inherited payload has an unpaired surrogate",
            ));
        }
        let Some(second_digits) = raw.get(*pos + 8..*pos + 12) else {
            return Ok(None);
        };
        let Some(second) = parse_hex4(second_digits) else {
            return Err(BlobReadError::Corrupt(
                "inherited payload has an invalid unicode escape",
            ));
        };
        if !(0xDC00..=0xDFFF).contains(&second) {
            return Err(BlobReadError::Corrupt(
                "inherited payload has an unpaired surrogate",
            ));
        }
        let scalar = 0x10000 + (((first as u32 - 0xD800) << 10) | (second as u32 - 0xDC00));
        let mut bytes = [0u8; 4];
        let len = encode_utf8(scalar, &mut bytes).ok_or(BlobReadError::Corrupt(
            "inherited payload has invalid unicode",
        ))?;
        *pos += 12;
        return Ok(Some(DecodedUnit {
            bytes,
            decoded_len: len,
        }));
    }
    if (0xDC00..=0xDFFF).contains(&first) {
        return Err(BlobReadError::Corrupt(
            "inherited payload has an unpaired surrogate",
        ));
    }
    let mut bytes = [0u8; 4];
    let len = encode_utf8(first as u32, &mut bytes).ok_or(BlobReadError::Corrupt(
        "inherited payload has invalid unicode",
    ))?;
    *pos += 6;
    Ok(Some(DecodedUnit {
        bytes,
        decoded_len: len,
    }))
}

fn parse_hex4(digits: &[u8]) -> Option<u16> {
    let mut value = 0u16;
    for &digit in digits {
        value = (value << 4)
            | match digit {
                b'0'..=b'9' => (digit - b'0') as u16,
                b'a'..=b'f' => (digit - b'a' + 10) as u16,
                b'A'..=b'F' => (digit - b'A' + 10) as u16,
                _ => return None,
            };
    }
    Some(value)
}

fn encode_utf8(scalar: u32, out: &mut [u8; 4]) -> Option<usize> {
    let character = char::from_u32(scalar)?;
    Some(character.encode_utf8(out).len())
}

/// 顺序扫描列内容的小游标；每次只保留一个窗口。
pub(super) struct Cursor<'a> {
    reader: &'a BlobReader,
    window: Vec<u8>,
    window_start: u64,
    pos: usize,
    pushback: Option<u8>,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(reader: &'a BlobReader) -> Self {
        Self {
            reader,
            window: Vec::new(),
            window_start: 0,
            pos: 0,
            pushback: None,
        }
    }

    /// 下一次 `take()` 会返回的字节的绝对偏移。
    pub(super) fn position(&self) -> u64 {
        let base = self.window_start + self.pos as u64;
        if self.pushback.is_some() {
            base.saturating_sub(1)
        } else {
            base
        }
    }

    fn fill(&mut self) -> Result<bool, BlobReadError> {
        if self.pos < self.window.len() {
            return Ok(true);
        }
        let next = self.window_start + self.window.len() as u64;
        if next >= self.reader.total() {
            return Ok(false);
        }
        let want = ((self.reader.total() - next) as usize).min(SCAN_WINDOW);
        self.window = self.reader.read(next, want)?;
        self.window_start = next;
        self.pos = 0;
        Ok(!self.window.is_empty())
    }

    pub(super) fn peek(&mut self) -> Result<Option<u8>, BlobReadError> {
        if let Some(byte) = self.pushback {
            return Ok(Some(byte));
        }
        if !self.fill()? {
            return Ok(None);
        }
        Ok(Some(self.window[self.pos]))
    }

    pub(super) fn take(&mut self) -> Result<Option<u8>, BlobReadError> {
        if let Some(byte) = self.pushback.take() {
            return Ok(Some(byte));
        }
        if !self.fill()? {
            return Ok(None);
        }
        let byte = self.window[self.pos];
        self.pos += 1;
        Ok(Some(byte))
    }

    pub(super) fn expect(&mut self, expected: u8) -> Result<(), BlobReadError> {
        match self.take()? {
            Some(byte) if byte == expected => Ok(()),
            _ => Err(BlobReadError::Unsupported),
        }
    }

    pub(super) fn skip_ws(&mut self) -> Result<(), BlobReadError> {
        while let Some(byte) = self.peek()? {
            if !byte.is_ascii_whitespace() {
                break;
            }
            let _ = self.take()?;
        }
        Ok(())
    }

    pub(super) fn expect_key(&mut self, key: &[u8]) -> Result<(), BlobReadError> {
        self.expect(b'"')?;
        for &expected in key {
            if self.take()? != Some(expected) {
                return Err(BlobReadError::Unsupported);
            }
        }
        self.expect(b'"')
    }

    pub(super) fn take_number(&mut self) -> Result<Option<u64>, BlobReadError> {
        let mut value: Option<u64> = None;
        while let Some(byte) = self.peek()? {
            if !byte.is_ascii_digit() {
                break;
            }
            let _ = self.take()?;
            value = Some(
                value
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add((byte - b'0') as u64),
            );
        }
        Ok(value)
    }

    /// 跳过一个 JSON 字符串的内容，停在结束引号之前；正确处理 `\"`/`\\`。
    pub(super) fn skip_string_content(&mut self) -> Result<(), BlobReadError> {
        let mut escaped = false;
        while let Some(byte) = self.take()? {
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' => {
                    self.pushback = Some(byte);
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(BlobReadError::Unsupported)
    }

    /// 数组之后：验证剩余部分是平衡的对象尾部并到达文档末尾。
    pub(super) fn verify_document_tail(&mut self) -> Result<(), BlobReadError> {
        let mut depth = 1i32; // 顶层对象尚未闭合
        let mut in_string = false;
        let mut escaped = false;
        while let Some(byte) = self.take()? {
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        // 顶层对象闭合后只允许空白。
                        while let Some(rest) = self.peek()? {
                            if !rest.is_ascii_whitespace() {
                                return Err(BlobReadError::Unsupported);
                            }
                            let _ = self.take()?;
                        }
                        return Ok(());
                    }
                }
                _ => {}
            }
            if depth < 0 {
                return Err(BlobReadError::Unsupported);
            }
        }
        Err(BlobReadError::Unsupported)
    }
}

pub(super) use super::inherited_read::{
    fallback_exceeds_bound, inherited_index_not_ready, map_inherited_blob_error,
    InheritedFactsRead, InheritedSummaryRead, LayoutProbe, SummaryProbe,
};

impl SqliteSessionData {
    fn inherited_cache_lookup(&self, id: &ThreadId, total: u64) -> InheritedCacheLookup {
        let mut cache = match self.inherited_facts.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache.lookup(id, total)
    }

    fn store_inherited_facts(&self, id: &ThreadId, entry: CachedInheritedFacts) {
        let mut cache = match self.inherited_facts.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache.store(id, entry);
    }

    /// 定位 inherited 列；SQL NULL 返回 `None`，行不存在是 `NotFound`。
    ///
    /// 不在这里取列长度：总长度由 blob 句柄给出，避免 `length(CAST(...))` 物化整列。
    async fn inherited_header(
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<(i64, Option<String>)>> {
        let row: Option<(i64, Option<String>, String)> =
            sqlx::query_as(HISTORY_WINDOW_INHERITED_HEADER_SQL)
                .bind(id.as_str())
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx(&error))?;
        let Some((rowid, parent, value_type)) = row else {
            return Err(not_found());
        };
        // NULL 列没有可读取的值；空 TEXT/BLOB 由 blob 句柄报告 0 字节。
        if value_type == "null" {
            return Ok(None);
        }
        Ok(Some((rowid, parent)))
    }

    /// 在已锁定的连接上执行一次同步 blob 读取；锁在返回前释放。
    ///
    /// `table`/`column` 必须是带 NUL 的静态字节串：blob 句柄存活期间不能再执行其它语句。
    pub(super) async fn blob_scope<T>(
        connection: &mut SqliteConnection,
        table: &'static [u8],
        column: &'static [u8],
        rowid: i64,
        read: impl FnOnce(&BlobReader) -> Result<T, BlobReadError>,
    ) -> SessionResourceResult<Result<T, BlobReadError>> {
        let mut locked = connection.lock_handle().await.map_err(|error| {
            SessionResourceError::from(HistoryWindowError::ReadFailed {
                detail: format!("sqlite handle is unavailable: {error}"),
            })
        })?;
        let result = match open_blob(&mut locked, table, column, rowid) {
            Ok(reader) => read(&reader),
            Err(error) => Err(error),
        };
        drop(locked);
        Ok(result)
    }

    /// inherited 冻结前缀的 blob 读取；语义同 [`Self::blob_scope`]。
    pub(super) async fn inherited_blob_scope<T>(
        connection: &mut SqliteConnection,
        rowid: i64,
        read: impl FnOnce(&BlobReader) -> Result<T, BlobReadError>,
    ) -> SessionResourceResult<Result<T, BlobReadError>> {
        Self::blob_scope(
            connection,
            b"threads\0",
            b"inherited_context\0",
            rowid,
            read,
        )
        .await
    }

    /// 只取 inherited 列总字节数；blob 句柄在这次调用结束时释放。
    ///
    /// 热路径用它先查内存缓存：命中时完全不碰派生索引，避免为一次缓存命中
    /// 读出该快照的全部 `inherited_payload_index` 行（R07）。
    async fn inherited_blob_total(
        connection: &mut SqliteConnection,
        rowid: i64,
    ) -> SessionResourceResult<Result<u64, BlobReadError>> {
        Self::inherited_blob_scope(connection, rowid, |reader| Ok(reader.total())).await
    }

    /// 读取 inherited 轻量摘要；bounds 只需要首尾身份，不构造全量布局。
    ///
    /// 缓存按 `(thread id, 列总字节数)` 有界保存：`inherited_context` 由本 store 的写入
    /// 入口保证 write-once（创建时写入，`WHERE inherited_context IS NULL`），总字节数变化
    /// 即视为新快照并重新扫描。该不变量由 API 维持、数据库没有触发器兜底：绕过 store 的
    /// 同长度改写不会被识别。
    ///
    /// 派生索引命中时只读快照行与首尾身份，不做全列扫描；索引缺失、损坏或列总字节数
    /// 不符时回退 [`scan_summary`]，行为与引入索引前相同。
    ///
    /// 读取顺序是**先缓存、后索引**：blob 句柄存活期间不能执行其它 SQL，因此先用一次
    /// blob 打开取总字节数并查缓存，命中立即返回；miss 后释放句柄，再读索引并重新打开
    /// blob 复核长度。索引行不参与缓存命中路径。
    ///
    /// 总长度由 `sqlite3_blob_bytes` 给出，**不**查询 `length(CAST(...))`：后者会让
    /// SQLite 把整列读进内存，16 MiB 前缀每次读取都会多分配一份正文。
    pub(super) async fn read_inherited_summary_cached(
        &self,
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<InheritedSummaryRead> {
        let Some((rowid, parent)) = Self::inherited_header(connection, id).await? else {
            return Ok(InheritedSummaryRead::Facts { snapshot: None });
        };
        let total = match Self::inherited_blob_total(connection, rowid).await? {
            Ok(total) => total,
            Err(error) if is_unsupported_blob_error(&error) || is_transient_blob_error(&error) => {
                return Ok(InheritedSummaryRead::Unsupported);
            }
            Err(error) => return Err(map_inherited_blob_error(error)),
        };
        if total == 0 {
            return Ok(InheritedSummaryRead::Facts { snapshot: None });
        }
        match self.inherited_cache_lookup(id, total) {
            InheritedCacheLookup::Unsupported => return Ok(InheritedSummaryRead::Unsupported),
            InheritedCacheLookup::Summary(summary) => {
                return Ok(InheritedSummaryRead::Facts {
                    snapshot: inherited_snapshot_of(id, parent, &summary),
                });
            }
            InheritedCacheLookup::Layout(layout) => {
                return Ok(InheritedSummaryRead::Facts {
                    snapshot: inherited_snapshot_of(id, parent, &layout.summary()),
                });
            }
            InheritedCacheLookup::Miss => {}
        }
        // 缓存 miss 才读派生索引；此时首个 blob 句柄已经释放。
        let indexed = super::inherited_index::read(connection, id).await?;
        let probed = Self::inherited_blob_scope(connection, rowid, |reader| {
            let total = reader.total();
            if total == 0 {
                return Ok(SummaryProbe::Absent);
            }
            // 两次打开之间快照可能变化：按本次长度复核缓存，命中就不再使用索引。
            match self.inherited_cache_lookup(id, total) {
                InheritedCacheLookup::Unsupported => {
                    return Ok(SummaryProbe::Unsupported);
                }
                InheritedCacheLookup::Summary(summary) => {
                    return Ok(SummaryProbe::Cached(summary));
                }
                InheritedCacheLookup::Layout(layout) => {
                    return Ok(SummaryProbe::Cached(Arc::new(layout.summary())));
                }
                InheritedCacheLookup::Miss => {}
            }
            if let Some(indexed) = indexed
                .as_ref()
                .filter(|indexed| indexed.validate(id, parent.as_deref(), total))
            {
                let summary = Arc::new(indexed.summary());
                self.store_inherited_facts(id, CachedInheritedFacts::Summary(Arc::clone(&summary)));
                return Ok(SummaryProbe::Cached(summary));
            }
            // 派生索引缺失/不完整时，回退扫描必须先过有界门禁；超过预算不做全列扫描。
            let entry_count = indexed.as_ref().map(|indexed| indexed.entry_count);
            if fallback_exceeds_bound(total, entry_count) {
                return Ok(SummaryProbe::IndexNotReady {
                    total_bytes: total,
                    entry_count,
                });
            }
            match scan_summary(reader) {
                Ok(summary) => {
                    let summary = Arc::new(summary);
                    self.store_inherited_facts(
                        id,
                        CachedInheritedFacts::Summary(Arc::clone(&summary)),
                    );
                    Ok(SummaryProbe::Parsed(summary))
                }
                Err(BlobReadError::Unsupported) => {
                    self.store_inherited_facts(
                        id,
                        CachedInheritedFacts::Unsupported { total_bytes: total },
                    );
                    Ok(SummaryProbe::Unsupported)
                }
                Err(error) => Err(error),
            }
        })
        .await?;
        match probed {
            Ok(SummaryProbe::Absent) => Ok(InheritedSummaryRead::Facts { snapshot: None }),
            Ok(SummaryProbe::Cached(summary)) | Ok(SummaryProbe::Parsed(summary)) => {
                Ok(InheritedSummaryRead::Facts {
                    snapshot: inherited_snapshot_of(id, parent, &summary),
                })
            }
            Ok(SummaryProbe::Unsupported) => Ok(InheritedSummaryRead::Unsupported),
            Ok(SummaryProbe::IndexNotReady {
                total_bytes,
                entry_count,
            }) => Err(inherited_index_not_ready(total_bytes, entry_count)),
            // 打开失败（类型不是 TEXT/BLOB、行在定位后消失）或扫描期间 blob 失效：
            // 交给 SQL 语义解释。瞬态错误不写缓存，避免一次竞争永久停用增量路径。
            Err(error) if is_unsupported_blob_error(&error) || is_transient_blob_error(&error) => {
                Ok(InheritedSummaryRead::Unsupported)
            }
            Err(error) => Err(map_inherited_blob_error(error)),
        }
    }

    /// 读取全量布局；只有需要位置/分页/正文时才走这里。
    ///
    /// 缓存语义与 summary 相同；已有 layout 直接复用，已有 summary 则改走派生索引或
    /// 重新扫描。索引缺失时按本次扫描结果回填一次（只作用于当前快照，不启动后台任务）。
    /// 读取顺序同样是先查缓存、miss 后才读派生索引（R07）。
    pub(super) async fn read_inherited_layout_cached(
        &self,
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<InheritedFactsRead> {
        let Some((rowid, parent)) = Self::inherited_header(connection, id).await? else {
            return Ok(InheritedFactsRead::Facts {
                snapshot: None,
                layout: None,
            });
        };
        let total = match Self::inherited_blob_total(connection, rowid).await? {
            Ok(total) => total,
            Err(error) if is_unsupported_blob_error(&error) || is_transient_blob_error(&error) => {
                return Ok(InheritedFactsRead::Unsupported);
            }
            Err(error) => return Err(map_inherited_blob_error(error)),
        };
        if total == 0 {
            return Ok(InheritedFactsRead::Facts {
                snapshot: None,
                layout: None,
            });
        }
        match self.inherited_cache_lookup(id, total) {
            InheritedCacheLookup::Unsupported => return Ok(InheritedFactsRead::Unsupported),
            InheritedCacheLookup::Layout(layout) => {
                return Ok(InheritedFactsRead::Facts {
                    snapshot: inherited_snapshot_of(id, parent, &layout.summary()),
                    layout: Some(layout),
                });
            }
            // summary 不含数组顺序，必须走索引或重新扫描成完整布局。
            InheritedCacheLookup::Summary(_) | InheritedCacheLookup::Miss => {}
        }
        // 缓存 miss（或只有 summary）才读派生索引；此时首个 blob 句柄已经释放。
        let indexed = super::inherited_index::read(connection, id).await?;
        let probed = Self::inherited_blob_scope(connection, rowid, |reader| {
            let total = reader.total();
            if total == 0 {
                return Ok(LayoutProbe::Absent);
            }
            // 两次打开之间快照可能变化：按本次长度复核缓存。
            match self.inherited_cache_lookup(id, total) {
                InheritedCacheLookup::Unsupported => {
                    return Ok(LayoutProbe::Unsupported);
                }
                InheritedCacheLookup::Layout(layout) => {
                    return Ok(LayoutProbe::Cached(layout));
                }
                // summary 不含数组顺序，必须走索引或重新扫描成完整布局。
                InheritedCacheLookup::Summary(_) | InheritedCacheLookup::Miss => {}
            }
            if let Some(indexed) = indexed
                .as_ref()
                .filter(|indexed| indexed.validate(id, parent.as_deref(), total))
            {
                let layout = Arc::new(indexed.to_layout());
                self.store_inherited_facts(id, CachedInheritedFacts::Layout(Arc::clone(&layout)));
                return Ok(LayoutProbe::Cached(layout));
            }
            // 派生索引缺失/不完整时，回退扫描必须先过有界门禁；超过预算不做全列扫描。
            let entry_count = indexed.as_ref().map(|indexed| indexed.entry_count);
            if fallback_exceeds_bound(total, entry_count) {
                return Ok(LayoutProbe::IndexNotReady {
                    total_bytes: total,
                    entry_count,
                });
            }
            match scan_layout(reader) {
                Ok(layout) => {
                    let layout = Arc::new(layout);
                    self.store_inherited_facts(
                        id,
                        CachedInheritedFacts::Layout(Arc::clone(&layout)),
                    );
                    Ok(LayoutProbe::Parsed(layout))
                }
                Err(BlobReadError::Unsupported) => {
                    self.store_inherited_facts(
                        id,
                        CachedInheritedFacts::Unsupported { total_bytes: total },
                    );
                    Ok(LayoutProbe::Unsupported)
                }
                Err(error) => Err(error),
            }
        })
        .await?;
        // legacy 回填：扫描已经付出 O(列总字节) 的代价，这里只把结果落成派生事实；
        // 只读库不写，索引写入失败也不改变本次读取结果。
        if let Ok(LayoutProbe::Parsed(layout)) = &probed {
            if !self.database.is_read_only() {
                super::inherited_index::backfill_from_layout(
                    connection,
                    id,
                    parent.as_deref(),
                    layout,
                )
                .await;
            }
        }
        match probed {
            Ok(LayoutProbe::Absent) => Ok(InheritedFactsRead::Facts {
                snapshot: None,
                layout: None,
            }),
            Ok(LayoutProbe::Cached(layout)) | Ok(LayoutProbe::Parsed(layout)) => {
                Ok(InheritedFactsRead::Facts {
                    snapshot: inherited_snapshot_of(id, parent, &layout.summary()),
                    layout: Some(layout),
                })
            }
            Ok(LayoutProbe::Unsupported) => Ok(InheritedFactsRead::Unsupported),
            Ok(LayoutProbe::IndexNotReady {
                total_bytes,
                entry_count,
            }) => Err(inherited_index_not_ready(total_bytes, entry_count)),
            Err(error) if is_unsupported_blob_error(&error) || is_transient_blob_error(&error) => {
                Ok(InheritedFactsRead::Unsupported)
            }
            Err(error) => Err(map_inherited_blob_error(error)),
        }
    }
}
