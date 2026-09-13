//! capability 边界、符号链接与竞态测试（FC-SBX-01 / A-008 的证据面）。
//!
//! 分两层验证根边界：
//! - **capability 层**（权威判定）：`RequestedPath` + `RootDir` 直接对抗
//!   `..` 穿越、绝对越界、前缀碰撞、符号链接逃逸与替换竞态；
//! - **路由层**（对调用方的可见行为）：`RootDir::resolve_host_path` 在参数解析阶段就拒绝
//!   越界，并给出 `outside the authorized workspace` 业务错误（见 `runtime/executor.rs`）。

#[path = "fs_support/mod.rs"]
mod support;

use std::path::Path;

use local_mcp_server::capability::RootDir;
use serde_json::json;

use support::{call, call_error, folder_args, list_names, path_args, symlink, tree};

/// capability 层拒绝：返回可外发文案（与工具执行同一条判定链）。
fn denial_message(root: &Path, requested: &str) -> String {
    let dir = RootDir::open(root).expect("打开授权根");
    match dir
        .request_path(requested)
        .and_then(|parsed| dir.resolve(&parsed))
    {
        Err(error) => error.message(),
        Ok(_) => panic!("{requested} 本应被 capability 层拒绝"),
    }
}

#[test]
fn test_in_root_operations_succeed() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("notes.txt")),
        path_args(&tree.path("notes.txt")),
    );
    assert!(!response.is_error, "根内读取必须成功: {}", response.text);
    assert!(response.text.contains("alpha"));
    assert!(
        response.text.contains("     1\talpha"),
        "行号格式: {}",
        response.text
    );
}

#[test]
fn test_parent_traversal_is_rejected_by_capability_layer() {
    let tree = tree("basic");
    let link = tree.root.join("escape-link");
    symlink(&tree.path("../outside"), &link);

    for requested in [
        tree.path("../outside/secret.txt"),
        format!("{}/../../etc/passwd", tree.root_str()),
        format!("{}/../root-evil/loot.txt", tree.root_str()),
    ] {
        let message = denial_message(&tree.root, &requested);
        assert!(
            message.contains("authorized workspace"),
            "{requested} 必须被拒绝，实际: {message}"
        );
        assert!(!message.contains("OUTSIDE-SECRET-CONTENT"));
    }
}

#[test]
fn test_parent_traversal_through_tool_leaves_no_side_effects() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let before = support::snapshot(&tree.outside);
    let root_before = support::snapshot(&tree.root);

    // 工具层可见的越界形态是「根内符号链接指向根外」——这是模型真正能构造出来的形态。
    let escapes = [
        (tree.path("link_outside_file"), "secret.txt"),
        (tree.path("link_outside_dir/secret.txt"), "secret.txt"),
    ];
    for (target, _) in escapes {
        let message = call_error(&runtime, "Read", Some(&target), path_args(&target));
        assert!(
            !message.contains("OUTSIDE-SECRET-CONTENT"),
            "越界读取不得回显根外内容: {message}"
        );
    }
    assert_eq!(before, support::snapshot(&tree.outside), "根外必须零副作用");
    assert_eq!(
        root_before,
        support::snapshot(&tree.root),
        "根内必须零副作用"
    );
}

#[test]
fn test_absolute_paths_outside_root_are_rejected() {
    let tree = tree("basic");
    for requested in ["/etc/passwd", "/tmp", "/private/etc/hosts"] {
        let message = denial_message(&tree.root, requested);
        assert!(message.contains("authorized workspace"), "{message}");
    }
}

