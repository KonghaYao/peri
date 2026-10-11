//! `resources` provider 级证据：list 聚合与「每项可读」、模板形状、`_meta` 投影
//! 与读取错误分类（含「扫描/读取间替换」的 Denied）。

use std::path::Path;

use peri_acp_types::workspace_resources::{
    ResourceScope, INSTRUCTION_INDEX_URI, INSTRUCTION_MAIN_URI, META_KEY_DIGEST, META_KEY_PLUGIN,
    META_KEY_SCOPE, MIME_MARKDOWN,
};

use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

fn write(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建父目录");
    }
    std::fs::write(path, content).expect("写夹具");
}

fn write_skill(root: &Path, dir_name: &str, name: &str) {
    write(
        &root.join(dir_name).join("SKILL.md"),
        format!("---\nname: {name}\ndescription: demo\n---\nbody\n").as_bytes(),
    );
}

/// 完整输入：project 技能根、plugin 技能根、agent 根 + 工作区指令文档。
fn full_input(skill_root: &Path, plugin_root: &Path, agent_root: &Path) -> WorkspaceResourcesInput {
    WorkspaceResourcesInput {
        instruction_excludes: Vec::new(),
        skill_roots: vec![
            ResourceRoot::new(skill_root, ResourceScope::Project),
            ResourceRoot::plugin(plugin_root, "plug"),
        ],
        agent_roots: vec![ResourceRoot::new(agent_root, ResourceScope::Project)],
        disable_bundled: true,
        budget: ResourceBudget::default(),
    }
}

#[test]
fn list_aggregates_skills_agents_and_instructions_and_every_item_is_readable() {
    let cwd = tempdir();
    write(&cwd.path().join("CLAUDE.md"), b"project instructions\n");
    write(&cwd.path().join("CLAUDE.local.md"), b"local overlay\n");

    let skills = tempdir();
    write_skill(skills.path(), "alpha", "alpha");
    write(
        &skills.path().join("alpha").join("scripts").join("run.sh"),
        b"#!/bin/sh\n",
    );
    write(&skills.path().join("alpha").join("data.bin"), &[0x00, 0xff]);

    let plugins = tempdir();
    write_skill(plugins.path(), "beta", "beta");

    let agents = tempdir();
    write(
        &agents.path().join("coder.md"),
        b"---\nname: Coder\ndescription: writes code\n---\nbody\n",
    );

    let provider = WorkspaceResourceProvider::new(
        cwd.path(),
        full_input(skills.path(), plugins.path(), agents.path()),
    );
    let resources = provider.list_resources();
    let uris: Vec<&str> = resources
        .iter()
        .map(|resource| resource.uri.as_str())
        .collect();
    assert!(uris.contains(&"skill://project/alpha/SKILL.md"));
    assert!(uris.contains(&"skill://project/alpha/scripts/run.sh"));
    assert!(uris.contains(&"skill://project/alpha/data.bin"));
    assert!(uris.contains(&"skill://plugin/plug/beta/SKILL.md"));
    assert!(uris.contains(&"agent://project/coder/agent.md"));
    assert!(uris.contains(&INSTRUCTION_MAIN_URI));
    assert!(uris.contains(&INSTRUCTION_LOCAL_URI));
    assert!(uris.contains(&INSTRUCTION_INDEX_URI));

    // 「list 每项可读」：逐个 URI 走同一个 read 入口。
    for resource in &resources {
        provider
            .read(&resource.uri)
            .unwrap_or_else(|error| panic!("list 项必须可读：{}（{error}）", resource.uri));
    }

    // `_meta` 投影：scope / plugin / digest。
    let plugin_entry = resources
        .iter()
        .find(|resource| resource.uri == "skill://plugin/plug/beta/SKILL.md")
        .expect("plugin 技能");
    let meta = plugin_entry.meta.as_ref().expect("meta 必填").0.clone();
    assert_eq!(
        meta.get(META_KEY_SCOPE).and_then(|v| v.as_str()),
        Some("plugin")
    );
    assert_eq!(
        meta.get(META_KEY_PLUGIN).and_then(|v| v.as_str()),
        Some("plug")
    );
    assert!(meta
        .get(META_KEY_DIGEST)
        .and_then(|v| v.as_str())
        .expect("digest")
        .starts_with("sha256:"));

    // 二进制附件以 blob 读取（base64 由 handler 投影；provider 侧给原始字节）。
    let payload = provider
        .read("skill://project/alpha/data.bin")
        .expect("blob 可读");
    match payload.body {
        ResourceBody::Blob(bytes) => assert_eq!(bytes, vec![0x00, 0xff]),
        ResourceBody::Text(_) => panic!("NUL 字节必须按 blob 处理"),
    }

    // 未知 scheme 与无根 scope 的分类。
    assert_eq!(
        provider.read("http://example.test/x").unwrap_err(),
        ResourceError::InvalidUri
    );
    assert_eq!(
        provider
            .read("skill://builtin/use-artifacts/SKILL.md")
            .unwrap_err(),
        ResourceError::InvalidUri,
        "disable_bundled 时 builtin scope 无根"
    );
    assert_eq!(
        provider.read("skill://project/nope/SKILL.md").unwrap_err(),
        ResourceError::NotFound
    );
    assert_eq!(
        provider.read("agent://project/nope/agent.md").unwrap_err(),
        ResourceError::NotFound
    );
    assert_eq!(
        provider.read(INSTRUCTION_MAIN_URI).map(|p| p.uri).ok(),
        Some(INSTRUCTION_MAIN_URI.to_string())
    );
}

