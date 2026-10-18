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

use peri_acp_types::history_window::{HistoryInheritedSnapshot, HistoryWindowError};
use peri_acp_types::messages::MessageId;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, OnceLock};

use super::history::HISTORY_WINDOW_INHERITED_HEADER_SQL;
use super::history_window::inherited_snapshot_of;
use super::*;

/// 扫描/解码窗口。读放大受 [`MAX_ESCAPE_SPAN`] 约束，与正文总大小无关。
const SCAN_WINDOW: usize = 16 * 1024;
/// 解码检查点间隔（逻辑字节）。随机块读取最多回退这么多逻辑字节重新解码。
const CHECKPOINT_BYTES: u64 = 64 * 1024;
/// JSON 转义的最大原始跨度：`\uD800\uDC00` 为 12 字节；留 4 字节余量。
const MAX_ESCAPE_SPAN: usize = 16;
/// 身份前缀：canonical 序列化把 `id` 放在 payload 的前几个字段。
const ID_PREFIX_LIMIT: usize = 2048;
const ID_PREFIX_RAW_LIMIT: usize = 8 * 1024;

const SQLITE_OK: c_int = 0;
const SQLITE_ERROR: c_int = 1;
/// 行被并发 UPDATE/DELETE 或 schema 变化会令 blob 句柄失效（SQLITE_ABORT / SQLITE_SCHEMA）。
const SQLITE_ABORT: c_int = 4;
const SQLITE_SCHEMA: c_int = 17;

extern "C" {
    fn sqlite3_blob_open(
        db: *mut c_void,
        z_db: *const c_char,
        z_table: *const c_char,
        z_column: *const c_char,
        i_row: i64,
        flags: c_int,
        pp_blob: *mut *mut c_void,
    ) -> c_int;
    fn sqlite3_blob_bytes(blob: *mut c_void) -> c_int;
    fn sqlite3_blob_read(blob: *mut c_void, z: *mut c_void, n: c_int, i_offset: c_int) -> c_int;
    fn sqlite3_blob_close(blob: *mut c_void) -> c_int;
}

/// blob 读取失败：`Unsupported` 让调用方回退 SQL，其余是真实的存储/格式故障。
#[derive(Debug)]
pub(super) enum BlobReadError {
    /// 不是 canonical 信封（交给 SQL 路径按原语义解释）。
    Unsupported,
    /// 请求 offset 超出条目长度（调用方映射为参数错误）。
    OffsetBeyondLength,
    /// SQLite 返回的原始错误码。
    Sqlite(c_int),
    /// blob 被并发写入/schema 变化失效；不是内容损坏，调用方应回退 SQL 重读。
    Transient(&'static str),
    /// 信封是 canonical 形状但内容不可解码。
    Corrupt(&'static str),
}

fn cstr(bytes: &'static [u8]) -> *const c_char {
    debug_assert!(bytes.last() == Some(&0));
    bytes.as_ptr().cast()
}

/// 一条已打开的增量 blob。生命周期内不得在同一连接上执行其它语句。
pub(super) struct BlobReader {
    blob: *mut c_void,
    total: u64,
}

impl BlobReader {
    /// 打开 `table.column` 某一行（rowid）的 blob；只读。
    pub(super) fn open(
        db: *mut c_void,
        table: &'static [u8],
        column: &'static [u8],
        rowid: i64,
    ) -> Result<Self, BlobReadError> {
        let mut blob: *mut c_void = ptr::null_mut();
        // SAFETY: `db` 来自持有 `LockedSqliteHandle` 的连接；字符串字面量以 NUL 结尾；
        // `blob` 在失败时保持 null，成功时由本结构体唯一持有并负责关闭。
        let code = unsafe {
            sqlite3_blob_open(
                db,
                cstr(b"main\0"),
                cstr(table),
                cstr(column),
                rowid,
                0,
                &mut blob,
            )
        };
        if code != SQLITE_OK || blob.is_null() {
            return Err(match code {
                SQLITE_OK => BlobReadError::Corrupt("inherited context blob is not readable"),
                SQLITE_ABORT | SQLITE_SCHEMA => BlobReadError::Transient(
                    "inherited context changed while the blob was being opened",
                ),
                _ => BlobReadError::Sqlite(code),
            });
        }
        // SAFETY: 成功打开的 blob 句柄有效。
        let total = unsafe { sqlite3_blob_bytes(blob) };
        if total < 0 {
            // SAFETY: 句柄仍由本结构体独占。
            unsafe {
                sqlite3_blob_close(blob);
            }
            return Err(BlobReadError::Corrupt(
                "inherited context length is negative",
            ));
        }
        Ok(Self {
            blob,
            total: total as u64,
        })
    }

    pub(super) fn total(&self) -> u64 {
        self.total
    }

    /// 读取 `[offset, offset + len)`，越界部分截断（不跨过列尾）。
    pub(super) fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>, BlobReadError> {
        if offset >= self.total || len == 0 {
            return Ok(Vec::new());
        }
        let available = (self.total - offset).min(len as u64) as usize;
        let mut buffer = vec![0u8; available];
        let offset = i32::try_from(offset)
            .map_err(|_| BlobReadError::Corrupt("inherited context offset exceeds blob range"))?;
        let len = i32::try_from(buffer.len())
            .map_err(|_| BlobReadError::Corrupt("inherited context chunk exceeds blob range"))?;
        // SAFETY: `blob` 有效；`buffer` 可写且长度与 `len` 一致。
        let code = unsafe {
            sqlite3_blob_read(self.blob, buffer.as_mut_ptr().cast::<c_void>(), len, offset)
        };
        if code != SQLITE_OK {
            return Err(match code {
                SQLITE_ABORT | SQLITE_SCHEMA => BlobReadError::Transient(
                    "inherited context blob was invalidated by a concurrent write",
                ),
                _ => BlobReadError::Sqlite(code),
            });
        }
        Ok(buffer)
    }

    /// 读取 `[offset, min(offset + len + 余量, end))`；余量只用于保证转义不被窗口截断。
    fn window(&self, offset: u64, len: usize, end: u64) -> Result<Vec<u8>, BlobReadError> {
        let total = (end - offset).min((len + MAX_ESCAPE_SPAN) as u64) as usize;
        self.read(offset, total)
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        // SAFETY: blob 由构造成功时唯一持有，Drop 只执行一次。
        unsafe {
            sqlite3_blob_close(self.blob);
        }
    }
}

/// 一条 payload 在列内的转义原文区间（不含两端引号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PayloadSpan {
    pub(super) raw_start: u64,
    pub(super) raw_len: u64,
}

/// canonical 信封的布局：payload 顺序、原始区间与稳定身份。
pub(super) struct InheritedLayout {
    pub(super) total_bytes: u64,
    payloads: Vec<LayoutPayload>,
}

struct LayoutPayload {
    span: PayloadSpan,
    id: MessageId,
    /// 逻辑长度与检查点；只在块读取时按需构建一次。
    decoded: OnceLock<Arc<PayloadDecoding>>,
}

impl InheritedLayout {
    pub(super) fn len(&self) -> usize {
        self.payloads.len()
    }

