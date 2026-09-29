//! `resources::agents` 证据：两种目录形态、frontmatter 展示字段、同根去重、
//! 跨来源同名并存（W5）、builtin 静态表与 `_meta` frontmatter 投影、symlink/隐藏跳过。

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
        instruction_excludes: Vec::new(),
        skill_roots: Vec::new(),
        agent_roots: roots,
        disable_bundled: true,
        budget: ResourceBudget::default(),
    }
}

/// 目录中的磁盘来源记录（builtin 静态表恒在，测试按需滤掉）。
fn disk_records(catalog: &[AgentRecord]) -> Vec<&AgentRecord> {
    catalog
        .iter()
        .filter(|record| record.scope != ResourceScope::Builtin)
        .collect()
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
    let records = disk_records(&catalog);
    let mut ids: Vec<&str> = records
        .iter()
        .map(|record| record.agent_id.as_str())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["coder", "reviewer"]);
    let coder = records
        .iter()
        .find(|record| record.agent_id == "coder")
        .expect("coder");
    assert_eq!(coder.display_name, "Coder");
    assert_eq!(coder.description, "writes code");
    assert_eq!(
        coder.uri().as_deref(),
        Some("agent://project/coder/agent.md")
    );
    let reviewer = records
        .iter()
        .find(|record| record.agent_id == "reviewer")
        .expect("reviewer");
    assert_eq!(reviewer.store.relative_path(), "reviewer/agent.md");
    // W5：frontmatter 逐字投影进 `_meta`（宿主目录渲染的能力推断输入）。
    let meta = coder.meta();
    let frontmatter = meta
        .get(peri_acp_types::workspace_resources::META_KEY_FRONTMATTER)
        .and_then(|value| value.as_object())
        .expect("frontmatter 投影");
    assert_eq!(
        frontmatter.get("name").and_then(|value| value.as_str()),
        Some("Coder")
    );
}

#[test]
fn scan_without_frontmatter_falls_back_to_id_and_empty_description() {
    let base = tempdir();
    write(&base.path().join("plain.md"), "no frontmatter here\n");
    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::new(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    let records = disk_records(&catalog);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].display_name, "plain");
    assert_eq!(records[0].description, "");
    // 无合法 frontmatter ⇒ 不投影 frontmatter 键（宿主据此排除候选）。
    assert!(records[0].meta().is_empty());
}

#[test]
fn catalog_keeps_same_name_across_origins_and_dedupes_within_one_origin() {
    let high = tempdir();
    write(&high.path().join("dup.md"), "---\nname: high\n---\n");
    let low = tempdir();
    write(&low.path().join("dup.md"), "---\nname: low\n---\n");
    let roots = vec![
        ResourceRoot::new(high.path(), ResourceScope::Project),
        ResourceRoot::new(low.path(), ResourceScope::User),
    ];
    let catalog = super::scan_catalog(&input(roots.clone()), &ResourceBudget::default());
    // W5/§6.2：跨来源同名并存（两个来源各有自己的 URI，均可读）。
    let records = disk_records(&catalog);
    assert_eq!(records.len(), 2);
    let project = records
        .iter()
        .find(|record| record.scope == ResourceScope::Project)
        .expect("project 项");
    let user = records
        .iter()
        .find(|record| record.scope == ResourceScope::User)
        .expect("user 项");
    assert_eq!(project.display_name, "high");
    assert_eq!(user.display_name, "low");
    assert_ne!(project.uri(), user.uri());

    // 同一来源根内同名仍先到先得（第二个根不是该 scope 的根 ⇐ 同根去重按 scope 分组）。
    let same_scope = super::scan_catalog(
        &input(vec![
            ResourceRoot::new(high.path(), ResourceScope::Project),
            ResourceRoot::new(low.path(), ResourceScope::Project),
        ]),
        &ResourceBudget::default(),
    );
    let same_scope_records = disk_records(&same_scope);
    assert_eq!(same_scope_records.len(), 1);
    assert_eq!(same_scope_records[0].display_name, "high");

    // 各自可按 URI 定位（User 项不再是 shadow）。
    let shadow = super::locate(
        &input(roots),
        &ResourceBudget::default(),
        ResourceScope::User,
        None,
        "dup",
    )
    .expect("跨来源项可定位");
    assert_eq!(shadow.display_name, "low");
}