#[test]
fn test_prefix_collision_sibling_directory_is_rejected() {
    let tree = tree("basic");
    // 授权根 = <temp>/root；兄弟目录 <temp>/root-evil 与根构成字符串前缀关系。
    let evil = tree.root.with_file_name("root-evil");
    std::fs::create_dir_all(&evil).expect("创建前缀碰撞目录");
    std::fs::write(evil.join("loot.txt"), "EVIL-CONTENT\n").expect("写入");
    let runtime = tree.runtime();

    let target = evil.join("loot.txt").to_string_lossy().to_string();
    let message = denial_message(&tree.root, &target);
    assert!(message.contains("authorized workspace"), "{message}");

    // 路由层的宿主路径解析同样拒绝（同一套组件边界规则，D-003 后无第二表示）。
    let router_root = RootDir::open(&tree.root).expect("打开授权根");
    assert!(router_root.resolve_host_path(&target).is_err());
    assert!(router_root
        .resolve_host_path(&tree.root.join("notes.txt").to_string_lossy())
        .is_ok());

    // 工具层：即使路径是根内符号链接指向兄弟目录，也必须拒绝。
    let link = tree.root.join("evil-link");
    symlink(&evil.to_string_lossy(), &link);
    let message = call_error(
        &runtime,
        "Read",
        Some(&tree.path("evil-link/loot.txt")),
        path_args(&tree.path("evil-link/loot.txt")),
    );
    assert!(!message.contains("EVIL-CONTENT"), "{message}");
}

#[test]
fn test_final_symlink_escape_is_rejected() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let message = call_error(
        &runtime,
        "Read",
        Some(&tree.path("link_outside_file")),
        path_args(&tree.path("link_outside_file")),
    );
    assert!(
        !message.contains("OUTSIDE-SECRET-CONTENT"),
        "不得读取到根外内容: {message}"
    );
    assert!(
        message.contains("symlink") || message.contains("outside"),
        "必须给出逃逸拒绝文案: {message}"
    );
}

#[test]
fn test_middle_symlink_escape_is_rejected() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("link_outside_dir/secret.txt");
    let message = call_error(&runtime, "Read", Some(&target), path_args(&target));
    assert!(!message.contains("OUTSIDE-SECRET-CONTENT"), "{message}");
    assert!(
        message.contains("symlink") || message.contains("outside"),
        "{message}"
    );
}

#[test]
fn test_parent_relative_symlink_escape_is_rejected() {
    let tree = tree("basic");
    std::fs::write(tree.root.join("../outside-escape.txt"), "ESCAPE\n").expect("写入");
    let runtime = tree.runtime();
    let target = tree.path("link_to_parent_escape");
    let message = call_error(&runtime, "Read", Some(&target), path_args(&target));
    assert!(!message.contains("ESCAPE"), "{message}");
    assert!(!message.is_empty());
}

#[test]
fn test_in_root_symlinks_are_resolved() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("link_to_file")),
        path_args(&tree.path("link_to_file")),
    );
    assert!(
        !response.is_error,
        "根内符号链接必须可解析: {}",
        response.text
    );
    assert!(response.text.contains("pub fn add"), "{}", response.text);
}

#[test]
fn test_symlink_loop_fails_closed_instead_of_hanging() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("link_loop");
    let message = call_error(&runtime, "Read", Some(&target), path_args(&target));
    assert!(
        message.contains("symbolic links") || message.contains("symlink"),
        "环路必须报错而不是挂起: {message}"
    );
}

#[test]
fn test_write_does_not_follow_swapped_final_symlink() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("swap_target.txt");
    std::fs::write(&target, "original\n").expect("写入初始文件");

    // 竞态模拟：先解析出「父目录 fd + 末段名」，随后末段被换成指向根外的符号链接。
    let root = RootDir::open(&tree.root).expect("打开根");
    let requested = root.request_path(&target).expect("校验挂载前缀");
    let child = root.resolve(&requested).expect("解析父目录");
    std::fs::remove_file(&target).expect("删除目标");
    symlink(
        &tree.outside.join("secret.txt").to_string_lossy(),
        Path::new(&target),
    );

    // 基于已解析的父目录 fd 打开必须失败（O_NOFOLLOW），而不是跟随链接。
    assert!(child.open_read().is_err(), "被替换为符号链接后不得成功打开");

    // 通过工具重新写入也必须拒绝，且根外零副作用。
    let before = support::snapshot(&tree.outside);
    let message = call_error(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "hijacked\n" }),
    );
    assert!(!message.is_empty());
    assert_eq!(before, support::snapshot(&tree.outside), "根外必须零副作用");
}

#[test]
fn test_delete_and_recreate_race_keeps_root_boundary() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let victim = tree.path("src/lib.rs");

    std::fs::remove_file(&victim).expect("删除");
    symlink(
        &tree.outside.join("secret.txt").to_string_lossy(),
        Path::new(&victim),
    );
    let message = call_error(&runtime, "Read", Some(&victim), path_args(&victim));
    assert!(!message.contains("OUTSIDE-SECRET-CONTENT"), "{message}");

    std::fs::remove_file(&victim).expect("删除链接");
    std::fs::write(&victim, "pub fn restored() {}\n").expect("重建");
    let response = call(&runtime, "Read", Some(&victim), path_args(&victim));
    assert!(response.text.contains("restored"), "{}", response.text);
}

