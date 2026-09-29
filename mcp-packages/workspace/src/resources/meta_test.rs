//! `resources::meta` 证据：一级 `.md` 扫描语义（未知 stem 列出、非 `.md` 不列、
//! 子目录不递归、非 UTF-8 名 / 正文与读取失败跳过）、read 的字节精确与错误分类。

use std::path::Path;

use peri_acp_types::workspace_resources::{digest_bytes, META_KEY_SCOPE};

use super::*;
use crate::resources::{
    ResourceBody, ResourceBudget, WorkspaceResourceProvider, WorkspaceResourcesInput,
};

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

fn write(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建父目录");
    }
    std::fs::write(path, content).expect("写夹具");
}

/// 仅装配段落覆盖面的 provider（技能面关掉 builtin，避免干扰断言）。
fn meta_provider(cwd: &Path) -> WorkspaceResourceProvider {
    WorkspaceResourceProvider::new(
        cwd,
        WorkspaceResourcesInput {
            disable_bundled: true,
            budget: ResourceBudget::default(),
            ..Default::default()
        },
    )
}

fn meta_uris(provider: &WorkspaceResourceProvider) -> Vec<String> {
    provider
        .list_resources()
        .iter()
        .map(|resource| resource.uri.clone())
        .filter(|uri| uri.starts_with("peri-meta://"))
        .collect()
}

fn read_text(provider: &WorkspaceResourceProvider, uri: &str) -> String {
    match provider.read(uri).expect("必须可读").body {
        ResourceBody::Text(text) => text,
        other => panic!("段落覆盖必须是文本正文：{other:?}"),
    }
}

#[test]
fn scan_is_one_level_and_lists_unknown_section_ids_verbatim() {
    let cwd = tempdir();
    let meta = cwd.path().join(META_DIR_RELATIVE);
    write(&meta.join("01_intro.md"), b"intro\n");
    // 不在 SECTION_IDS 中的 stem 也列出（逐字保留宿主 scanner 的列出语义）。
    write(&meta.join("not_a_section.md"), b"custom\n");
    // 非 `.md` 不列（扩展名大小写敏感，与 scanner 逐字一致）。
    write(&meta.join("notes.txt"), b"ignored\n");
    write(&meta.join("upper.MD"), b"ignored\n");
    write(&meta.join("archive.md.bak"), b"ignored\n");
    // 子目录不递归（目录名为 `x.md` 也忽略）。
    write(&meta.join("nested").join("deep.md"), b"ignored\n");
    std::fs::create_dir_all(meta.join("dir.md")).expect("建同名目录");

    let provider = meta_provider(cwd.path());
    assert_eq!(
        meta_uris(&provider),
        vec![
            "peri-meta://workspace/01_intro",
            "peri-meta://workspace/not_a_section"
        ],
        "一级扫描 + 未知 stem 列出 + 排序稳定"
    );
}

#[test]
fn read_returns_verbatim_bytes_including_empty_and_cjk() {
    let cwd = tempdir();
    let meta = cwd.path().join(META_DIR_RELATIVE);
    // 前后空白、无尾换行、CJK 与 frontmatter 样式文本都必须逐字返回。
    let verbatim = "  \n---\nname: 不是 frontmatter\n---\n段落正文：中文与 emoji ✅\n  ";
    write(&meta.join("01_intro.md"), verbatim.as_bytes());
    write(&meta.join("empty.md"), b"");

    let provider = meta_provider(cwd.path());
    let payload = provider
        .read("peri-meta://workspace/01_intro")
        .expect("覆盖文档可读");
    assert_eq!(payload.mime, "text/markdown");
    assert_eq!(
        payload.digest,
        digest_bytes(verbatim.as_bytes()),
        "digest 对原始字节计算"
    );
    assert_eq!(
        payload
            .meta
            .0
            .get(META_KEY_SCOPE)
            .and_then(|value| value.as_str()),
        Some("project"),
        "workspace 绑定资源（peri-instruction 同口径）"
    );
    match payload.body {
        ResourceBody::Text(text) => assert_eq!(text, verbatim, "不 trim、不解析 frontmatter"),
        other => panic!("必须是文本正文：{other:?}"),
    }

    // 空文档：scanner 语义下「存在且可读」，read 返回空串（不是 NotFound）。
    assert!(meta_uris(&provider).contains(&"peri-meta://workspace/empty".to_string()));
    assert_eq!(read_text(&provider, "peri-meta://workspace/empty"), "");
}

#[test]
fn missing_or_unreadable_directory_yields_empty_list_not_error() {
    // 目录不存在（常态）：list 空、read 未知 → NotFound。
    let cwd = tempdir();
    let provider = meta_provider(cwd.path());
    assert!(meta_uris(&provider).is_empty());
    assert_eq!(
        provider.read("peri-meta://workspace/01_intro").unwrap_err(),
        ResourceError::NotFound
    );

    // `.peri/meta` 是普通文件（不可读目录）：同样是空批而不是错误。
    let cwd = tempdir();
    write(&cwd.path().join(META_DIR_RELATIVE), b"not a directory");
    let provider = meta_provider(cwd.path());
    assert!(meta_uris(&provider).is_empty());
    assert_eq!(
        provider.read("peri-meta://workspace/01_intro").unwrap_err(),
        ResourceError::NotFound
    );
}

