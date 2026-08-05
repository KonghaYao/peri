#[cfg(test)]
mod tests {
    use crate::sync::protocol::{FilesItem, McpItem, SettingsItem, SyncItems};

    fn settings_item() -> SettingsItem {
        SettingsItem {
            content: r#"{"key": "value"}"#.into(),
            claude_content: Some("claude".into()),
        }
    }

    // ── SyncItems：JSON 缺字段 ──

    #[test]
    fn test_sync_items_json_empty_object() {
        let items: SyncItems = serde_json::from_str("{}").expect("空对象应反序列化成功");
        assert!(items.settings.is_none());
        assert!(items.skills.is_none());
        assert!(items.mcp.is_none());
        assert!(items.plugins.is_none());
    }

    #[test]
    fn test_sync_items_json_partial_fields() {
        let json = r#"{"settings": {"content": "{}"}}"#;
        let items: SyncItems = serde_json::from_str(json).expect("缺字段应反序列化成功");
        assert!(items.settings.is_some());
        assert!(items.skills.is_none());
        assert!(items.mcp.is_none());
        assert!(items.plugins.is_none());
    }

    #[test]
    fn test_sync_items_json_serialization_unchanged() {
        let items = SyncItems {
            settings: Some(settings_item()),
            skills: None,
            mcp: None,
            plugins: None,
        };
        let json = serde_json::to_string(&items).expect("序列化应成功");
        // skip_serializing_if 保持生效：None 字段不出现。
        assert!(!json.contains("skills"));
        assert!(!json.contains("mcp"));
        assert!(json.contains("settings"));
        let back: SyncItems = serde_json::from_str(&json).expect("往返应成功");
        assert!(back.settings.is_some());
    }

    // ── SyncItems：MessagePack 缺字段 ──

    #[test]
    fn test_sync_items_msgpack_empty_map() {
        let packed = rmp_serde::to_vec(&serde_json::json!({})).expect("msgpack 序列化应成功");
        let items: SyncItems = rmp_serde::from_slice(&packed).expect("缺字段应反序列化成功");
        assert!(items.settings.is_none());
        assert!(items.skills.is_none());
        assert!(items.mcp.is_none());
        assert!(items.plugins.is_none());
    }

    #[test]
    fn test_sync_items_msgpack_partial_fields() {
        let value = serde_json::json!({
            "settings": {"content": "{}", "claude_content": null},
            "mcp": {"global": "{}"}
        });
        let packed = rmp_serde::to_vec(&value).expect("msgpack 序列化应成功");
        let items: SyncItems = rmp_serde::from_slice(&packed).expect("缺字段应反序列化成功");
        assert!(items.settings.is_some());
        assert!(items.skills.is_none());
        let mcp = items.mcp.expect("mcp 应存在");
        assert_eq!(mcp.global.as_deref(), Some("{}"));
        assert!(mcp.project.is_none());
        assert!(items.plugins.is_none());
    }

    // ── McpItem：JSON + MessagePack 缺字段 ──

    #[test]
    fn test_mcp_item_json_missing_project() {
        let mcp: McpItem = serde_json::from_str(r#"{"global": "g"}"#).expect("缺字段应成功");
        assert_eq!(mcp.global.as_deref(), Some("g"));
        assert!(mcp.project.is_none());
        let empty: McpItem = serde_json::from_str("{}").expect("空对象应成功");
        assert!(empty.global.is_none());
        assert!(empty.project.is_none());
    }

    #[test]
    fn test_mcp_item_msgpack_missing_global() {
        let packed =
            rmp_serde::to_vec(&serde_json::json!({"project": "p"})).expect("msgpack 序列化应成功");
        let mcp: McpItem = rmp_serde::from_slice(&packed).expect("缺字段应成功");
        assert!(mcp.global.is_none());
        assert_eq!(mcp.project.as_deref(), Some("p"));
    }

    #[test]
    fn test_mcp_item_msgpack_empty_map() {
        let packed = rmp_serde::to_vec(&serde_json::json!({})).expect("msgpack 序列化应成功");
        let mcp: McpItem = rmp_serde::from_slice(&packed).expect("空 map 应成功");
        assert!(mcp.global.is_none());
        assert!(mcp.project.is_none());
    }

    // ── FilesItem：缺 files 数组 ──

    #[test]
    fn test_files_item_missing_files() {
        let json = r#"{}"#;
        let item: FilesItem = serde_json::from_str(json).expect("缺 files 应成功");
        assert!(item.files.is_empty());
    }

    // ── 全字段往返 ──

    #[test]
    fn test_full_roundtrip_json_and_msgpack() {
        let items = SyncItems {
            settings: Some(settings_item()),
            skills: Some(FilesItem {
                files: vec![crate::sync::protocol::FileEntry {
                    path: "a/b.txt".into(),
                    content: b"hi".to_vec(),
                }],
            }),
            mcp: Some(McpItem {
                global: Some("g".into()),
                project: Some("p".into()),
            }),
            plugins: None,
        };
        let json = serde_json::to_string(&items).unwrap();
        let back: SyncItems = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mcp.unwrap().project.as_deref(), Some("p"));

        let packed = rmp_serde::to_vec(&items).unwrap();
        let back: SyncItems = rmp_serde::from_slice(&packed).unwrap();
        assert!(back.skills.is_some());
        assert_eq!(back.mcp.unwrap().global.as_deref(), Some("g"));
    }
}