#[test]
fn scan_read_replacement_does_not_leak() {
    // 「扫描/读取间替换」的公开面口径：替换为 symlink 目录后，重新扫描的公开批
    // 不含 symlink 项（NotFound）；即使文件名检查通过，canonical 前缀校验仍会
    // 拦截逃逸（`path_test` 的 Denied 用例覆盖该层）。
    #[cfg(unix)]
    {
        let skills = tempdir();
        write_skill(skills.path(), "alpha", "alpha");
        write(
            &skills.path().join("alpha").join("sub").join("data.txt"),
            b"inside",
        );

        let provider = WorkspaceResourceProvider::new(
            skills.path(),
            WorkspaceResourcesInput {
                instruction_excludes: Vec::new(),
                skill_roots: vec![ResourceRoot::new(skills.path(), ResourceScope::Project)],
                agent_roots: Vec::new(),
                disable_bundled: true,
                budget: ResourceBudget::default(),
            },
        );
        // 首次读取正常（对照）。
        assert!(provider.read("skill://project/alpha/sub/data.txt").is_ok());

        // 替换：sub → 外部目录（含同名文件）。
        let outside = tempdir();
        write(&outside.path().join("data.txt"), b"outside");
        std::fs::remove_dir_all(skills.path().join("alpha").join("sub")).expect("删除原子目录");
        std::os::unix::fs::symlink(outside.path(), skills.path().join("alpha").join("sub"))
            .expect("建 symlink");

        assert_eq!(
            provider
                .read("skill://project/alpha/sub/data.txt")
                .unwrap_err(),
            ResourceError::NotFound,
            "替换后的 symlink 项不在公开批（拒绝公开，且不返回外部内容）"
        );
        // 整个技能仍在（入口未受影响），但其 manifest 已不含逃逸项。
        let entry = provider
            .skills_get("skill://project/alpha/SKILL.md")
            .expect("技能仍可读");
        let uris: Vec<&str> = entry
            .skill
            .resources
            .as_ref()
            .expect("manifest")
            .iter()
            .map(|resource| resource.uri.as_str())
            .collect();
        assert!(!uris.contains(&"skill://project/alpha/sub/data.txt"));
    }
}

#[test]
fn templates_cover_skill_and_agent_shapes_without_duplicates() {
    let cwd = tempdir();
    let provider = WorkspaceResourceProvider::new(cwd.path(), WorkspaceResourcesInput::new());
    let templates = provider.list_resource_templates();
    let uri_templates: Vec<&str> = templates
        .iter()
        .map(|template| template.uri_template.as_str())
        .collect();
    // 每个 scope 一条入口 + 一条附件；agent 五条（user/global/project/plugin/
    // builtin——W5 迁入 builtin 静态表）。
    assert_eq!(templates.len(), ResourceScope::ALL.len() * 2 + 5);
    let mut sorted = uri_templates.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), uri_templates.len(), "模板不得重复");
    assert!(uri_templates.contains(&"skill://user/{name}/SKILL.md"));
    assert!(uri_templates.contains(&"skill://plugin/{plugin_name}/{name}/SKILL.md"));
    assert!(uri_templates.contains(&"skill://user/{name}/{+path}"));
    assert!(uri_templates.contains(&"agent://project/{name}/agent.md"));
    assert!(uri_templates.contains(&"agent://plugin/{plugin_name}/{name}/agent.md"));
    assert!(uri_templates.contains(&"agent://builtin/{name}/agent.md"));
    // 模板的 MIME 与描述存在（供宿主发现面展示）。
    let entry = templates
        .iter()
        .find(|template| template.uri_template == "skill://user/{name}/SKILL.md")
        .expect("入口模板");
    assert_eq!(entry.mime_type.as_deref(), Some("text/markdown"));
    assert!(entry.description.is_some());
}

