//! `resources::scan` 的发现语义证据：leaf 语义、预算截断、symlink/隐藏条目跳过
//! 与附件枚举的确定性。

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

fn skill_file(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\nbody\n")
}

#[test]
fn find_skill_dirs_uses_leaf_semantics_and_skips_tool_dirs() {
    let dir = tempdir();
    let root = dir.path();
    // 普通子目录技能。
    write(
        &root.join("beta").join("SKILL.md"),
        &skill_file("beta", "d"),
    );
    // leaf：该目录含 SKILL.md，其子目录不得被发现为独立技能。
    write(
        &root.join("alpha").join("SKILL.md"),
        &skill_file("alpha", "d"),
    );
    write(
        &root.join("alpha").join("nested").join("SKILL.md"),
        &skill_file("nested", "d"),
    );
    // 跳过名单与隐藏目录。
    write(
        &root.join("node_modules").join("pkg").join("SKILL.md"),
        &skill_file("pkg", "d"),
    );
    write(
        &root.join(".hidden").join("SKILL.md"),
        &skill_file("hidden", "d"),
    );

    let mut found: Vec<String> = find_skill_dirs(root, 6, 1000)
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .expect("子路径")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec!["alpha", "beta"],
        "只发现 leaf 技能目录（alpha 的 nested 不再下钻）"
    );
}

#[test]
fn find_skill_dirs_treats_root_with_entry_as_single_leaf() {
    let dir = tempdir();
    let root = dir.path();
    write(&root.join("SKILL.md"), &skill_file("root-skill", "d"));
    write(
        &root.join("child").join("SKILL.md"),
        &skill_file("child", "d"),
    );
    let found = find_skill_dirs(root, 6, 1000);
    assert_eq!(
        found,
        vec![root.to_path_buf()],
        "根自身含 SKILL.md 时整个根是技能（leaf），子目录不发现"
    );
}

#[test]
fn find_skill_dirs_respects_depth_and_dir_budget() {
    let dir = tempdir();
    let root = dir.path();
    write(
        &root.join("a").join("b").join("c").join("SKILL.md"),
        &skill_file("deep", "d"),
    );
    // 深度 2：c 位于第 3 层，发现不到；深度 3 可见。
    assert!(
        find_skill_dirs(root, 2, 1000).is_empty(),
        "深度预算必须生效"
    );
    assert_eq!(find_skill_dirs(root, 3, 1000).len(), 1);
    // 目录数预算 1：根目录消耗 1 后即停。
    let many = tempdir();
    write(
        &many.path().join("x").join("SKILL.md"),
        &skill_file("x", "d"),
    );
    assert!(
        find_skill_dirs(many.path(), 6, 1).is_empty(),
        "单 root 目录数预算必须生效"
    );
}

#[cfg(unix)]
#[test]
fn find_skill_dirs_skips_symlinked_directories() {
    let outside = tempdir();
    write(&outside.path().join("SKILL.md"), &skill_file("linked", "d"));
    let dir = tempdir();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).expect("建 symlink");
    assert!(
        find_skill_dirs(dir.path(), 6, 1000).is_empty(),
        "symlink 技能目录不得被发现（X3：公开面禁 symlink）"
    );
}

#[test]
fn enumerate_skill_files_is_sorted_and_skips_hidden_and_symlinks() {
    let dir = tempdir();
    let root = dir.path();
    write(&root.join("SKILL.md"), &skill_file("demo", "d"));
    write(&root.join("scripts").join("run.sh"), "#!/bin/sh\n");
    write(&root.join("assets").join("logo.bin"), "binary");
    write(&root.join(".hidden-file"), "nope");
    write(&root.join("nested").join("SKILL.md"), "nested body");

    #[cfg(unix)]
    {
        std::fs::write(root.join("target.txt"), "t").expect("写目标");
        std::os::unix::fs::symlink(root.join("target.txt"), root.join("link.txt"))
            .expect("建 symlink");
    }

    let files = enumerate_skill_files(root, 64);
    #[cfg(unix)]
    let expected = vec![
        "SKILL.md",
        "assets/logo.bin",
        "nested/SKILL.md",
        "scripts/run.sh",
        "target.txt",
    ];
    #[cfg(not(unix))]
    let expected = vec![
        "SKILL.md",
        "assets/logo.bin",
        "nested/SKILL.md",
        "scripts/run.sh",
    ];
    assert_eq!(
        files, expected,
        "排序确定、隐藏与 symlink 跳过、嵌套 SKILL.md 作为附件"
    );
    // 预算截断按排序前缀。
    let truncated = enumerate_skill_files(root, 2);
    assert_eq!(truncated, vec!["SKILL.md", "assets/logo.bin"]);
}

#[test]
fn relative_path_string_rejects_escape() {
    let base = Path::new("/tmp/base");
    assert_eq!(
        relative_path_string(base, &base.join("a").join("b.txt")).as_deref(),
        Some("a/b.txt")
    );
    assert!(relative_path_string(base, Path::new("/tmp/other/x")).is_none());
    assert!(
        relative_path_string(base, base).is_none(),
        "空相对路径无意义"
    );
}
