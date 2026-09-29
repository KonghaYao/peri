//! `resources::agents` 证据：两种目录形态、frontmatter 展示字段、跨根去重与
//! symlink/隐藏跳过。

use std::path::Path;

use super::*;
use crate::resources::ResourceRoot;
use peri_acp_types::workspace_resources::ResourceScope;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建父目录");
    }
    std::fs::write(path, content).expect("写夹具");
}

fn input(roots: Vec<ResourceRoot>) -> WorkspaceResourcesInput {
    WorkspaceResourcesInput {
        skill_roots: Vec::new(),
        agent_roots: roots,
        disable_bundled: true,
        budget: ResourceBudget::default(),
    }
}

#[test]
fn scan_supports_file_and_directory_forms() {
    let base = tempdir();
    write(
        &base.path().join("coder.md"),
        "---\nname: Coder\ndescription: writes code\n---\nbody\n",
    );
    write(
        &base.path().join("reviewer").join("agent.md"),
        "---\nname: Reviewer\ndescription: reviews code\n---\nbody\n",
    );
    // 没有 agent.md 的目录、非 md 文件与隐藏文件都不进入目录。
    std::fs::create_dir(base.path().join("empty-dir")).expect("建目录");
    write(&base.path().join("notes.txt"), "not an agent");
    write(&base.path().join(".hidden.md"), "hidden");

    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::new(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    let mut ids: Vec<&str> = catalog
        .iter()
        .map(|record| record.agent_id.as_str())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["coder", "reviewer"]);
    let coder = catalog
        .iter()
        .find(|record| record.agent_id == "coder")
        .expect("coder");
    assert_eq!(coder.display_name, "Coder");
    assert_eq!(coder.description, "writes code");
    assert_eq!(
        coder.uri().as_deref(),
        Some("agent://project/coder/agent.md")
    );
    let reviewer = catalog
        .iter()
        .find(|record| record.agent_id == "reviewer")
        .expect("reviewer");
    assert_eq!(reviewer.relative_path, "reviewer/agent.md");
}

#[test]
fn scan_without_frontmatter_falls_back_to_id_and_empty_description() {
    let base = tempdir();
    write(&base.path().join("plain.md"), "no frontmatter here\n");
    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::new(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].display_name, "plain");
    assert_eq!(catalog[0].description, "");
}

#[test]
fn catalog_dedupes_across_roots_first_wins() {
    let high = tempdir();
    write(&high.path().join("dup.md"), "---\nname: high\n---\n");
    let low = tempdir();
    write(&low.path().join("dup.md"), "---\nname: low\n---\n");
    let catalog = super::scan_catalog(
        &input(vec![
            ResourceRoot::new(high.path(), ResourceScope::Project),
            ResourceRoot::new(low.path(), ResourceScope::User),
        ]),
        &ResourceBudget::default(),
    );
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].display_name, "high");
    assert_eq!(catalog[0].scope, ResourceScope::Project);

    // shadow 项仍可按 URI 定位（与技能面同口径）。
    let shadow = super::locate(
        &input(vec![
            ResourceRoot::new(high.path(), ResourceScope::Project),
            ResourceRoot::new(low.path(), ResourceScope::User),
        ]),
        &ResourceBudget::default(),
        ResourceScope::User,
        None,
        "dup",
    )
    .expect("shadow 可定位");
    assert_eq!(shadow.display_name, "low");
}

#[cfg(unix)]
#[test]
fn symlinked_definitions_are_skipped() {
    let outside = tempdir();
    write(
        &outside.path().join("linked.md"),
        "---\nname: linked\n---\n",
    );
    let base = tempdir();
    std::os::unix::fs::symlink(
        outside.path().join("linked.md"),
        base.path().join("linked.md"),
    )
    .expect("建 symlink");
    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::new(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    assert!(catalog.is_empty(), "symlink 定义不得公开");
}

#[test]
fn plugin_scope_carries_plugin_name_in_uri() {
    let base = tempdir();
    write(&base.path().join("helper.md"), "---\nname: Helper\n---\n");
    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::plugin(base.path(), "my-plugin")]),
        &ResourceBudget::default(),
    );
    assert_eq!(catalog.len(), 1);
    assert_eq!(
        catalog[0].uri().as_deref(),
        Some("agent://plugin/my-plugin/helper/agent.md")
    );
}
