//! `resources::instructions` 证据：候选优先级、空文档短路、`@import` 展开
//! （深度/环/越界）、index manifest 与读取投影。

use std::path::Path;

use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建父目录");
    }
    std::fs::write(path, content).expect("写夹具");
}

fn budget() -> ResourceBudget {
    ResourceBudget::default()
}

#[test]
fn main_candidate_uses_first_existing_file() {
    let dir = tempdir();
    write(&dir.path().join("CLAUDE.md"), "claude content");
    // AGENTS.md 不存在：选 CLAUDE.md。
    let bundle = scan(dir.path(), &budget());
    assert_eq!(bundle.main.as_ref().expect("main").text, "claude content");
    assert_eq!(bundle.index.selected.as_deref(), Some("CLAUDE.md"));

    // AGENTS.md 存在时优先。
    write(&dir.path().join("AGENTS.md"), "agents content");
    let bundle = scan(dir.path(), &budget());
    assert_eq!(bundle.main.as_ref().expect("main").text, "agents content");
    assert_eq!(bundle.index.selected.as_deref(), Some("AGENTS.md"));

    // 前两者都不在时选 .claude/AGENTS.md。
    let dir2 = tempdir();
    write(&dir2.path().join(".claude").join("AGENTS.md"), "nested");
    let bundle = scan(dir2.path(), &budget());
    assert_eq!(bundle.main.as_ref().expect("main").text, "nested");
    assert_eq!(bundle.index.selected.as_deref(), Some(".claude/AGENTS.md"));
}

#[test]
fn first_existing_empty_document_short_circuits_without_fallback() {
    let dir = tempdir();
    write(&dir.path().join("AGENTS.md"), "   \n");
    write(&dir.path().join("CLAUDE.md"), "later candidate");
    let bundle = scan(dir.path(), &budget());
    assert!(bundle.main.is_none(), "首个存在但为空 → main 不存在");
    assert_eq!(
        bundle.index.selected.as_deref(),
        Some("AGENTS.md"),
        "选中项仍是首个存在的文件（不继续 fallback，现状语义）"
    );
}

#[test]
fn local_document_is_independent_resource() {
    let dir = tempdir();
    write(&dir.path().join("CLAUDE.md"), "main");
    write(&dir.path().join("CLAUDE.local.md"), "local overlay");
    let bundle = scan(dir.path(), &budget());
    assert_eq!(bundle.main.as_ref().expect("main").text, "main");
    assert_eq!(bundle.local.as_ref().expect("local").text, "local overlay");
    assert_eq!(
        bundle.local.as_ref().expect("local").source_label,
        "CLAUDE.local.md"
    );

    // 空的 local 视为不存在。
    let dir2 = tempdir();
    write(&dir2.path().join("CLAUDE.md"), "main");
    write(&dir2.path().join("CLAUDE.local.md"), "\n");
    let bundle = scan(dir2.path(), &budget());
    assert!(bundle.local.is_none());
}

#[test]
fn imports_expand_for_claude_docs_with_depth_and_cycle_guard() {
    let dir = tempdir();
    write(
        &dir.path().join("CLAUDE.md"),
        "top\n<!-- @import docs/a.md -->\nbottom\n",
    );
    write(
        &dir.path().join("docs").join("a.md"),
        "A\n<!-- @import b.md -->\n",
    );
    write(&dir.path().join("docs").join("b.md"), "B\n");
    let bundle = scan(dir.path(), &budget());
    let main = bundle.main.as_ref().expect("main");
    assert_eq!(
        main.text, "top\nA\nB\n\n\nbottom\n",
        "递归展开并保留周边文本（占位符行自身的换行与导入文件尾换行都保留，不 trim）"
    );
    let imports: Vec<&str> = bundle
        .index
        .imports
        .iter()
        .map(|i| i.relative.as_str())
        .collect();
    assert_eq!(imports, vec!["docs/a.md", "docs/b.md"], "深度优先顺序");

    // 环：a 引回 CLAUDE.md 时保留占位符，不无限递归。
    let dir2 = tempdir();
    write(&dir2.path().join("CLAUDE.md"), "<!-- @import loop.md -->\n");
    write(
        &dir2.path().join("loop.md"),
        "L\n<!-- @import CLAUDE.md -->\n",
    );
    let bundle = scan(dir2.path(), &budget());
    let text = &bundle.main.as_ref().expect("main").text;
    assert_eq!(
        text, "L\n<!-- @import CLAUDE.md -->\n\n",
        "环引用保留原始占位符（其余文本含换行不动）"
    );
}

#[test]
fn import_depth_is_bounded_to_three_levels() {
    let dir = tempdir();
    write(&dir.path().join("CLAUDE.md"), "<!-- @import d1.md -->\n");
    write(&dir.path().join("d1.md"), "1<!-- @import d2.md -->\n");
    write(&dir.path().join("d2.md"), "2<!-- @import d3.md -->\n");
    write(&dir.path().join("d3.md"), "3<!-- @import d4.md -->\n");
    write(&dir.path().join("d4.md"), "4\n");
    let bundle = scan(dir.path(), &budget());
    let text = bundle.main.as_ref().expect("main").text.clone();
    assert!(text.contains('3'), "第 3 层展开：{text}");
    assert!(
        text.contains("<!-- @import d4.md -->"),
        "第 4 层越深保留占位符：{text}"
    );
}

#[test]
fn agents_md_does_not_resolve_imports() {
    let dir = tempdir();
    write(&dir.path().join("AGENTS.md"), "<!-- @import other.md -->\n");
    write(&dir.path().join("other.md"), "other");
    let bundle = scan(dir.path(), &budget());
    assert_eq!(
        bundle.main.as_ref().expect("main").text,
        "<!-- @import other.md -->\n",
        "AGENTS.md 系列不解析 @import（现状语义）"
    );
    assert!(bundle.index.imports.is_empty());
}