#[test]
fn empty_input_still_lists_builtin_skills_and_instruction_index() {
    let cwd = tempdir();
    let provider = WorkspaceResourceProvider::new(cwd.path(), WorkspaceResourcesInput::new());
    let resources = provider.list_resources();
    let uris: Vec<&str> = resources
        .iter()
        .map(|resource| resource.uri.as_str())
        .collect();
    assert!(
        uris.contains(&INSTRUCTION_INDEX_URI),
        "空工作区的 index 恒可读"
    );
    assert!(
        uris.iter().any(|uri| uri.starts_with("skill://builtin/")),
        "builtin 默认启用（7 个内置技能）"
    );
    assert!(
        !uris.contains(&INSTRUCTION_MAIN_URI),
        "无主文档时不列出 main"
    );
}

#[test]
fn meta_sections_are_listed_with_digest_meta_and_readable() {
    // J6：段落覆盖文档与技能/agent/指令并列进入同一公开批；list 只列实际存在的
    // `.peri/meta/*.md` stem（含不在 SECTION_IDS 的项），正文不预取。
    let cwd = tempdir();
    write(
        &cwd.path().join(".peri").join("meta").join("01_intro.md"),
        "override 段落：中文\n".as_bytes(),
    );
    write(
        &cwd.path().join(".peri").join("meta").join("custom.md"),
        b"unknown stem\n",
    );
    let provider = WorkspaceResourceProvider::new(cwd.path(), WorkspaceResourcesInput::new());

    let resources = provider.list_resources();
    let meta_uris: Vec<&str> = resources
        .iter()
        .map(|resource| resource.uri.as_str())
        .filter(|uri| uri.starts_with("peri-meta://"))
        .collect();
    assert_eq!(
        meta_uris,
        vec![
            "peri-meta://workspace/01_intro",
            "peri-meta://workspace/custom"
        ]
    );

    // 「list 每项可读」对段落覆盖同样成立。
    for resource in &resources {
        provider
            .read(&resource.uri)
            .unwrap_or_else(|error| panic!("list 项必须可读：{}（{error}）", resource.uri));
    }

    let entry = resources
        .iter()
        .find(|resource| resource.uri == "peri-meta://workspace/01_intro")
        .expect("段落覆盖条目");
    assert_eq!(entry.mime_type.as_deref(), Some(MIME_MARKDOWN));
    assert_eq!(entry.name, "01_intro");
    let meta = entry.meta.as_ref().expect("meta 必填").0.clone();
    assert_eq!(
        meta.get(META_KEY_SCOPE).and_then(|value| value.as_str()),
        Some("project"),
        "workspace 绑定资源（与 peri-instruction 同口径）"
    );
    assert_eq!(
        meta.get(META_KEY_DIGEST).and_then(|value| value.as_str()),
        Some(
            peri_acp_types::workspace_resources::digest_bytes("override 段落：中文\n".as_bytes())
                .as_str()
        )
    );

    // 目录不存在（未配置覆盖的工作区）：公开批不含 peri-meta，且不是错误。
    let empty = tempdir();
    let provider = WorkspaceResourceProvider::new(empty.path(), WorkspaceResourcesInput::new());
    assert!(provider
        .list_resources()
        .iter()
        .all(|resource| !resource.uri.starts_with("peri-meta://")));
}

#[test]
fn skills_endpoints_delegate_to_catalog() {
    let cwd = tempdir();
    let skills = tempdir();
    write_skill(skills.path(), "alpha", "alpha");
    let provider = WorkspaceResourceProvider::new(
        cwd.path(),
        WorkspaceResourcesInput {
            instruction_excludes: Vec::new(),
            skill_roots: vec![ResourceRoot::new(skills.path(), ResourceScope::Project)],
            agent_roots: Vec::new(),
            disable_bundled: true,
            budget: ResourceBudget::default(),
        },
    );
    let list = provider.skills_list(None).expect("skills/list");
    assert_eq!(list.skills.len(), 1);
    let get = provider
        .skills_get("skill://project/alpha/SKILL.md")
        .expect("skills/get");
    assert_eq!(get.skill.uri, "skill://project/alpha/SKILL.md");
    assert!(provider.skills_list(Some("c1".into())).is_err());
}
