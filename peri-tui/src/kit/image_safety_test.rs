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
