use super::*;

/// [M8] 坏条目混合好条目：坏条目被**隔离**（不整批失败），好条目照常进入发现
/// 结果，并报出被隔离计数。
#[test]
fn collect_entries_isolates_bad_entries_and_keeps_good_ones() {
    let dto = |uri: &str, frontmatter: serde_json::Value| -> SkillListEntryDto {
        serde_json::from_value(serde_json::json!({
            "uri": uri,
            "frontmatter": frontmatter,
        }))
        .unwrap()
    };
    let dto_entries = vec![
        dto(
            "skill://a/SKILL.md",
            serde_json::json!({ "name": "a", "description": "A" }),
        ),
        // 缺必填字段
        dto(
            "skill://b/SKILL.md",
            serde_json::json!({ "description": "B" }),
        ),
        // 类型不符（非字符串）
        dto(
            "skill://c/SKILL.md",
            serde_json::json!({ "name": "c", "description": 7 }),
        ),
        dto(
            "skill://d/SKILL.md",
            serde_json::json!({ "name": "d", "description": "D" }),
        ),
    ];

    let (entries, rejected) = collect_entries("demo", dto_entries, "test");
    assert_eq!(entries.len(), 2, "好条目必须保留");
    assert_eq!(rejected, 2, "坏条目被隔离并计数");
    assert_eq!(entries[0].uri, "skill://a/SKILL.md");
    assert_eq!(entries[1].uri, "skill://d/SKILL.md");
}
