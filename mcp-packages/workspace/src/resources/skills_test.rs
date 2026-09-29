//! `resources::skills` 证据：扫描/优先级/非法项处置、manifest 形状与 URI 定位
//! （含「get 未枚举合法项可读」）。

use std::path::{Path, PathBuf};

use peri_acp_types::workspace_resources::{digest_bytes, ResourceScope};

use super::*;
use crate::resources::ResourceRoot;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

fn write(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建父目录");
    }
    std::fs::write(path, content).expect("写夹具");
}

fn skill_md(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\n")
}

fn root(path: &Path, scope: ResourceScope) -> ResourceRoot {
    ResourceRoot::new(path, scope)
}

fn input(roots: Vec<ResourceRoot>) -> WorkspaceResourcesInput {
    WorkspaceResourcesInput {
        skill_roots: roots,
        agent_roots: Vec::new(),
        // 测本地根时关掉 builtin，避免 7 个内置技能干扰计数。
        disable_bundled: true,
        budget: ResourceBudget::default(),
    }
}

/// 建一个技能目录并返回其根。
fn skill_dir(base: &Path, dir_name: &str, name: &str) -> PathBuf {
    let dir = base.join(dir_name);
    write(&dir.join("SKILL.md"), skill_md(name, "demo").as_bytes());
    dir
}

