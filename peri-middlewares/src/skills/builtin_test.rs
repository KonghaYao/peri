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
