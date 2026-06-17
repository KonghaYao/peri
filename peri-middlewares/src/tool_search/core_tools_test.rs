    #[test]
    fn test_core_tool_not_deferred() {
        assert!(!is_deferred_tool("Read"));
    }

    #[test]
    fn test_meta_tool_not_deferred() {
        assert!(!is_deferred_tool("SearchExtraTools"));
        assert!(!is_deferred_tool("ExecuteExtraTool"));
    }

    #[test]
    fn test_deferred_tool() {
        assert!(is_deferred_tool("CronRegister"));
        assert!(is_deferred_tool("CronList"));
        assert!(is_deferred_tool("CronRemove"));
    }

    #[test]
    fn test_mcp_tool_deferred() {
        assert!(is_deferred_tool("mcp__slack__send_message"));
        assert!(is_deferred_tool("mcp__read_resource"));
    }

    #[test]
    fn test_unknown_tool_deferred() {
        assert!(is_deferred_tool("UnknownTool"));
        assert!(is_deferred_tool(""));
    }

    #[test]
    fn test_core_tools_sorted_csv_includes_all_12_tools() {
        // P1-1: 动态生成必须覆盖全部 12 个 Core 工具（含 folder_operations）
        let csv = core_tools_sorted_csv();
        for tool in [
            TOOL_READ,
            TOOL_WRITE,
            TOOL_EDIT,
            TOOL_GLOB,
            TOOL_GREP,
            TOOL_FOLDER_OPS,
            TOOL_BASH,
            TOOL_WEBFETCH,
            TOOL_WEBSEARCH,
            TOOL_AGENT,
            TOOL_ASK_USER,
            TOOL_TODO,
        ] {
            assert!(
                csv.contains(tool),
                "core_tools_sorted_csv 应包含 {tool}，实际: {csv}"
            );
        }
    }

    #[test]
    fn test_core_tools_sorted_csv_is_stable() {
        // P1-1: 多次调用必须返回相同字符串（保护 prompt cache 前缀）
        let csv1 = core_tools_sorted_csv();
        let csv2 = core_tools_sorted_csv();
        let csv3 = core_tools_sorted_csv();
        assert_eq!(csv1, csv2, "两次调用应一致");
        assert_eq!(csv2, csv3, "三次调用应一致");
    }

    #[test]
    fn test_core_tools_sorted_csv_is_alphabetically_sorted() {
        // P1-1: 排序保证跨进程/跨版本稳定（HashSet 序不确定）
        let csv = core_tools_sorted_csv();
        let parts: Vec<&str> = csv.split(", ").collect();
        let mut sorted = parts.clone();
        sorted.sort();
        assert_eq!(
            parts, sorted,
            "csv 应按字典序排序，实际: {csv}"
        );
    }