#[test]
fn scan_builds_complete_manifest_with_entry_first() {
    let base = tempdir();
    let dir = skill_dir(base.path(), "alpha", "alpha");
    write(&dir.join("scripts").join("run.sh"), b"#!/bin/sh\n");
    write(&dir.join("assets").join("logo.png"), &[0xff, 0x00, 0x80]);
    write(&dir.join("notes.txt"), b"plain text\n");

    let catalog = super::scan_catalog(
        &input(vec![root(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    assert_eq!(catalog.len(), 1);
    let record = &catalog[0];
    assert_eq!(record.name, "alpha");
    assert_eq!(record.description, "demo");
    let relative: Vec<&str> = record
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    assert_eq!(
        relative,
        vec!["SKILL.md", "assets/logo.png", "notes.txt", "scripts/run.sh"],
        "manifest 完整且入口在首项"
    );
    let logo = &record.files[1];
    assert!(!logo.text, "无效 UTF-8 附件按 blob 处理");
    assert_eq!(logo.size, 3);
    let entry = &record.files[0];
    assert!(entry.text);
    assert_eq!(
        entry.digest,
        digest_bytes(skill_md("alpha", "demo").as_bytes())
    );
    // URI 形状与契约一致。
    assert_eq!(
        record.uri().as_deref(),
        Some("skill://project/alpha/SKILL.md")
    );
    let entry = record.entry().expect("条目");
    let resources = entry.resources.expect("provider 必须给完整清单");
    assert_eq!(resources.len(), 4);
    assert_eq!(resources[0].uri, "skill://project/alpha/SKILL.md");
    assert_eq!(resources[1].uri, "skill://project/alpha/assets/logo.png");
}

#[test]
fn catalog_prefers_first_root_and_hides_shadow_but_get_reads_by_uri() {
    let high = tempdir();
    skill_dir(high.path(), "dup", "dup");
    let low = tempdir();
    skill_dir(low.path(), "dup", "dup");

    let input = input(vec![
        root(high.path(), ResourceScope::Project),
        root(low.path(), ResourceScope::User),
    ]);
    let catalog = super::scan_catalog(&input, &ResourceBudget::default());
    assert_eq!(catalog.len(), 1, "跨根同名只公开 winner");
    assert_eq!(catalog[0].scope, ResourceScope::Project, "先到先得");

    // shadow（User 根）不在 list 枚举，但按 URI 仍可读取（get 局部列表外项）。
    let shadow = super::locate(
        &input,
        &ResourceBudget::default(),
        ResourceScope::User,
        None,
        "dup",
    )
    .expect("shadow 项必须可按 URI 定位");
    assert_eq!(shadow.scope, ResourceScope::User);
    let response = super::skills_get_response(
        &input,
        &ResourceBudget::default(),
        "skill://user/dup/SKILL.md",
    )
    .expect("shadow 的 skills/get 必须成功");
    assert_eq!(response.skill.uri, "skill://user/dup/SKILL.md");
    assert_eq!(
        response.skill.frontmatter.get("name"),
        Some(&serde_json::json!("dup"))
    );
}

#[test]
fn illegal_names_and_non_text_entry_are_not_published() {
    let base = tempdir();
    // 名称含 `/`：URI 段非法 → 不公开。
    let bad = base.path().join("bad");
    write(&bad.join("SKILL.md"), skill_md("bad/name", "d").as_bytes());
    // SKILL.md 非 UTF-8 → 不公开。
    let binary = base.path().join("binary");
    write(&binary.join("SKILL.md"), &[0xff, 0xfe, 0x01]);
    // frontmatter 缺 description → 不公开。
    let incomplete = base.path().join("incomplete");
    write(
        &incomplete.join("SKILL.md"),
        b"---\nname: incomplete\n---\n",
    );
    // 合法技能作为对照。
    skill_dir(base.path(), "good", "good");

    let catalog = super::scan_catalog(
        &input(vec![root(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    let names: Vec<&str> = catalog.iter().map(|record| record.name.as_str()).collect();
    assert_eq!(names, vec!["good"], "非法项全部不公开，合法项保留");
}

#[cfg(unix)]
#[test]
fn symlinked_skill_dir_and_entry_file_are_not_published() {
    let outside = tempdir();
    skill_dir(outside.path(), "linked", "linked");

    let base = tempdir();
    std::os::unix::fs::symlink(outside.path().join("linked"), base.path().join("linked"))
        .expect("建 symlink 目录");

    let entry_target = base.path().join("real-entry.md");
    write(&entry_target, skill_md("symlinked-entry", "d").as_bytes());
    let linked_entry = base.path().join("entry-link");
    std::fs::create_dir(&linked_entry).expect("建目录");
    std::os::unix::fs::symlink(&entry_target, linked_entry.join("SKILL.md"))
        .expect("建 symlink 文件");

    let catalog = super::scan_catalog(
        &input(vec![root(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    assert!(
        catalog.is_empty(),
        "symlink 技能目录与 symlink SKILL.md 都不得公开，实际：{:?}",
        catalog
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn oversized_attachment_is_excluded_but_skill_stays() {
    let base = tempdir();
    let dir = skill_dir(base.path(), "alpha", "alpha");
    write(&dir.join("small.txt"), b"ok");
    write(&dir.join("big.txt"), &[b'a'; 64]);

    let budget = ResourceBudget {
        max_file_bytes: 48,
        ..ResourceBudget::default()
    };
    let catalog = super::scan_catalog(
        &input(vec![root(base.path(), ResourceScope::Project)]),
        &budget,
    );
    assert_eq!(catalog.len(), 1);
    let relative: Vec<&str> = catalog[0]
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    assert_eq!(
        relative,
        vec!["SKILL.md", "small.txt"],
        "超预算附件不列入 manifest"
    );
}

#[test]
fn hidden_directories_are_skipped() {
    let base = tempdir();
    skill_dir(base.path(), ".hidden-skill", "hidden-skill");
    skill_dir(base.path(), "visible", "visible");
    let catalog = super::scan_catalog(
        &input(vec![root(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    let names: Vec<&str> = catalog.iter().map(|record| record.name.as_str()).collect();
    assert_eq!(names, vec!["visible"]);
}

#[test]
fn builtin_catalog_appends_lowest_priority_and_respects_disable_flag() {
    let base = tempdir();
    // 与 builtin 同名（use-artifacts）的本地技能：本地必须赢，builtin 同名声不出现。
    skill_dir(base.path(), "use-artifacts", "use-artifacts");

    let enabled = WorkspaceResourcesInput {
        skill_roots: vec![root(base.path(), ResourceScope::Project)],
        agent_roots: Vec::new(),
        disable_bundled: false,
        budget: ResourceBudget::default(),
    };
    let catalog = super::scan_catalog(&enabled, &ResourceBudget::default());
    let local: Vec<&str> = catalog
        .iter()
        .filter(|record| record.scope == ResourceScope::Project)
        .map(|record| record.name.as_str())
        .collect();
    assert_eq!(local, vec!["use-artifacts"]);
    let builtin_count = catalog
        .iter()
        .filter(|record| record.scope == ResourceScope::Builtin)
        .count();
    assert_eq!(
        builtin_count,
        crate::resources::builtin::BUILTIN_SKILLS.len() - 1,
        "同名的 builtin 项被本地遮蔽"
    );
    assert_eq!(
        catalog.last().expect("非空").scope,
        ResourceScope::Builtin,
        "builtin 恒为最低优先级（列表尾部）"
    );

    let disabled = WorkspaceResourcesInput {
        disable_bundled: true,
        ..enabled
    };
    assert!(
        super::scan_catalog(&disabled, &ResourceBudget::default())
            .iter()
            .all(|record| record.scope != ResourceScope::Builtin),
        "disable_bundled 必须关闭 builtin 面"
    );
}

#[test]
fn locate_reports_missing_scope_and_missing_skill_distinctly() {
    let base = tempdir();
    skill_dir(base.path(), "alpha", "alpha");
    let input = input(vec![root(base.path(), ResourceScope::Project)]);

    assert_eq!(
        super::locate(
            &input,
            &ResourceBudget::default(),
            ResourceScope::Project,
            None,
            "nope"
        )
        .unwrap_err(),
        ResourceError::NotFound
    );
    assert_eq!(
        super::locate(
            &input,
            &ResourceBudget::default(),
            ResourceScope::User,
            None,
            "alpha"
        )
        .unwrap_err(),
        ResourceError::InvalidUri,
        "scope 未配置根与「技能不存在」必须内部可区分"
    );
}

#[test]
fn skills_list_response_shape_and_errors() {
    let base = tempdir();
    let dir = skill_dir(base.path(), "alpha", "alpha");
    write(&dir.join("extra.txt"), b"x");
    let input = input(vec![root(base.path(), ResourceScope::Project)]);
    let budget = ResourceBudget::default();

    let response = super::skills_list_response(&input, &budget, None).expect("list 成功");
    assert_eq!(response.skills.len(), 1);
    assert!(response.next_cursor.is_none(), "首期不分页");
    assert!(response.ttl_ms.is_none() && response.cache_scope.is_none());
    let entry = &response.skills[0];
    assert_eq!(entry.uri, "skill://project/alpha/SKILL.md");
    assert_eq!(
        entry.frontmatter.get("name"),
        Some(&serde_json::json!("alpha"))
    );
    assert_eq!(entry.resources.as_ref().expect("manifest").len(), 2);

    assert_eq!(
        super::skills_list_response(&input, &budget, Some("cursor".to_string())).unwrap_err(),
        ResourceError::InvalidUri,
        "首期不分页：非空 cursor 必须拒绝（不得伪装分页）"
    );
    assert_eq!(
        super::skills_get_response(&input, &budget, "skill://project/nope/SKILL.md").unwrap_err(),
        ResourceError::NotFound
    );
    assert_eq!(
        super::skills_get_response(&input, &budget, "skill://project/alpha/extra.txt").unwrap_err(),
        ResourceError::InvalidUri,
        "skills/get 只接受入口 URI"
    );
    assert_eq!(
        super::skills_get_response(&input, &budget, "not a uri").unwrap_err(),
        ResourceError::InvalidUri
    );
}

#[test]
fn record_read_file_rejects_unlisted_paths() {
    let base = tempdir();
    let dir = skill_dir(base.path(), "alpha", "alpha");
    write(&dir.join("extra.txt"), b"listed");
    let catalog = super::scan_catalog(
        &input(vec![root(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    let record = &catalog[0];
    let (bytes, file) = record.read_file("extra.txt", 1024).expect("列出项可读");
    assert_eq!(bytes, b"listed");
    assert!(file.text);
    assert_eq!(
        record.read_file("not-listed.txt", 1024).unwrap_err(),
        ResourceError::NotFound,
        "未列入 manifest 的路径不可读"
    );
}
