//! 来源身份：摘要稳定、媒体映射、路径解析与文件版本。

use super::{UploadId, UploadMedia, digest_of, file_version, percent_decode, resolve_local_path};

#[test]
fn digest_is_stable_and_distinguishes_payloads() {
    assert_eq!(digest_of("AAAA"), digest_of("AAAA"));
    assert_ne!(digest_of("AAAA"), digest_of("BBBB"));
    assert_ne!(digest_of(""), digest_of("A"));
}

#[test]
fn upload_id_pins_media_type() {
    let png = UploadId::from_payload("AAAA", "image/png");
    assert_eq!(png.media, UploadMedia::Png);
    assert_eq!(png.digest, digest_of("AAAA"));
    let jpeg = UploadId::from_payload("AAAA", "image/jpeg");
    assert_ne!(png, jpeg, "同一载荷不同媒体类型是不同身份");
    assert_eq!(UploadMedia::from_mime("IMAGE/JPEG"), UploadMedia::Jpeg);
    assert_eq!(UploadMedia::from_mime("image/svg+xml"), UploadMedia::Other);
    assert_eq!(UploadMedia::Png.mime(), "image/png");
}

#[test]
fn local_paths_are_resolved_and_remote_rejected() {
    // 绝对路径按**平台**规则解析：Windows 上 `/tmp/a.png` 是「有根无盘符」，
    // 会挂到当前盘符根目录。断言解析结果的后缀而不是 POSIX 字面量，
    // 否则 Unix 通过、Windows 失败（PR #603 首轮 CI 复现）。
    let parsed = resolve_local_path("/tmp/a.png").expect("绝对路径应被解析");
    assert!(
        parsed.is_absolute(),
        "解析结果必须仍是绝对路径：{}",
        parsed.display()
    );
    assert!(
        parsed
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("tmp/a.png"),
        "绝对路径只按平台规则解析：{}",
        parsed.display()
    );

    let from_uri = resolve_local_path("file:///tmp/a%20b.png").expect("file URI 应被解析");
    assert!(
        from_uri
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("tmp/a b.png"),
        "file:// 去 scheme + percent decode：{}",
        from_uri.display()
    );
    assert!(resolve_local_path("https://example.com/a.png").is_none());
    assert!(resolve_local_path("javascript:alert(1)").is_none());
    assert!(resolve_local_path("   ").is_none());
}

#[test]
fn percent_decoding_matches_uri_rules() {
    assert_eq!(percent_decode("a%20b.png"), "a b.png");
    assert_eq!(percent_decode("a%2Fb.png"), "a/b.png");
    assert_eq!(percent_decode("100%.png"), "100%.png");
}

#[test]
fn file_version_tracks_canonical_path_and_length() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a.png");
    std::fs::write(&path, [1_u8, 2, 3]).expect("write");
    let (canonical, mtime, len) = file_version(path.clone());
    assert_eq!(len, 3);
    assert!(mtime > 0);
    assert!(canonical.is_absolute());

    let missing = dir.path().join("missing.png");
    let (_, mtime, len) = file_version(missing);
    assert_eq!((mtime, len), (0, 0), "缺失文件按零版本登记（解码失败降级）");
}
