//! 图片安全层（image-p0-p1-spec §5 T5）。
//!
//! 集中实现 `image-rendering-research.md` §6.1 Q6 与 §6.2 全部安全约束：
//! - 路径分级：canonicalize 后是否落在受管理目录 `~/.peri/images` 内（Q6 采纳）；
//! - 常规文件 / 扩展名 / MIME 头（magic bytes）校验；
//! - 六项资源上限常量（§6.2-2）；
//! - 控制字符过滤：展示字段进终端前剥离/可见化（§6.2-4）；
//! - URL scheme 分类：远程 markdown URL 不自动下载（§6.2-1）。
//!
//! 被 T4（@image 显示/打开）与 T7（预览校验）复用。
//!
//! 安全降级（§6.4 / §5.7）：任何校验失败只降级（文本/不预览），不阻断发送；
//! 错误文本不含路径等敏感细节（RUST-ERROR-001）。
//!
//! TOCTOU（§6.2-3）：校验与读取分离时文件可能被替换/变 symlink——本模块
//! 实现为「先 `File::open` + 从 fd 取 `metadata()` → 读入 ≤ [`MAX_IMAGE_BYTES`]
//! 的缓冲区 → 对缓冲区统一校验」，**优先从已校验 fd 读取**，不二次按路径打开。
//! 解码路径经 [`read_validated_image`] 复用校验缓冲（F2：decode 不得按路径重开）。

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};

use thiserror::Error;

// ── 六项资源上限（§6.2-2）──────────────────────────────────────────────

/// 编码字节数上限（与 `peri-middlewares` ImageMiddleware 20MB 校验一致）。
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 宽/高单边上限。
pub const MAX_IMAGE_SIDE: u32 = 4096;

/// 总像素数上限（4096×4096；与 [`MAX_IMAGE_SIDE`] 在边界上恰好一致，
/// 作为独立防御线保留）。
pub const MAX_TOTAL_PIXELS: u64 = 16 * 1024 * 1024;

/// 同屏图片数上限（P1 单预览；P2 前恒 1，常量预留）。
pub const MAX_SCREEN_IMAGES: usize = 1;

/// 累计解码缓存上限（P1 单张，预留）。
pub const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// 解析时间上限（P1 以像素/尺寸上限近似实现，不引入时钟依赖）。
pub const MAX_DECODE_MS: u64 = 200;

// ── 路径分级 ─────────────────────────────────────────────────────────────

/// 路径分级（§6.1 Q6 采纳）：受管理目录 / 手工路径 / 其他。
///
/// - [`PathGrade::Managed`]：canonicalize 后落在受管理目录内，可自动预览；
/// - [`PathGrade::Manual`]：canonicalize 成功但不在受管理目录内（手工 `@image`
///   任意路径），预览前须完整校验、可要求显式操作；
/// - [`PathGrade::Other`]：无法 canonicalize（不存在/权限不足）等，只降级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathGrade {
    /// canonicalize 后仍在受管理目录（`~/.peri/images`）内的常规路径。
    Managed,
    /// 存在但不在受管理目录内的手工路径。
    Manual,
    /// 无法解析或其他不可用路径。
    Other,
}

/// URL scheme 分类（§6.2-1：远程 markdown URL 不自动下载）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlKind {
    /// `file://` 与无 scheme 的相对/绝对路径（Local 仍需路径校验）。
    Local,
    /// `http` / `https`——默认不自动下载，只显示文本/链接。
    RemoteHttp,
    /// 其余 scheme（`javascript:`、`data:`、`ftp:` 等）——只显示文本/链接。
    Dangerous,
}

/// 校验通过后的图片元信息（T7 预览直接使用；像素数来自 header 读取，非全量解码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageMeta {
    /// 像素宽度。JPEG/GIF/WebP 本期不解析尺寸，为 0（§5.5）。
    pub width: u32,
    /// 像素高度。JPEG/GIF/WebP 本期不解析尺寸，为 0（§5.5）。
    pub height: u32,
    /// 实际读入的字节数（≤ [`MAX_IMAGE_BYTES`]）。
    pub size_bytes: u64,
    /// MIME 类型（magic bytes 判定），如 `"image/png"`。
    pub mime: &'static str,
}

/// 资源上限种类（区分字节/像素超限，§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// 编码字节数超 [`MAX_IMAGE_BYTES`]。
    Bytes,
    /// 单边像素超 [`MAX_IMAGE_SIDE`]。
    Side,
    /// 总像素数超 [`MAX_TOTAL_PIXELS`]。
    Pixels,
}

