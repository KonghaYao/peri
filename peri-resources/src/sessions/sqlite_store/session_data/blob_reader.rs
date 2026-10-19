//! SQLite 增量 blob 读取原语。
//!
//! 用 `sqlite3_blob_open` 打开某一行的列值，再按字节区间读取：内存上界只与请求窗口
//! （及转义余量）相关，不随列总大小增长。JSON 信封结构与 payload 解码由
//! `inherited_blob` 负责；这里只提供 I/O 与错误分类。
//!
//! 错误分类决定调用方能否回退 SQL：`Unsupported`/`SQLITE_ERROR` 表示该值不可增量读取，
//! `Transient` 表示并发写或 schema 变化使句柄失效、内容未损坏。

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

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

/// JSON 转义的最大原始跨度：`\uD800\uDC00` 为 12 字节；留 4 字节余量。
const MAX_ESCAPE_SPAN: usize = 16;

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
                SQLITE_OK => BlobReadError::Corrupt("column blob is not readable"),
                SQLITE_ABORT | SQLITE_SCHEMA => {
                    BlobReadError::Transient("column changed while the blob was being opened")
                }
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
            return Err(BlobReadError::Corrupt("column blob length is negative"));
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
            .map_err(|_| BlobReadError::Corrupt("blob offset exceeds blob range"))?;
        let len = i32::try_from(buffer.len())
            .map_err(|_| BlobReadError::Corrupt("blob chunk exceeds blob range"))?;
        // SAFETY: `blob` 有效；`buffer` 可写且长度与 `len` 一致。
        let code = unsafe {
            sqlite3_blob_read(self.blob, buffer.as_mut_ptr().cast::<c_void>(), len, offset)
        };
        if code != SQLITE_OK {
            return Err(match code {
                SQLITE_ABORT | SQLITE_SCHEMA => {
                    BlobReadError::Transient("column blob was invalidated by a concurrent write")
                }
                _ => BlobReadError::Sqlite(code),
            });
        }
        Ok(buffer)
    }

    /// 读取 `[offset, min(offset + len + 余量, end))`；余量只用于保证转义不被窗口截断。
    pub(super) fn window(
        &self,
        offset: u64,
        len: usize,
        end: u64,
    ) -> Result<Vec<u8>, BlobReadError> {
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

/// 在已锁定的连接上打开任意表/列的增量 blob。
pub(super) fn open_blob(
    locked: &mut sqlx::sqlite::LockedSqliteHandle<'_>,
    table: &'static [u8],
    column: &'static [u8],
    rowid: i64,
) -> Result<BlobReader, BlobReadError> {
    let db: *mut c_void = locked.as_raw_handle().as_ptr().cast();
    BlobReader::open(db, table, column, rowid)
}
