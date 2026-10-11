//! `resources::path` 的边界证据：canonical 根、symlink 拒绝、越界拒绝与
//! 预算拒绝。

use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

#[test]
fn canonical_root_accepts_dirs_and_rejects_missing_or_files() {
    let dir = tempdir();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("建目录");
    assert_eq!(
        canonical_root(&nested).as_deref(),
        Some(nested.canonicalize().expect("canonical").as_path())
    );
    assert!(canonical_root(&dir.path().join("missing")).is_none());
    std::fs::write(dir.path().join("file.txt"), b"x").expect("写文件");
    assert!(
        canonical_root(&dir.path().join("file.txt")).is_none(),
        "文件不是资源根"
    );
}

#[test]
fn hidden_names_are_dot_prefixed() {
    assert!(is_hidden_name(".git"));
    assert!(is_hidden_name(".hidden"));
    assert!(!is_hidden_name("visible"));
    assert!(!is_hidden_name(".."));
}

#[test]
fn safe_read_file_reads_regular_file() {
    let dir = tempdir();
    std::fs::write(dir.path().join("a.txt"), b"hello").expect("写夹具");
    let root = dir.path().canonicalize().expect("canonical");
    let bytes = safe_read_file(&root, "a.txt", 1024).expect("读取成功");
    assert_eq!(bytes, b"hello");
}

#[test]
fn safe_read_file_rejects_missing_dir_and_symlink_targets() {
    let dir = tempdir();
    let root = dir.path().canonicalize().expect("canonical");
    std::fs::create_dir(root.join("sub")).expect("建目录");
    assert_eq!(
        safe_read_file(&root, "missing.txt", 1024).unwrap_err(),
        ResourceError::NotFound
    );
    assert_eq!(
        safe_read_file(&root, "sub", 1024).unwrap_err(),
        ResourceError::NotFound,
        "目录不是可读文件"
    );
    assert_eq!(
        safe_read_file(&root, "", 1024).unwrap_err(),
        ResourceError::InvalidUri
    );

    #[cfg(unix)]
    {
        let target = root.join("target.txt");
        std::fs::write(&target, b"secret").expect("写目标");
        std::os::unix::fs::symlink(&target, root.join("link.txt")).expect("建 symlink");
        assert_eq!(
            safe_read_file(&root, "link.txt", 1024).unwrap_err(),
            ResourceError::NotFound,
            "symlink 文件必须拒绝（公开面禁 symlink）"
        );
    }
}

#[test]
fn safe_read_file_rejects_budget_overrun() {
    let dir = tempdir();
    std::fs::write(dir.path().join("big.txt"), vec![b'a'; 100]).expect("写夹具");
    let root = dir.path().canonicalize().expect("canonical");
    assert_eq!(
        safe_read_file(&root, "big.txt", 99).unwrap_err(),
        ResourceError::Budget
    );
    assert!(safe_read_file(&root, "big.txt", 100).is_ok());
}

#[cfg(unix)]
#[test]
fn safe_read_file_rejects_escape_through_symlinked_directory() {
    // 根内子目录被替换为指向根外的 symlink：文件名检查通过（末段不是 symlink），
    // canonical 前缀校验必须拦截。
    let outside = tempdir();
    std::fs::write(outside.path().join("secret.txt"), b"top secret").expect("写外部文件");
    let dir = tempdir();
    std::fs::create_dir(dir.path().join("sub")).expect("建目录");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("sub").join("escape"))
        .expect("建 symlink");
    let root = dir.path().canonicalize().expect("canonical");
    assert_eq!(
        safe_read_file(&root, "sub/escape/secret.txt", 1024).unwrap_err(),
        ResourceError::Denied,
        "canonical 前缀校验必须拒绝根外目标"
    );
}

#[test]
fn is_text_bytes_uses_utf8_and_nul_checks() {
    assert!(is_text_bytes(b"hello\nworld"));
    assert!(is_text_bytes("非 ASCII 文本".as_bytes()));
    assert!(!is_text_bytes(b"with\0nul"));
    assert!(!is_text_bytes(&[0xff, 0xfe, 0x00, 0x01]));
}