#[test]
fn test_traversal_route_never_escapes_root() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let before = support::snapshot(&tree.outside);
    let root_before = support::snapshot(&tree.root);

    // folder_operations / Glob / Edit 都遵守同一根边界（用根内链接指向根外）。
    let folder_target = tree.path("link_outside_dir");
    let message = call_error(
        &runtime,
        "folder_operations",
        Some(&folder_target),
        folder_args("list", &folder_target),
    );
    assert!(!message.contains("OUTSIDE-SECRET-CONTENT"), "{message}");

    let glob_target = tree.path("link_outside_dir");
    let message = call_error(
        &runtime,
        "Glob",
        Some(&glob_target),
        json!({ "pattern": "*", "path": glob_target }),
    );
    assert!(!message.contains("OUTSIDE-SECRET-CONTENT"), "{message}");

    let edit_target = tree.path("link_outside_file");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&edit_target),
        json!({ "file_path": edit_target, "old_string": "OUTSIDE", "new_string": "X" }),
    );
    assert!(!message.contains("OUTSIDE-SECRET-CONTENT"), "{message}");

    assert_eq!(before, support::snapshot(&tree.outside), "根外必须零副作用");
    assert_eq!(
        root_before,
        support::snapshot(&tree.root),
        "根内必须零副作用"
    );
}

#[test]
fn test_write_outside_root_never_creates_anything() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    // 指向根外目录的符号链接：写入必须失败，且两个方向都无副作用。
    let link = tree.root.join("outside-dir-link");
    symlink(&tree.outside.to_string_lossy(), &link);
    let root_before = support::snapshot(&tree.root);
    let outside_before = support::snapshot(&tree.outside);
    for relative in ["outside-dir-link/new.txt", "link_outside_dir/created.txt"] {
        let target = tree.path(relative);
        let message = call_error(
            &runtime,
            "Write",
            Some(&target),
            json!({ "file_path": target, "content": "nope\n" }),
        );
        assert!(!message.is_empty(), "{relative} 必须被拒绝");
    }
    assert_eq!(
        root_before,
        support::snapshot(&tree.root),
        "根内不得有副作用"
    );
    assert_eq!(
        outside_before,
        support::snapshot(&tree.outside),
        "根外不得有副作用"
    );
    assert!(!Path::new("/tmp/sandbox-mcp-should-not-exist.txt").exists());
}

#[test]
fn test_root_itself_is_addressable_for_listing_operations() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    for path in [tree.root_str(), format!("{}/", tree.root_str())] {
        let response = call(
            &runtime,
            "folder_operations",
            Some(&path),
            folder_args("list", &path),
        );
        assert!(!response.is_error, "根 listing 必须可用: {}", response.text);
        assert!(response.text.contains("Total:"), "{}", response.text);
    }
    let response = call(
        &runtime,
        "folder_operations",
        Some(&tree.root_str()),
        folder_args("exists", &tree.root_str()),
    );
    assert!(
        response.text.contains("Type: Directory"),
        "{}",
        response.text
    );
}

#[test]
fn test_glob_does_not_descend_into_symlinked_directories() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let response = call(
        &runtime,
        "Glob",
        Some(&tree.root_str()),
        json!({ "pattern": "**/*.rs", "path": tree.root_str() }),
    );
    let list = if response.is_error {
        String::new()
    } else {
        response.text.clone()
    };
    assert!(
        list.matches("/src/lib.rs").count() <= 1,
        "符号链接目录不得被下钻: {list}"
    );
}

#[test]
fn test_raw_listing_and_git_dir_stay_visible_but_glob_skips_them() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let names = list_names(&tree.root);
    assert!(names.contains(&".git".to_string()));
    assert!(names.contains(&"node_modules".to_string()));

    let response = call(
        &runtime,
        "Glob",
        Some(&tree.root_str()),
        json!({ "pattern": "**/*.js", "path": tree.root_str() }),
    );
    assert!(
        !response.text.contains("node_modules"),
        "skip-dirs 必须生效: {}",
        response.text
    );
}

