//! 图片来源身份：本地文件（路径 + 版本）与进程内上传（内容摘要）。
//!
//! 摘要即身份：粘贴/工具图片入库时用 FNV-1a 算一次 64 位摘要（进程内、不落盘、
//! 不跨进程），视图模型只携带摘要与媒体类型，base64 永远留在
//! [`super::cache`] 的上传仓库里。

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use peri_tui_components::markdown::{UrlKind, classify_url};

/// 渲染/请求两侧共用的图片来源。
#[derive(Debug, Clone, Copy)]
pub enum ImageSource<'a> {
    /// 本地路径或 `file://`（远程 URL 与危险 scheme 由解析侧拒绝）。
    File(&'a str),
    /// 提交期登记的内容摘要。
    Uploaded(UploadId),
}

/// 上传图片身份：内容摘要 + 媒体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UploadId {
    pub digest: u64,
    pub media: UploadMedia,
}

/// 图片媒体类型（缓存键与展示文案用，不参与解码判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UploadMedia {
    Png,
    Jpeg,
    Gif,
    Webp,
    Other,
}

impl UploadMedia {
    /// MIME → 媒体枚举；未知类型归 [`Self::Other`]。
    #[must_use]
    pub fn from_mime(mime: &str) -> Self {
        match mime.trim().to_ascii_lowercase().as_str() {
            "image/png" => Self::Png,
            "image/jpeg" | "image/jpg" => Self::Jpeg,
            "image/gif" => Self::Gif,
            "image/webp" => Self::Webp,
            _ => Self::Other,
        }
    }

    /// 稳定的 MIME 文案（日志与降级 meta 行用）。
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Other => "image/*",
        }
    }
}

impl UploadId {
    /// 由标准 base64 载荷与 MIME 构造身份（摘要只算一次）。
    #[must_use]
    pub fn from_payload(base64_data: &str, media_type: &str) -> Self {
        Self {
            digest: digest_of(base64_data),
            media: UploadMedia::from_mime(media_type),
        }
    }
}

/// FNV-1a 64 位摘要（进程内身份，避免为几 MB 载荷引入新依赖）。
#[must_use]
pub fn digest_of(data: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in data.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 渲染/请求两侧共用的 alias 键：来源文本 + 预算，不含文件版本与单元格像素
/// （版本/字体变化由 `CacheKey` 表达，alias 指向当前版本）。
///
/// 预算必须进键：同一图片在转录区（112×8）、用户气泡（112×6）、工具卡
/// （过程区×8）各有不同目标格数，混用会拿到错误的编码尺寸。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SourceRef {
    File {
        source: String,
        cols: u16,
        max_rows: u16,
    },
    Uploaded {
        digest: u64,
        epoch: u64,
        cols: u16,
        max_rows: u16,
    },
}

/// 本地 URL/路径解析：`file://` 去 scheme，相对路径按进程 cwd 拼接。
/// 不做 canonicalize（请求线程内的读取会以 fd 为准）。
pub(crate) fn resolve_local_path(source: &str) -> Option<PathBuf> {
    let source = source.trim();
    if source.is_empty() || classify_url(source) != UrlKind::Local {
        return None;
    }
    let raw = source.strip_prefix("file://").unwrap_or(source);
    let decoded = percent_decode(raw);
    if decoded.is_empty() {
        return None;
    }
    let path = PathBuf::from(decoded);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

pub(crate) fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 文件版本：canonical 路径 + mtime + 长度（版本变化视为不同图片）。
pub(crate) fn file_version(path: PathBuf) -> (PathBuf, u128, u64) {
    let canonical = path.canonicalize().unwrap_or(path);
    match std::fs::metadata(&canonical) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos());
            (canonical, mtime, meta.len())
        }
        Err(_) => (canonical, 0, 0),
    }
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