#[test]
fn read_rejects_unknown_section_and_foreign_shapes() {
    let cwd = tempdir();
    write(
        &cwd.path().join(META_DIR_RELATIVE).join("01_intro.md"),
        b"intro\n",
    );
    let provider = meta_provider(cwd.path());

    // 合法 URI、扫描不到该 stem → NotFound（handler 映射 -32002 / SEP-2164）。
    assert_eq!(
        provider.read("peri-meta://workspace/nope").unwrap_err(),
        ResourceError::NotFound
    );
    // 穿越面：URI 语法层拒绝（不做任意路径 join，也不二次解码）。
    for uri in [
        "peri-meta://workspace/../01_intro",
        "peri-meta://workspace/%2e%2e/01_intro",
        "peri-meta://workspace/01_intro/extra",
        "peri-meta://workspace/a%2Fb",
        "peri-meta://other/01_intro",
        "peri-meta:/01_intro",
    ] {
        assert_eq!(
            provider.read(uri).unwrap_err(),
            ResourceError::InvalidUri,
            "必须按非法 URI 拒绝: {uri}"
        );
    }
}

#[test]
fn non_utf8_content_is_skipped() {
    let cwd = tempdir();
    let meta = cwd.path().join(META_DIR_RELATIVE);
    write(&meta.join("bad.md"), &[0xff, 0xfe, 0x00]);
    write(&meta.join("good.md"), b"ok\n");

    let provider = meta_provider(cwd.path());
    assert_eq!(meta_uris(&provider), vec!["peri-meta://workspace/good"]);
    assert_eq!(
        provider.read("peri-meta://workspace/bad").unwrap_err(),
        ResourceError::NotFound,
        "读取失败的条目在公开面按不存在处理"
    );
}

#[test]
fn stem_outside_uri_segment_charset_is_not_published() {
    let cwd = tempdir();
    let meta = cwd.path().join(META_DIR_RELATIVE);
    // `%` / `#` / `?` 等字符无法进入合法 URI：不公开（无编码改写面）。
    write(&meta.join("bad%name.md"), b"x\n");
    write(&meta.join("with space.md"), b"ok\n");

    let provider = meta_provider(cwd.path());
    assert_eq!(
        meta_uris(&provider),
        vec!["peri-meta://workspace/with space"],
        "空格合法（URI 段约束允许），`%` 名不公开"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_skipped() {
    use std::os::unix::fs::PermissionsExt;

    let cwd = tempdir();
    let meta = cwd.path().join(META_DIR_RELATIVE);
    let locked = meta.join("locked.md");
    write(&locked, b"secret\n");
    write(&meta.join("good.md"), b"ok\n");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("去掉读权限");
    if std::fs::read(&locked).is_ok() {
        // 特权环境（root）忽略权限位，无法制造读取失败：不作断言（恢复后返回）。
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644))
            .expect("恢复权限");
        return;
    }

    let provider = meta_provider(cwd.path());
    assert_eq!(meta_uris(&provider), vec!["peri-meta://workspace/good"]);
    assert_eq!(
        provider.read("peri-meta://workspace/locked").unwrap_err(),
        ResourceError::NotFound
    );

    // 恢复权限以便 TempDir 清理。
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).expect("恢复权限");
}

#[cfg(unix)]
#[test]
fn symlinked_section_is_not_published() {
    let outside = tempdir();
    write(&outside.path().join("linked.md"), b"outside\n");

    let cwd = tempdir();
    let meta = cwd.path().join(META_DIR_RELATIVE);
    std::fs::create_dir_all(&meta).expect("建覆盖目录");
    std::os::unix::fs::symlink(outside.path().join("linked.md"), meta.join("linked.md"))
        .expect("建 symlink");
    write(&meta.join("real.md"), b"real\n");

    // W1 公开面禁 symlink（与旧 scanner 的 `is_file()` 跟随语义有意不同）：
    // 工作区外的文件不得进入系统提示词覆盖面。
    let provider = meta_provider(cwd.path());
    assert_eq!(meta_uris(&provider), vec!["peri-meta://workspace/real"]);
    assert_eq!(
        provider.read("peri-meta://workspace/linked").unwrap_err(),
        ResourceError::NotFound
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_file_name_yields_no_section_id() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // 旧 scanner 语义：`file_stem().to_str()` 为 None ⇒ 跳过（warn）。
    // macOS 文件系统拒绝创建非 UTF-8 名字（EILSEQ），因此在路径层覆盖该规则；
    // Linux 上同一函数即条目扫描使用的判定点。
    let mut bytes = b"bad".to_vec();
    bytes.push(0xff);
    bytes.extend_from_slice(b".md");
    let path = std::path::PathBuf::from(OsStr::from_bytes(&bytes));
    assert!(path.to_str().is_none(), "用例本身要求非 UTF-8 路径");
    assert_eq!(section_id_of(&path), None);

    // 对照：合法 UTF-8 名字提取 stem（含 `.md` 之外的后缀由扩展名检查负责）。
    assert_eq!(section_id_of(Path::new("good.md")), Some("good"));
    assert_eq!(section_id_of(Path::new("a.b.md")), Some("a.b"));
}