impl std::fmt::Display for LimitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitKind::Bytes => write!(f, "byte size"),
            LimitKind::Side => write!(f, "side length"),
            LimitKind::Pixels => write!(f, "total pixels"),
        }
    }
}

/// 图片文件校验错误（结构化错误，RUST-ERROR-001；文本不含路径细节）。
#[derive(Debug, Error)]
pub enum ImageSafetyError {
    /// 路径不是常规文件（目录、socket、设备等）。
    #[error("not a regular file")]
    NotRegularFile,
    /// 扩展名缺失或不在白名单（png/jpg/jpeg/gif/webp）。
    #[error("unsupported or missing image extension")]
    BadExtension,
    /// 文件内容（magic bytes）与扩展名不匹配，或非任何已知图片格式。
    #[error("file content does not match its extension")]
    MimeMismatch,
    /// 超过资源上限（字节/单边/总像素）。
    #[error("image exceeds the {0} limit")]
    TooLarge(LimitKind),
    /// 底层 I/O 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// 图片头解码失败（如 PNG IHDR 损坏）。
    #[error("image decode error: {0}")]
    Decode(#[from] png::DecodingError),
}

/// 受管理图片目录根（`~/.peri/images`；与 `input_area.rs` 粘贴落盘规则一致）。
pub fn managed_images_root() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("images")
}

/// canonicalize + 受管理目录判定（`~/.peri/images`）。
///
/// 返回 (分级, canonical 后路径)；canonicalize 失败时为 ([`PathGrade::Other`], None)。
/// symlink 先经 canonicalize 解析再判定（§6.2-3）：指向受管理目录内 → Managed，
/// 指向目录外 → 降级 Manual。
pub fn grade_path(path: &Path) -> (PathGrade, Option<PathBuf>) {
    grade_path_with_root(path, &managed_images_root())
}

/// [`grade_path`] 的根目录注入版（单测用 tempdir 模拟 `~/.peri/images`）。
pub(crate) fn grade_path_with_root(
    path: &Path,
    managed_root: &Path,
) -> (PathGrade, Option<PathBuf>) {
    let Ok(canonical) = path.canonicalize() else {
        return (PathGrade::Other, None);
    };
    // 受管理根同样 canonicalize（如 macOS /var → /private/var），保证前缀比较一致；
    // 根不存在时（从未落盘过图片）无文件能落在其内，降级不误判。
    let canonical_root = managed_root
        .canonicalize()
        .unwrap_or_else(|_| managed_root.to_path_buf());
    let grade = if canonical.starts_with(&canonical_root) {
        PathGrade::Managed
    } else {
        PathGrade::Manual
    };
    (grade, Some(canonical))
}

/// 完整校验链：canonicalize → 常规文件 → 扩展名 → MIME 头 → 字节上限 → 像素上限。
///
/// 全部通过返回 [`ImageMeta`]；任何一步失败返回结构化错误。
/// 像素上限仅对 PNG 生效（header-only 读 IHDR）；JPEG/GIF/WebP 本期仅做
/// 扩展名 + magic bytes 校验，尺寸记为 0（§5.5，T6 引入 image crate 后补齐）。
pub fn validate_image_file(path: &Path) -> Result<ImageMeta, ImageSafetyError> {
    read_validated_image(path).map(|(meta, _)| meta)
}