#[test]
fn builtin_agents_are_public_and_locatable() {
    let budget = ResourceBudget::default();
    let input = input(Vec::new());
    let catalog = super::scan_catalog(&input, &budget);
    let builtin: Vec<&AgentRecord> = catalog
        .iter()
        .filter(|record| record.scope == ResourceScope::Builtin)
        .collect();
    let mut ids: Vec<&str> = builtin
        .iter()
        .map(|record| record.agent_id.as_str())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "coder",
            "explorer",
            "general-purpose",
            "plan",
            "verification",
            "web-researcher"
        ]
    );
    let coder = builtin
        .iter()
        .find(|record| record.agent_id == "coder")
        .expect("builtin coder");
    assert_eq!(
        coder.uri().as_deref(),
        Some("agent://builtin/coder/agent.md")
    );

    let located = super::locate(&input, &budget, ResourceScope::Builtin, None, "coder")
        .expect("builtin 可按 URI 定位");
    let bytes = super::read_record_definition(&located, &budget).expect("builtin 定义可读");
    let text = String::from_utf8(bytes).expect("UTF-8");
    assert!(text.contains("name: coder"), "定义正文应可读：{text:.64}");
    assert_eq!(
        located.digest,
        peri_acp_types::workspace_resources::digest_bytes(text.as_bytes())
    );

    // 未知 builtin 标识与带 plugin 段的 builtin URI 都是非法/缺失，不是 panic。
    assert!(matches!(
        super::locate(&input, &budget, ResourceScope::Builtin, None, "nope"),
        Err(ResourceError::NotFound)
    ));
    assert!(matches!(
        super::locate(
            &input,
            &budget,
            ResourceScope::Builtin,
            Some("plugin"),
            "coder"
        ),
        Err(ResourceError::InvalidUri)
    ));
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
    assert!(disk_records(&catalog).is_empty(), "symlink 定义不得公开");
}

#[test]
fn plugin_scope_carries_plugin_name_in_uri() {
    let base = tempdir();
    write(&base.path().join("helper.md"), "---\nname: Helper\n---\n");
    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::plugin(base.path(), "my-plugin")]),
        &ResourceBudget::default(),
    );
    let records = disk_records(&catalog);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].uri().as_deref(),
        Some("agent://plugin/my-plugin/helper/agent.md")
    );
}

#[test]
fn two_project_roots_prefer_the_first_one_for_same_id() {
    // F4：`{cwd}/.claude/agents` 与 `{cwd}/agents` 是两个 project 根，顺序即优先级；
    // 同名时先到者胜（与迁移前候选序一致），`agents/` 独有的定义仍可发现。
    let claude = tempdir();
    write(
        &claude.path().join("shared.md"),
        "---\nname: from-claude\n---\n",
    );
    let legacy = tempdir();
    write(
        &legacy.path().join("shared.md"),
        "---\nname: from-agents\n---\n",
    );
    write(
        &legacy.path().join("only-legacy.md"),
        "---\nname: legacy-only\n---\n",
    );

    let catalog = super::scan_catalog(
        &input(vec![
            ResourceRoot::new(claude.path(), ResourceScope::Project),
            ResourceRoot::new(legacy.path(), ResourceScope::Project),
        ]),
        &ResourceBudget::default(),
    );
    let records = disk_records(&catalog);
    let shared = records
        .iter()
        .find(|record| record.agent_id == "shared")
        .expect("同名项");
    assert_eq!(shared.display_name, "from-claude", "首个根优先");
    assert!(records
        .iter()
        .any(|record| record.agent_id == "only-legacy"));
}

#[test]
fn directory_form_shadows_file_form_for_the_same_id() {
    // F13：同 id 两形态的优先级被显式固化（目录形态先；不由字典序巧合决定）。
    let base = tempdir();
    write(&base.path().join("dup.md"), "---\nname: from-file\n---\n");
    write(
        &base.path().join("dup").join("agent.md"),
        "---\nname: from-dir\n---\n",
    );
    let catalog = super::scan_catalog(
        &input(vec![ResourceRoot::new(base.path(), ResourceScope::Project)]),
        &ResourceBudget::default(),
    );
    let records = disk_records(&catalog);
    let dup: Vec<&AgentRecord> = records
        .iter()
        .copied()
        .filter(|record| record.agent_id == "dup")
        .collect();
    assert_eq!(dup.len(), 1, "同 id 两形态只公开一项");
    assert_eq!(dup[0].display_name, "from-dir", "目录形态先");
}
