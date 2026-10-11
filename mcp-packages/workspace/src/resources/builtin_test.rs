//! `resources::builtin` 证据：注册表与 frontmatter 一致性、入口字节 digest、
//! `disable_bundled` 是**调用方**（catalog）的过滤位。

use super::*;
use peri_acp_types::workspace_resources::digest_bytes;

#[test]
fn builtin_registry_names_match_frontmatter() {
    assert!(!BUILTIN_SKILLS.is_empty(), "builtin 注册表不得为空");
    let mut names: Vec<&str> = BUILTIN_SKILLS.iter().map(|skill| skill.name).collect();
    names.sort();
    let len = names.len();
    names.dedup();
    assert_eq!(names.len(), len, "builtin 名称唯一");

    for skill in BUILTIN_SKILLS {
        let map = crate::resources::frontmatter::frontmatter_json(skill.content)
            .unwrap_or_else(|| panic!("{} frontmatter 必须可解析", skill.name));
        let (name, description) = crate::resources::frontmatter::frontmatter_summary(&map)
            .unwrap_or_else(|| panic!("{} 必须有 name/description", skill.name));
        assert_eq!(name, skill.name, "注册表名必须与 frontmatter name 一致");
        assert!(!description.is_empty());
    }
}

#[test]
fn builtin_skill_records_expose_entry_bytes_and_index() {
    let budget = ResourceBudget::default();
    let records = builtin_skill_records(&budget);
    assert_eq!(records.len(), BUILTIN_SKILLS.len());
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.scope, ResourceScope::Builtin);
        assert_eq!(record.files.len(), 1, "builtin 只有 SKILL.md 一个文件");
        assert_eq!(record.files[0].relative_path, SKILL_ENTRY_FILE);
        assert!(record.files[0].text);
        let bytes = builtin_skill_bytes(index).expect("索引必须命中");
        assert_eq!(
            record.files[0].digest,
            digest_bytes(bytes),
            "digest 必须按嵌入字节计算"
        );
        // 注册的顺序与 index 对应：读回的字节必须就是该条目的内容。
        assert_eq!(bytes, BUILTIN_SKILLS[index].content.as_bytes());
    }
    assert!(
        builtin_skill_bytes(BUILTIN_SKILLS.len()).is_none(),
        "越界索引无字节"
    );
}

#[test]
fn builtin_records_respect_file_budget() {
    let tiny = ResourceBudget {
        max_file_bytes: 16,
        ..ResourceBudget::default()
    };
    assert!(
        builtin_skill_records(&tiny).is_empty(),
        "所有 builtin 技能都超过 16 字节时必须全部跳过"
    );
}
