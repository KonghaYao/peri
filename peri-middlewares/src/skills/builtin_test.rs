use std::path::PathBuf;

use crate::skills::{scan_skill_roots, SkillRoot, SkillSource, SkillsMiddleware};

use super::{parse_builtin_frontmatter, BUILTIN_SKILLS};

#[test]
fn test_builtin_skills_non_empty() {
    // 至少含 use-artifacts 验证用例
    assert!(BUILTIN_SKILLS.iter().any(|s| s.name == "use-artifacts"),
        "BUILTIN_SKILLS 应含 use-artifacts");
}

#[test]
fn test_builtin_skills_unique_names() {
    let mut names: Vec<&str> = BUILTIN_SKILLS.iter().map(|s| s.name).collect();
    names.sort();
    let original_len = names.len();
    names.dedup();
    assert_eq!(names.len(), original_len, "BUILTIN_SKILLS 名称不应重复");
}

#[test]
fn test_builtin_skills_frontmatter_valid() {
    // 每个 BUILTIN_SKILLS 的 frontmatter 都应能解析出 name + description
    for skill in BUILTIN_SKILLS {
        let parsed = parse_builtin_frontmatter(skill.content);
        assert!(parsed.is_some(),
            "builtin skill {} frontmatter 解析失败", skill.name);
        let (name, aliases, desc) = parsed.unwrap();
        assert_eq!(name, skill.name,
            "builtin skill {} frontmatter name 字段不匹配", skill.name);
        if skill.name == "programmatic-tool-calling" {
            assert_eq!(aliases, vec!["ptc"]);
        }
        assert!(!desc.is_empty(),
            "builtin skill {} description 为空", skill.name);
    }
}

#[test]
fn test_ultra_adlc_skill_registered_and_discriminating() {
    let skill = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc");
    let (name, aliases, description) =
        parse_builtin_frontmatter(skill.content).expect("ultra-adlc frontmatter 应有效");

    assert_eq!(name, "ultra-adlc");
    assert!(aliases.is_empty());
    let description = description.to_ascii_lowercase();
    assert!(description.contains("very large"));
    assert!(description.contains("end-to-end"));
    assert!(description.contains("do not use for ordinary"));
    assert!(skill.content.contains("userInvocable: true"));
    assert!(skill.content.contains("argumentHint:"));
}

#[test]
fn test_ultra_adlc_skill_is_discoverable_in_builtin_summary() {
    let skills = scan_skill_roots(&[SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Builtin,
        plugin_name: None,
    }]);
    let summary = SkillsMiddleware::build_summary(&skills);

    assert!(
        summary.contains("**ultra-adlc** [builtin]"),
        "builtin 摘要应暴露 ultra-adlc，实际: {summary}"
    );
}

#[test]
fn test_ultra_adlc_skill_encodes_peri_workflow_contract() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content;

    for marker in [
        "./.peri/adlc/",
        "intent.md",
        "execution.md",
        "evidence.md",
        "exactly two **logical** workflows",
        "discovery-design",
        "delivery-convergence",
        "AskUserQuestion",
        "SearchExtraTools(\"workflow\")",
        "ExecuteExtraTool(\"Workflow\"",
        ".claude/workflow-runs/<run-id>/state.json",
        "maxConcurrency: 12",
        "() => agent(",
        "Date.now()",
        "new Date()",
        "Math.random()",
        "phase(name) only marks a stage",
        "delivery_status",
        "path_allowlist",
        "git status --porcelain",
        "at most 4",
    ] {
        assert!(content.contains(marker), "ultra-adlc 应锁定 {marker}");
    }

    assert!(
        !content.contains("./peri/adlc/"),
        "ultra-adlc 不得再锁定已删除路径 ./peri/adlc/"
    );
    assert!(
        !content.contains("at most three"),
        "ultra-adlc 提问上限应与 AskUserQuestion 对齐为 at most 4"
    );

    for profile in ["`fable`", "`opus`", "`sonnet`", "`haiku`"] {
        assert!(
            content.contains(profile),
            "ultra-adlc 应覆盖 profile {profile}"
        );
    }
}

#[test]
fn test_ultra_adlc_skill_guards_complete_delivery_and_audit() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content;

    for marker in [
        "There is no",
        "`partially_complete`",
        "Completion Assessor",
        "exactly one new",
        "100% coverage",
        "gap-round-N.md",
        "learning/agent-performance.md",
        "Only when the assessor verdict is `complete`",
        "must not receive a success performance record",
        "Never commit, push, publish, deploy",
        "Never put a secret, token, password",
    ] {
        assert!(content.contains(marker), "ultra-adlc 应锁定 {marker}");
    }
}

#[test]
fn test_ultra_task_skill_registered_and_discoverable() {
    let skill = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-task")
        .expect("BUILTIN_SKILLS 应含 ultra-task");
    let (name, aliases, description) =
        parse_builtin_frontmatter(skill.content).expect("ultra-task frontmatter 应有效");

    assert_eq!(name, "ultra-task");
    assert!(aliases.is_empty());
    assert!(description.contains("task supervision"));
    assert!(description.contains("subagents"));
    assert!(skill.content.contains("userInvocable: true"));
    assert!(skill.content.contains("argumentHint:"));

    let skills = scan_skill_roots(&[SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Builtin,
        plugin_name: None,
    }]);
    let summary = SkillsMiddleware::build_summary(&skills);
    assert!(
        summary.contains("**ultra-task** [builtin]"),
        "builtin 摘要应暴露 ultra-task，实际: {summary}"
    );
}

#[test]
fn test_parse_builtin_frontmatter_invalid_returns_none() {
    // 格式错误的 frontmatter 应返回 None
    let bad = "no frontmatter here";
    assert!(parse_builtin_frontmatter(bad).is_none());

    let bad2 = "---\nname: only_name\n---\nbody";
    assert!(parse_builtin_frontmatter(bad2).is_none(),
        "缺 description 字段应返回 None");
}

#[test]
fn test_parse_builtin_frontmatter_valid() {
    let content = "---\nname: test-skill\ndescription: 测试 skill\n---\n\n# Body\n";
    let parsed = parse_builtin_frontmatter(content).unwrap();
    assert_eq!(parsed.0, "test-skill");
    assert!(parsed.1.is_empty());
    assert_eq!(parsed.2, "测试 skill");
}

#[test]
fn test_parse_builtin_frontmatter_trims_trailing_newline() {
    // YAML `>`（折叠标量）和 `|`（字面标量）会在末尾保留 `\n`，
    // 下游拼到 Markdown list item 末尾会让 list 渲染断裂，需要 trim
    let content = "---\nname: folded\ndescription: >\n  Multi line description.\n---\n\n# Body\n";
    let parsed = parse_builtin_frontmatter(content).unwrap();
    assert_eq!(parsed.0, "folded");
    assert!(
        !parsed.2.ends_with('\n') && !parsed.2.ends_with('\r'),
        "description 不应含尾随换行，实际: {:?}", parsed.2
    );
}