#[test]
fn out_of_workspace_import_keeps_placeholder() {
    let outside = tempdir();
    write(&outside.path().join("secret.md"), "outside secret");
    let dir = tempdir();
    let outside_path = outside.path().canonicalize().expect("canonical");
    write(
        &dir.path().join("CLAUDE.md"),
        &format!("<!-- @import {}/secret.md -->\n", outside_path.display()),
    );
    let bundle = scan(dir.path(), &budget());
    let text = bundle.main.as_ref().expect("main").text.clone();
    assert!(
        text.contains("<!-- @import "),
        "越界 import 保留占位符：{text}"
    );
    assert!(!text.contains("outside secret"));
    assert!(bundle.index.imports.is_empty());
}

#[test]
fn missing_import_keeps_placeholder() {
    let dir = tempdir();
    write(
        &dir.path().join("CLAUDE.md"),
        "<!-- @import missing.md -->\n",
    );
    let bundle = scan(dir.path(), &budget());
    assert_eq!(
        bundle.main.as_ref().expect("main").text,
        "<!-- @import missing.md -->\n"
    );
}

#[test]
fn oversized_main_is_treated_as_absent() {
    let dir = tempdir();
    write(&dir.path().join("CLAUDE.md"), &"x".repeat(64));
    let tiny = ResourceBudget {
        max_file_bytes: 16,
        ..ResourceBudget::default()
    };
    let bundle = scan(dir.path(), &tiny);
    assert!(bundle.main.is_none());
    assert_eq!(
        read(dir.path(), &tiny, InstructionDocument::Main).unwrap_err(),
        ResourceError::NotFound,
        "超预算文档对读取面等同找不到（list 与 read 一致）"
    );
}

#[test]
fn index_manifest_reports_documents_and_imports() {
    let dir = tempdir();
    write(
        &dir.path().join("CLAUDE.md"),
        "<!-- @import part.md -->\ntail\n",
    );
    write(&dir.path().join("part.md"), "part\n");
    write(&dir.path().join("CLAUDE.local.md"), "local\n");
    let bundle = scan(dir.path(), &budget());
    let index = &bundle.index;
    assert_eq!(index.scope, "workspace");
    assert_eq!(index.candidates, MAIN_CANDIDATES.to_vec());
    assert_eq!(index.selected.as_deref(), Some("CLAUDE.md"));
    assert_eq!(index.documents.len(), 2);
    assert!(index.documents[0].present, "main 存在");
    assert_eq!(index.documents[0].uri, INSTRUCTION_MAIN_URI);
    assert!(index.documents[1].present, "local 存在");
    assert_eq!(index.documents[1].uri, INSTRUCTION_LOCAL_URI);
    assert_eq!(index.imports.len(), 1);
    assert_eq!(index.imports[0].relative, "part.md");
    assert_eq!(
        index.imports[0].digest,
        digest_bytes(b"part\n"),
        "import digest 按原始字节计算"
    );
    // 序列化形状（宿主消费面用 camelCase）。
    let value = serde_json::to_value(index).expect("序列化");
    assert!(value.get("selected").is_some());
    assert!(value.get("documents").is_some());
    assert!(value.get("imports").is_some());
}

#[test]
fn reads_project_expected_mime_and_digest() {
    let dir = tempdir();
    write(&dir.path().join("CLAUDE.md"), "main\n");
    write(&dir.path().join("CLAUDE.local.md"), "local\n");

    let main = read(dir.path(), &budget(), InstructionDocument::Main).expect("main");
    assert_eq!(main.mime, MIME_MARKDOWN);
    assert_eq!(main.digest, digest_bytes(b"main\n"));
    let local = read(dir.path(), &budget(), InstructionDocument::Local).expect("local");
    assert_eq!(local.mime, MIME_TEXT);
    let index = read(dir.path(), &budget(), InstructionDocument::Index).expect("index");
    assert_eq!(index.mime, MIME_JSON);
    let parsed: serde_json::Value = serde_json::from_str(&index.text).expect("index 是 JSON");
    assert_eq!(parsed["selected"], "CLAUDE.md");

    assert_eq!(
        read(dir.path(), &budget(), InstructionDocument::Local).map(|r| r.text),
        Ok("local\n".to_string())
    );

    // 无 main 时 read(main) = NotFound。
    let empty = tempdir();
    assert_eq!(
        read(empty.path(), &budget(), InstructionDocument::Main).unwrap_err(),
        ResourceError::NotFound
    );
    // index 恒可读（空工作区也是合法 manifest）。
    assert!(read(empty.path(), &budget(), InstructionDocument::Index).is_ok());
}

#[test]
fn claude_symlink_target_is_read() {
    // 主文档允许 symlink（存量用法）；越界 import 仍按占位符保留（见上一用例）。
    let outside = tempdir();
    write(&outside.path().join("shared.md"), "shared body\n");
    let dir = tempdir();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside
                .path()
                .canonicalize()
                .expect("canonical")
                .join("shared.md"),
            dir.path().join("CLAUDE.md"),
        )
        .expect("建 symlink");
        let bundle = scan(dir.path(), &budget());
        assert_eq!(bundle.main.as_ref().expect("main").text, "shared body\n");
    }
    // 非 Unix 平台的对照：直接写文件（symlink 语义不适用）。
    #[cfg(not(unix))]
    {
        write(&dir.path().join("CLAUDE.md"), "shared body\n");
        let bundle = scan(dir.path(), &budget());
        assert_eq!(bundle.main.as_ref().expect("main").text, "shared body\n");
    }
}