/// 完整校验链 + 返回校验时读入的缓冲（§6.2-3 TOCTOU 闭合：解码必须复用
/// 本缓冲，**不得按路径二次打开**——validate 通过后文件可能被替换）。
/// 校验逻辑与 [`validate_image_file`] 完全一致；缓冲字节数 ≤ [`MAX_IMAGE_BYTES`]。
pub fn read_validated_image(path: &Path) -> Result<(ImageMeta, Vec<u8>), ImageSafetyError> {
    // TOCTOU（§6.2-3）：先打开 fd，之后一切以该 fd 与读入的缓冲区为准，
    // 不按路径二次打开（文件可能在校验与读取之间被替换/变 symlink）。
    let file = std::fs::File::open(path)?;
    let file_meta = file.metadata()?;
    if !file_meta.is_file() {
        return Err(ImageSafetyError::NotRegularFile);
    }
    if file_meta.len() > MAX_IMAGE_BYTES {
        return Err(ImageSafetyError::TooLarge(LimitKind::Bytes));
    }

    // 读入 ≤ MAX_IMAGE_BYTES 的缓冲区，之后对缓冲区统一校验（§6.2-3）。
    let mut buf = Vec::with_capacity(file_meta.len() as usize);
    file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageSafetyError::TooLarge(LimitKind::Bytes));
    }

    let ext_mime = extension_mime(path)?;
    let mime = sniff_mime(&buf).ok_or(ImageSafetyError::MimeMismatch)?;
    // 扩展名与 magic 冲突以 magic 为准：MIME 不匹配 → 降级（§5.5）。
    if mime != ext_mime {
        return Err(ImageSafetyError::MimeMismatch);
    }

    let (width, height) = if mime == "image/png" {
        // 仅 IHDR header 读取，不整图解码（§5.5）。
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&buf));
        let info = decoder.read_header_info()?;
        let (w, h) = (info.width, info.height);
        if u64::from(w) * u64::from(h) > MAX_TOTAL_PIXELS {
            return Err(ImageSafetyError::TooLarge(LimitKind::Pixels));
        }
        if w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
            return Err(ImageSafetyError::TooLarge(LimitKind::Side));
        }
        (w, h)
    } else {
        // JPEG/GIF/WebP：本期仅扩展名 + magic 校验，尺寸未解析（0）。
        (0, 0)
    };

    Ok((
        ImageMeta {
            width,
            height,
            size_bytes: buf.len() as u64,
            mime,
        },
        buf,
    ))
}

/// 扩展名 → MIME 白名单；缺失或不在白名单 → [`ImageSafetyError::BadExtension`]。
fn extension_mime(path: &Path) -> Result<&'static str, ImageSafetyError> {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return Err(ImageSafetyError::BadExtension);
    };
    match ext.to_ascii_lowercase().as_str() {
        "png" => Ok("image/png"),
        "jpg" | "jpeg" => Ok("image/jpeg"),
        "gif" => Ok("image/gif"),
        "webp" => Ok("image/webp"),
        _ => Err(ImageSafetyError::BadExtension),
    }
}

/// 按 magic bytes 判定图片 MIME（§5.5）：
/// PNG `\x89PNG\r\n\x1a\n`、JPEG `\xFF\xD8\xFF`、GIF `GIF87a/GIF89a`、
/// WEBP `RIFF....WEBP`；无法判定 → None。
fn sniff_mime(buf: &[u8]) -> Option<&'static str> {
    if buf.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if buf.len() >= 12 && &buf[..4] == b"RIFF" && &buf[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if buf.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    None
}