// ─────────────────────── 重复枚举回归（F-001）───────────────────────
//
// 根因：目录枚举曾经 `dup(2)` 来源 fd 再 `fdopendir`，而 dup 与来源共享目录偏移，
// 第一次 readdir 到 EOF 后偏移停在末尾，第二次枚举就静默返回空。以下用例覆盖
// 「同一句柄重复枚举」「重新打开根句柄」「子目录重复枚举」与跨工具顺序。

#[test]
fn test_repeated_enumerations_of_root_and_subdirectory_keep_all_entries() {
    let tree = tree("basic");
    let dir = RootDir::open(&tree.root).expect("打开授权根");
    let root_path = dir
        .request_path(&tree.root_str())
        .expect("根路径必须可解析");

    let names = |handle: &local_mcp_server::capability::DirHandle| -> Vec<String> {
        handle
            .entries()
            .expect("枚举目录")
            .into_iter()
            .map(|entry| entry.name)
            .collect()
    };

    // 同一个 DirHandle 上连续枚举两次。
    let handle = dir.open_dir(&root_path).expect("打开根目录");
    let first = names(&handle);
    let second = names(&handle);
    assert!(!first.is_empty(), "根目录第一次枚举不应为空");
    assert_eq!(first, second, "同一句柄的重复枚举必须给出同一条目集");

    // 两次独立打开的根句柄（旧实现的根分支共享同一个根 fd 的偏移）。
    let reopened = names(&dir.open_dir(&root_path).expect("重新打开根目录"));
    assert_eq!(first, reopened, "重新打开的根句柄必须看到同一条目集");

    // 子目录同样不受重复枚举影响。
    let sub_path = dir
        .request_path(&tree.path("src"))
        .expect("子目录必须可解析");
    let sub = dir.open_dir(&sub_path).expect("打开子目录");
    let sub_first = names(&sub);
    let sub_second = names(&sub);
    assert!(!sub_first.is_empty(), "子目录枚举不应为空");
    assert_eq!(sub_first, sub_second, "子目录重复枚举必须一致");
}

#[test]
fn test_root_glob_then_folder_list_both_report_entries() {
    let tree = tree("basic");
    let runtime = tree.runtime();

    // 审查复现顺序：第一次枚举（Glob 根）正确，第二次（folder list 根）曾经静默为空。
    let glob = call(
        &runtime,
        "Glob",
        Some(&tree.root_str()),
        json!({ "pattern": "**/*.rs", "path": tree.root_str() }),
    );
    assert!(!glob.is_error, "{}", glob.text);
    assert!(glob.text.contains("/src/lib.rs"), "{}", glob.text);
    assert!(glob.text.contains("/src/main.rs"), "{}", glob.text);

    let listing = call(
        &runtime,
        "folder_operations",
        Some(&tree.root_str()),
        folder_args("list", &tree.root_str()),
    );
    assert!(!listing.is_error, "{}", listing.text);
    assert!(
        !listing.text.contains("Total: 0 directories, 0 files"),
        "根目录第二次枚举不得为空: {}",
        listing.text
    );
    assert!(listing.text.contains("notes.txt"), "{}", listing.text);
    assert!(
        support::field(&listing, "entries")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            > 0,
        "entries 字段必须非零: {}",
        listing.structured
    );
}

#[test]
fn test_write_then_root_enumeration_reports_the_new_file() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("fresh_note.txt");
    let write = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "fresh\n" }),
    );
    assert!(!write.is_error, "{}", write.text);

    let listing = call(
        &runtime,
        "folder_operations",
        Some(&tree.root_str()),
        folder_args("list", &tree.root_str()),
    );
    assert!(!listing.is_error, "{}", listing.text);
    assert!(
        listing.text.contains("fresh_note.txt"),
        "写入之后的根枚举必须包含新文件: {}",
        listing.text
    );

    let glob = call(
        &runtime,
        "Glob",
        Some(&tree.root_str()),
        json!({ "pattern": "*.txt", "path": tree.root_str() }),
    );
    assert!(!glob.is_error, "{}", glob.text);
    assert!(glob.text.contains("fresh_note.txt"), "{}", glob.text);
}
