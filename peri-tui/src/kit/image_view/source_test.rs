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
    assert_eq!(
        resolve_local_path("/tmp/a.png"),
        Some(std::path::PathBuf::from("/tmp/a.png"))
    );
    assert_eq!(
        resolve_local_path("file:///tmp/a%20b.png"),
        Some(std::path::PathBuf::from("/tmp/a b.png"))
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