/// 展示字段进终端前过滤（§6.2-4）：剥离 C0（保留 `\n` `\t`）、ESC、C1、DEL；
/// 其余不可见/不可打印字符（零宽、bidi 控制、软连字符等）可见化为 `�`。
///
/// `\u{200c}`（ZWNJ）/`\u{200d}`（ZWJ）**原样保留**：它们是显示宽度 0 的合法
/// 组合符（emoji ZWJ 序列的组成部分），非注入向量；bidi 威胁由
/// `\u{202a}..=\u{202e}` 覆盖（评审 P1-2）。
///
/// 返回 [`Cow`]：无控制字符时零拷贝借用。
pub fn sanitize_for_terminal(s: &str) -> Cow<'_, str> {
    // 快速路径：第一个需要处理的字符之前直接借用。
    let Some((first, _)) = s.char_indices().find(|(_, c)| needs_sanitization(*c)) else {
        return Cow::Borrowed(s);
    };
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..first]);
    for c in s[first..].chars() {
        match c {
            '\n' | '\t' => out.push(c),
            // C0（除 \n\t）、ESC、C1、DEL → 剥离（防 escape 序列注入终端）。
            '\u{0000}'..='\u{001f}' | '\u{007f}' | '\u{0080}'..='\u{009f}' => {}
            // 其余不可见/不可打印字符 → 可见化：软连字符、零宽系列（ZWSP/
            // LRM/RLM——ZWNJ/ZWJ 除外，见函数文档）、bidi 控制、LS/PS、
            // 通用格式字符、BOM。
            '\u{00ad}'
            | '\u{200b}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{2060}'..='\u{2064}'
            | '\u{feff}' => out.push('\u{fffd}'),
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// 该字符是否需要被 [`sanitize_for_terminal`] 处理（与过滤循环保持一致）。
fn needs_sanitization(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        '\u{0000}'..='\u{001f}' | '\u{007f}' | '\u{0080}'..='\u{009f}' => true,
        '\u{00ad}'
        | '\u{200b}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{2060}'..='\u{2064}'
        | '\u{feff}' => true,
        _ => false,
    }
}

/// URL scheme 分类（§6.2-1）：`file://` 与无 scheme 的相对/绝对路径 → [`UrlKind::Local`]
/// （Local 仍需路径校验）；`http`/`https` → [`UrlKind::RemoteHttp`]；其余 scheme
/// （`javascript:`、`data:`、`ftp:` 等）→ [`UrlKind::Dangerous`]。
///
/// Windows 盘符（`C:\foo` / `C:/foo`）是本地路径，不按 scheme 处理。
pub fn classify_url(url: &str) -> UrlKind {
    // RFC 3986 scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"
    let colon = url.find(':').filter(|&colon| {
        let mut chars = url[..colon].chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    let Some(colon) = colon else {
        // 无 scheme：相对/绝对路径。
        return UrlKind::Local;
    };
    let scheme = url[..colon].to_ascii_lowercase();
    // 单字母 + 路径分隔符 → Windows 盘符（本地路径，非 scheme）。
    if scheme.len() == 1 && url[colon + 1..].starts_with(['\\', '/']) {
        return UrlKind::Local;
    }
    match scheme.as_str() {
        "file" => UrlKind::Local,
        "http" | "https" => UrlKind::RemoteHttp,
        _ => UrlKind::Dangerous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跨平台 symlink 创建（unix: `symlink` / windows: `symlink_file`）。
    fn make_symlink(
        target: impl AsRef<std::path::Path>,
        link: impl AsRef<std::path::Path>,
    ) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
    }

    /// 最小合法 PNG 构造（签名 + IHDR + IEND，CRC 正确）；仅 header 即可被
    /// `read_header_info` 解析，无需真实像素数据。
    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth 8 / RGBA / deflate / adaptive / 无 interlace
        png.extend_from_slice(&(ihdr.len() as u32).to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&ihdr);
        let mut crc_input = Vec::new();
        crc_input.extend_from_slice(b"IHDR");
        crc_input.extend_from_slice(&ihdr);
        png.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes()); // IEND 长度 0
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&crc32(b"IEND").to_be_bytes());
        png
    }

    /// PNG chunk CRC-32（IEEE 802.3，reflected 表驱动等价实现）。
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    // ── 路径分级（§5.6）────────────────────────────────────────────────

    #[test]
    fn test_grade_managed_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".peri").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let file = write_file(&root, "a.png", b"x");
        let (grade, canonical) = grade_path_with_root(&file, &root);
        assert_eq!(grade, PathGrade::Managed);
        // canonicalize 会把 macOS /var 解析为 /private/var，与原始输入可能不同。
        assert_eq!(
            canonical.as_deref(),
            Some(file.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn test_grade_manual_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("managed");
        std::fs::create_dir_all(&root).unwrap();
        let other = dir.path().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        let file = write_file(&other, "a.png", b"x");
        let (grade, canonical) = grade_path_with_root(&file, &root);
        assert_eq!(grade, PathGrade::Manual);
        assert_eq!(
            canonical.as_deref(),
            Some(file.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn test_grade_symlink_into_managed_is_managed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".peri").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let file = write_file(&root, "a.png", b"x");
        let link = dir.path().join("link.png");
        make_symlink(&file, &link).unwrap();
        // canonicalize 解析 symlink 后仍落在受管理目录内 → Managed（§6.2-3）。
        let (grade, canonical) = grade_path_with_root(&link, &root);
        assert_eq!(grade, PathGrade::Managed);
        assert_eq!(
            canonical.as_deref(),
            Some(file.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn test_grade_symlink_outside_root_downgrades() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".peri").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let file = write_file(&outside, "a.png", b"x");
        let link = root.join("escape.png");
        make_symlink(&file, &link).unwrap();
        // symlink 指向目录外 → 降级 Manual（§6.2-3）。
        let (grade, canonical) = grade_path_with_root(&link, &root);
        assert_eq!(grade, PathGrade::Manual);
        assert_eq!(
            canonical.as_deref(),
            Some(file.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn test_grade_missing_path_is_other() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.png");
        let (grade, canonical) = grade_path_with_root(&missing, dir.path());
        assert_eq!(grade, PathGrade::Other);
        assert_eq!(canonical, None);
    }

    // ── 文件校验（§5.6）────────────────────────────────────────────────

    #[test]
    fn test_validate_png_ok() {
        let dir = tempfile::tempdir().unwrap();
        let png = make_png(64, 32);
        let file = write_file(dir.path(), "a.png", &png);
        let meta = validate_image_file(&file).unwrap();
        assert_eq!(meta.width, 64);
        assert_eq!(meta.height, 32);
        assert_eq!(meta.mime, "image/png");
        assert_eq!(meta.size_bytes, png.len() as u64);
    }

    #[test]
    fn test_validate_png_uppercase_extension() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "a.PNG", &make_png(1, 1));
        assert!(validate_image_file(&file).is_ok());
    }

    #[test]
    fn test_validate_jpeg_gif_webp_magic_only() {
        let dir = tempfile::tempdir().unwrap();
        // JPEG：仅 3 字节 magic + 内容（本期不解析尺寸）。
        let jpeg = write_file(
            dir.path(),
            "a.jpg",
            &[0xFF, 0xD8, 0xFF, 0xE0, 0x10, b'J', b'F', b'I', b'F', b'0'],
        );
        let meta = validate_image_file(&jpeg).unwrap();
        assert_eq!(meta.mime, "image/jpeg");
        assert_eq!((meta.width, meta.height), (0, 0));

        let gif = write_file(dir.path(), "a.gif", b"GIF89a\x01\x00\x01\x00\x80\x00\x00");
        let meta = validate_image_file(&gif).unwrap();
        assert_eq!(meta.mime, "image/gif");
        assert_eq!((meta.width, meta.height), (0, 0));

        let webp = write_file(dir.path(), "a.webp", b"RIFF\x24\x00\x00\x00WEBPVP8 ");
        let meta = validate_image_file(&webp).unwrap();
        assert_eq!(meta.mime, "image/webp");
        assert_eq!((meta.width, meta.height), (0, 0));
    }

    #[test]
    fn test_validate_png_extension_text_content_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "fake.png", b"not an image at all");
        let err = validate_image_file(&file).unwrap_err();
        assert!(matches!(err, ImageSafetyError::MimeMismatch));
    }

    #[test]
    fn test_validate_extension_magic_conflict() {
        let dir = tempfile::tempdir().unwrap();
        // 扩展名 .png 但内容为 JPEG magic → 以 magic 为准，MimeMismatch。
        let file = write_file(dir.path(), "a.png", &[0xFF, 0xD8, 0xFF, 0xE0]);
        let err = validate_image_file(&file).unwrap_err();
        assert!(matches!(err, ImageSafetyError::MimeMismatch));
    }

    #[test]
    fn test_validate_bad_extension() {
        let dir = tempfile::tempdir().unwrap();
        let txt = write_file(dir.path(), "a.txt", b"plain");
        let err = validate_image_file(&txt).unwrap_err();
        assert!(matches!(err, ImageSafetyError::BadExtension));

        let no_ext = write_file(dir.path(), "noext", b"\x89PNG\r\n\x1a\n");
        let err = validate_image_file(&no_ext).unwrap_err();
        assert!(matches!(err, ImageSafetyError::BadExtension));
    }

    #[test]
    fn test_validate_directory_is_not_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_image_file(dir.path()).unwrap_err();
        // Unix 可 open 目录再经 is_file() 判定 → NotRegularFile；Windows 上
        // File::open(目录) 直接失败 → Io（两种都是"目录不得作为图片通过校验"）。
        assert!(
            matches!(
                err,
                ImageSafetyError::NotRegularFile | ImageSafetyError::Io(_)
            ),
            "目录校验应拒绝，实际: {err:?}"
        );
    }

    #[test]
    fn test_validate_too_large_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.png");
        let f = std::fs::File::create(&big).unwrap();
        // 空洞文件：metadata 长度即超限，无需真实写入 20MB。
        f.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        drop(f);
        let err = validate_image_file(&big).unwrap_err();
        assert!(matches!(err, ImageSafetyError::TooLarge(LimitKind::Bytes)));
    }

    #[test]
    fn test_validate_pixel_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        // 总像素超限（5000×5000 = 25M > 16M）→ Pixels。
        let wide = write_file(dir.path(), "big.png", &make_png(5000, 5000));
        let err = validate_image_file(&wide).unwrap_err();
        assert!(matches!(err, ImageSafetyError::TooLarge(LimitKind::Pixels)));

        // 面积小但单边超限 → Side。
        let tall = write_file(dir.path(), "tall.png", &make_png(4097, 1));
        let err = validate_image_file(&tall).unwrap_err();
        assert!(matches!(err, ImageSafetyError::TooLarge(LimitKind::Side)));
    }

    #[test]
    fn test_validate_png_at_limit_passes() {
        // 4096×4096 恰好等于两侧上限与像素上限，应通过。
        let dir = tempfile::tempdir().unwrap();
        let at = write_file(dir.path(), "at.png", &make_png(4096, 4096));
        let meta = validate_image_file(&at).unwrap();
        assert_eq!((meta.width, meta.height), (4096, 4096));
    }

    #[test]
    fn test_validate_broken_png_header_is_decode_error() {
        let dir = tempfile::tempdir().unwrap();
        // PNG magic + 损坏/截断的 IHDR → Decode（而非 MimeMismatch）。
        let broken = write_file(
            dir.path(),
            "broken.png",
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\xff\xff",
        );
        let err = validate_image_file(&broken).unwrap_err();
        assert!(matches!(err, ImageSafetyError::Decode(_)));
    }

    // ── 控制字符过滤（§5.6）────────────────────────────────────────────

    #[test]
    fn test_sanitize_strips_control_characters() {
        // ESC 与 NUL 被剥离，`[31m` 与 `c` 等普通字符保留。
        assert_eq!(sanitize_for_terminal("a\x1b[31mb\x00c"), "a[31mbc");
        // C1 与 DEL 同样剥离。
        assert_eq!(sanitize_for_terminal("x\u{80}y\u{7f}z"), "xyz");
    }

    #[test]
    fn test_sanitize_keeps_newline_and_tab() {
        assert_eq!(sanitize_for_terminal("a\n\tb"), "a\n\tb");
    }

    #[test]
    fn test_sanitize_visible_replaces_invisible() {
        // 零宽空格/软连字符/LRM/RLM → 可见化 '�'。
        assert_eq!(sanitize_for_terminal("a\u{200b}b"), "a\u{fffd}b");
        assert_eq!(sanitize_for_terminal("a\u{200e}b"), "a\u{fffd}b");
        assert_eq!(sanitize_for_terminal("a\u{200f}b"), "a\u{fffd}b");
        assert_eq!(sanitize_for_terminal("a\u{00ad}b"), "a\u{fffd}b");
        assert_eq!(sanitize_for_terminal("a\u{feff}b"), "a\u{fffd}b");
        // LS/PS（评审 P2-10）→ 可见化（终端渲染空白，防不可见分隔混淆）。
        assert_eq!(sanitize_for_terminal("a\u{2028}b"), "a\u{fffd}b");
        assert_eq!(sanitize_for_terminal("a\u{2029}b"), "a\u{fffd}b");
    }

    /// 评审 P1-2 回归：ZWJ/ZWNJ 是 emoji 组合序列的组成部分，必须原样保留
    /// （可见化会打碎 `👨‍👩‍👧` 为 `👨�👩�👧`）；它们非注入向量（宽度 0 组合符，
    /// bidi 威胁由 202a..202e 覆盖）。
    #[test]
    fn test_sanitize_keeps_zwj_zwnj() {
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(sanitize_for_terminal(family), family, "ZWJ 序列原样保留");
        assert_eq!(
            sanitize_for_terminal("a\u{200c}b"),
            "a\u{200c}b",
            "ZWNJ 原样保留"
        );
    }

    #[test]
    fn test_sanitize_borrowed_for_plain_text() {
        let s = "plain text 中文";
        let out = sanitize_for_terminal(s);
        assert!(matches!(out, Cow::Borrowed(_)), "无控制字符时应零拷贝借用");
        assert_eq!(out, s);
    }

    // ── URL scheme 分类（§5.6）─────────────────────────────────────────

    #[test]
    fn test_classify_url_local() {
        for url in [
            "file:///x/y.png",
            "/abs/path.png",
            "rel/path.png",
            "rel",
            "C:\\img.png",
            "C:/img.png",
        ] {
            assert_eq!(classify_url(url), UrlKind::Local, "url={url}");
        }
    }

    #[test]
    fn test_classify_url_remote_http() {
        for url in ["https://example.com/a.png", "http://example.com/a.png"] {
            assert_eq!(classify_url(url), UrlKind::RemoteHttp, "url={url}");
        }
    }

    #[test]
    fn test_classify_url_dangerous() {
        for url in [
            "javascript:alert(1)",
            "data:image/png;base64,AAAA",
            "ftp://example.com/a.png",
        ] {
            assert_eq!(classify_url(url), UrlKind::Dangerous, "url={url}");
        }
    }
}