    pub(super) fn first_id(&self) -> Option<MessageId> {
        self.payloads.first().map(|payload| payload.id)
    }

    pub(super) fn last_id(&self) -> Option<MessageId> {
        self.payloads.last().map(|payload| payload.id)
    }

    pub(super) fn position_of(&self, entry_id: MessageId) -> Option<usize> {
        self.payloads
            .iter()
            .position(|payload| payload.id == entry_id)
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

/// 扫描 canonical 信封；不支持的结构返回 `Unsupported`，由调用方回退 SQL。
pub(super) fn scan_layout(reader: &BlobReader) -> Result<InheritedLayout, BlobReadError> {
    if reader.total() == 0 {
        return Err(BlobReadError::Unsupported);
    }
    let mut cursor = Cursor::new(reader);
    cursor.skip_ws()?;
    cursor.expect(b'{')?;
    cursor.skip_ws()?;
    cursor.expect_key(b"version")?;
    cursor.skip_ws()?;
    cursor.expect(b':')?;
    cursor.skip_ws()?;
    if cursor.take_number()? != Some(1) {
        return Err(BlobReadError::Unsupported);
    }
    cursor.skip_ws()?;
    cursor.expect(b',')?;
    cursor.skip_ws()?;
    cursor.expect_key(b"payloads")?;
    cursor.skip_ws()?;
    cursor.expect(b':')?;
    cursor.skip_ws()?;
    cursor.expect(b'[')?;

    let mut spans = Vec::new();
    cursor.skip_ws()?;
    if cursor.peek()? == Some(b']') {
        cursor.expect(b']')?;
    } else {
        loop {
            cursor.skip_ws()?;
            cursor.expect(b'"')?;
            let raw_start = cursor.position();
            cursor.skip_string_content()?;
            let raw_end = cursor.position();
            spans.push(PayloadSpan {
                raw_start,
                raw_len: raw_end.saturating_sub(raw_start),
            });
            cursor.expect(b'"')?;
            cursor.skip_ws()?;
            match cursor.take()? {
                Some(b',') => continue,
                Some(b']') => break,
                _ => return Err(BlobReadError::Unsupported),
            }
        }
    }
    // 数组之后仍必须是合法的顶层对象尾部：截断/损坏不得被当成可用快照。
    cursor.verify_document_tail()?;

    let mut payloads = Vec::with_capacity(spans.len());
    for span in spans {
        let Some(id) = read_payload_id(reader, span)? else {
            return Err(BlobReadError::Unsupported);
        };
        payloads.push(LayoutPayload {
            span,
            id,
            decoded: OnceLock::new(),
        });
    }
    Ok(InheritedLayout {
        total_bytes: reader.total(),
        payloads,
    })
}

/// 从 payload 原文的前缀里取首个 `"id":"..."`（两类 payload 的 id 都在正文之前）。
fn read_payload_id(
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
struct Cursor<'a> {
    reader: &'a BlobReader,
    window: Vec<u8>,
    window_start: u64,
    pos: usize,
    pushback: Option<u8>,
}

impl<'a> Cursor<'a> {
    fn new(reader: &'a BlobReader) -> Self {
        Self {
            reader,
            window: Vec::new(),
            window_start: 0,
            pos: 0,
            pushback: None,
        }
    }

    /// 下一次 `take()` 会返回的字节的绝对偏移。
    fn position(&self) -> u64 {
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

    fn peek(&mut self) -> Result<Option<u8>, BlobReadError> {
        if let Some(byte) = self.pushback {
            return Ok(Some(byte));
        }
        if !self.fill()? {
            return Ok(None);
        }
        Ok(Some(self.window[self.pos]))
    }

    fn take(&mut self) -> Result<Option<u8>, BlobReadError> {
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

    fn expect(&mut self, expected: u8) -> Result<(), BlobReadError> {
        match self.take()? {
            Some(byte) if byte == expected => Ok(()),
            _ => Err(BlobReadError::Unsupported),
        }
    }

    fn skip_ws(&mut self) -> Result<(), BlobReadError> {
        while let Some(byte) = self.peek()? {
            if !byte.is_ascii_whitespace() {
                break;
            }
            let _ = self.take()?;
        }
        Ok(())
    }

    fn expect_key(&mut self, key: &[u8]) -> Result<(), BlobReadError> {
        self.expect(b'"')?;
        for &expected in key {
            if self.take()? != Some(expected) {
                return Err(BlobReadError::Unsupported);
            }
        }
        self.expect(b'"')
    }

    fn take_number(&mut self) -> Result<Option<u64>, BlobReadError> {
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
    fn skip_string_content(&mut self) -> Result<(), BlobReadError> {
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
    fn verify_document_tail(&mut self) -> Result<(), BlobReadError> {
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

/// 缓存条目：按线程记住已解析的布局；`Unsupported` 也缓存，避免每次读取都重复扫描。
#[derive(Clone)]
pub(super) enum CachedInheritedLayout {
    Parsed(Arc<InheritedLayout>),
    Unsupported { total_bytes: u64 },
}

/// inherited 事实读取结果：`Unsupported` 表示信封不是 canonical 形状，调用方回退 SQL。
pub(super) enum InheritedFactsRead {
    Facts {
        snapshot: Option<HistoryInheritedSnapshot>,
        layout: Option<Arc<InheritedLayout>>,
    },
    Unsupported,
}

/// 定位与打开 blob 之间只做一次缓存判定/扫描，结果交给外层分类。
enum InheritedProbe {
    Absent,
    Cached(CachedInheritedLayout),
    Parsed(Arc<InheritedLayout>),
    Unsupported,
}

pub(super) fn map_inherited_blob_error(error: BlobReadError) -> SessionResourceError {
    match error {
        BlobReadError::OffsetBeyondLength => {
            HistoryWindowError::invalid_request("body chunk offset is beyond the entry length")
                .into()
        }
        BlobReadError::Corrupt(detail) => corrupt(detail),
        BlobReadError::Sqlite(code) => HistoryWindowError::ReadFailed {
            detail: format!("sqlite blob read failed (code {code})"),
        }
        .into(),
        BlobReadError::Transient(detail) => HistoryWindowError::ReadFailed {
            detail: detail.to_owned(),
        }
        .into(),
        BlobReadError::Unsupported => corrupt("inherited context is not incrementally readable"),
    }
}

impl SqliteSessionData {
    fn cached_inherited_layout(&self, id: &ThreadId, total: u64) -> Option<CachedInheritedLayout> {
        let cache = match self.inherited_layouts.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        match cache.get(id) {
            Some(CachedInheritedLayout::Parsed(layout)) if layout.total_bytes == total => {
                Some(CachedInheritedLayout::Parsed(Arc::clone(layout)))
            }
            Some(CachedInheritedLayout::Unsupported { total_bytes }) if *total_bytes == total => {
                Some(CachedInheritedLayout::Unsupported {
                    total_bytes: *total_bytes,
                })
            }
            _ => None,
        }
    }

    fn store_inherited_layout(&self, id: &ThreadId, entry: CachedInheritedLayout) {
        let mut cache = match self.inherited_layouts.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache.insert(id.clone(), entry);
    }

    /// 在已锁定的连接上执行一次同步 blob 读取；锁在返回前释放。
    pub(super) async fn inherited_blob_scope<T>(
        connection: &mut SqliteConnection,
        rowid: i64,
        read: impl FnOnce(&BlobReader) -> Result<T, BlobReadError>,
    ) -> SessionResourceResult<Result<T, BlobReadError>> {
        let mut locked = connection.lock_handle().await.map_err(|error| {
            SessionResourceError::from(HistoryWindowError::ReadFailed {
                detail: format!("sqlite handle is unavailable: {error}"),
            })
        })?;
        let result = match open_thread_blob(&mut locked, rowid) {
            Ok(reader) => read(&reader),
            Err(error) => Err(error),
        };
        drop(locked);
        Ok(result)
    }

    /// 优先用增量 blob 读取 inherited 事实；不可增量读取时返回 `Unsupported`。
    ///
    /// 布局按 `(thread id, 列总字节数)` 缓存：`inherited_context` 由本 store 的写入入口
    /// 保证 write-once（创建时写入，`WHERE inherited_context IS NULL`），总字节数变化即
    /// 视为新快照并重新扫描。该不变量由 API 维持、数据库没有触发器兜底：绕过 store 的
    /// 同长度改写不会被识别（见 owner 报告的风险项）。这里不解析 JSON、不取正文。
    ///
    /// 总长度由 `sqlite3_blob_bytes` 给出，**不**查询 `length(CAST(...))`：后者会让
    /// SQLite 把整列读进内存，16 MiB 前缀每次读取都会多分配一份正文。
    pub(super) async fn read_inherited_facts_cached(
        &self,
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<InheritedFactsRead> {
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
            return Ok(InheritedFactsRead::Facts {
                snapshot: None,
                layout: None,
            });
        }
        let probed = Self::inherited_blob_scope(connection, rowid, |reader| {
            let total = reader.total();
            if total == 0 {
                return Ok(InheritedProbe::Absent);
            }
            if let Some(cached) = self.cached_inherited_layout(id, total) {
                return Ok(InheritedProbe::Cached(cached));
            }
            match scan_layout(reader) {
                Ok(layout) => {
                    let layout = Arc::new(layout);
                    self.store_inherited_layout(
                        id,
                        CachedInheritedLayout::Parsed(Arc::clone(&layout)),
                    );
                    Ok(InheritedProbe::Parsed(layout))
                }
                Err(BlobReadError::Unsupported) => {
                    self.store_inherited_layout(
                        id,
                        CachedInheritedLayout::Unsupported { total_bytes: total },
                    );
                    Ok(InheritedProbe::Unsupported)
                }
                Err(error) => Err(error),
            }
        })
        .await?;
        match probed {
            Ok(InheritedProbe::Absent) => Ok(InheritedFactsRead::Facts {
                snapshot: None,
                layout: None,
            }),
            Ok(InheritedProbe::Cached(CachedInheritedLayout::Parsed(layout)))
            | Ok(InheritedProbe::Parsed(layout)) => Ok(InheritedFactsRead::Facts {
                snapshot: inherited_snapshot_of(id, parent, &layout),
                layout: Some(layout),
            }),
            Ok(InheritedProbe::Cached(CachedInheritedLayout::Unsupported { .. }))
            | Ok(InheritedProbe::Unsupported) => Ok(InheritedFactsRead::Unsupported),
            // 打开失败（类型不是 TEXT/BLOB、行在定位后消失）或扫描期间 blob 失效：
            // 交给 SQL 语义解释。瞬态错误不写缓存，避免一次竞争永久停用增量路径。
            Err(error) if is_unsupported_blob_error(&error) || is_transient_blob_error(&error) => {
                Ok(InheritedFactsRead::Unsupported)
            }
            Err(error) => Err(map_inherited_blob_error(error)),
        }
    }
}

/// 列读取失败是否只是“这个值不是可增量读取的文本”（调用方回退 SQL）。
pub(super) fn is_unsupported_blob_error(error: &BlobReadError) -> bool {
    matches!(
        error,
        BlobReadError::Unsupported | BlobReadError::Sqlite(SQLITE_ERROR)
    )
}

/// blob 在读取期间被并发写或 schema 变化失效：内容未损坏，可回退 SQL 重读。
pub(super) fn is_transient_blob_error(error: &BlobReadError) -> bool {
    matches!(error, BlobReadError::Transient(_))
}

/// 在已锁定的连接上打开 `threads.inherited_context` 的增量 blob。
pub(super) fn open_thread_blob(
    locked: &mut sqlx::sqlite::LockedSqliteHandle<'_>,
    rowid: i64,
) -> Result<BlobReader, BlobReadError> {
    let db: *mut c_void = locked.as_raw_handle().as_ptr().cast();
    BlobReader::open(db, b"threads\0", b"inherited_context\0", rowid)
}
