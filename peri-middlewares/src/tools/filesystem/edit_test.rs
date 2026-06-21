    #[tokio::test]
    async fn test_edit_file_single_replace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello foo world").unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "f.txt", "old_string": "foo", "new_string": "bar"}),
             peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        // "foo" → "bar": same line count, one occurrence
        assert!(
            result.contains("Replaced text"),
            "unexpected: {result}"
        );
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "hello bar world");
    }

    #[tokio::test]
    async fn test_edit_file_old_string_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "f.txt", "old_string": "missing", "new_string": "x"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report not found: {err}"
        );
    }

    #[tokio::test]
    async fn test_edit_file_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x x x").unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({
            "file_path": "f.txt",
            "old_string": "x",
            "new_string": "y",
            "replace_all": true
        }), peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .unwrap();
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "y y y");
    }

    #[tokio::test]
    async fn test_edit_file_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "foo and foo").unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "f.txt", "old_string": "foo", "new_string": "bar"}),
             peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("not unique"),
            "should report ambiguity: {err}"
        );
    }

    #[tokio::test]
    async fn test_edit_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "ghost.txt", "old_string": "x", "new_string": "y"}),
             peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("File not found"),
            "should report file not found: {err}"
        );
    }

    #[tokio::test]
    async fn test_edit_file_empty_old_string_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "f.txt", "old_string": "", "new_string": "x", "replace_all": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("cannot be empty"),
            "empty old_string should be rejected: {err}"
        );
        // 文件内容不应被修改
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "hello world", "file should not be modified");
    }

    #[test]
    fn test_description_extended() {
        let tool = EditFileTool::new("/tmp");
        let desc = tool.description();
        assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
        assert!(desc.contains("old_string"), "description 应提及 old_string");
        assert!(
            desc.contains("replace_all"),
            "description 应提及 replace_all"
        );
        assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_tool_name_is_Edit() {
        let tool = EditFileTool::new("/tmp");
        assert_eq!(tool.name(), "Edit");
    }

    #[tokio::test]
    async fn test_edit_not_found_with_fuzzy_prefix_match() {
        let dir = tempfile::tempdir().unwrap();
        // old_string 有 7 行，前 5 行与文件完全一致，第 6 行不同
        // 策略 1（前缀匹配）取前 5 行做 find → 命中
        let file_content = "a\nb\nc\nd\ne\nf\ng\n";
        std::fs::write(dir.path().join("f.txt"), file_content).unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let err = tool
            .invoke(serde_json::json!({
                "file_path": "f.txt",
                "old_string": "a\nb\nc\nd\ne\nDIFFERENT\nextra\n",
                "new_string": "x"
            }), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("first 5 lines matched lines 1-5"),
            "应报告前缀匹配位置: {msg}"
        );
        assert!(
            msg.contains("Please Read this file"),
            "应建议重新 Read: {msg}"
        );
    }

    #[tokio::test]
    async fn test_edit_not_found_with_line_diff_hint() {
        let dir = tempfile::tempdir().unwrap();
        // 前 5 行完全不匹配，但中间有近似区域
        std::fs::write(
            dir.path().join("f.txt"),
            "aaa\nbbb\nccc\nddd\neee\nline1\nline2_CHANGED\nline3\nfff\nggg\n",
        )
        .unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let err = tool
            .invoke(serde_json::json!({
                "file_path": "f.txt",
                "old_string": "line1\nline2\nline3\n",
                "new_string": "x"
            }), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Please Read this file") || msg.contains("Closest match"),
            "应提供匹配提示: {msg}"
        );
    }

    #[tokio::test]
    async fn test_edit_not_found_long_old_string_skip_fuzzy() {
        let dir = tempfile::tempdir().unwrap();
        let long_line = "x".repeat(1000);
        let content = format!("{long_line}\n");
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        // old_string > 5000 字符
        let giant_old = "y".repeat(6000);
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let err = tool
            .invoke(serde_json::json!({
                "file_path": "f.txt",
                "old_string": giant_old,
                "new_string": "x"
            }), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Please Read this file"),
            "超长 old_string 应只给建议: {msg}"
        );
        assert!(
            !msg.contains("matched lines"),
            "超长 old_string 不应做模糊匹配: {msg}"
        );
    }

    #[tokio::test]
    async fn test_edit_not_unique_shows_line_ranges() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aaa\nfoo\nbbb\nfoo\nccc\n").unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let err = tool
            .invoke(serde_json::json!({
                "file_path": "f.txt",
                "old_string": "foo",
                "new_string": "bar"
            }), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("line 2"), "应报告第一个匹配行号: {msg}");
        assert!(msg.contains("line 4"), "应报告第二个匹配行号: {msg}");
        assert!(
            msg.contains("Match locations"),
            "应包含匹配位置标签: {msg}"
        );
    }

    #[tokio::test]
    async fn test_edit_not_unique_many_occurrences_truncated() {
        let dir = tempfile::tempdir().unwrap();
        // 15 次 "x\n"
        let content = "x\n".repeat(15);
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        let tool = EditFileTool::new(dir.path().to_str().unwrap());
        let err = tool
            .invoke(serde_json::json!({
                "file_path": "f.txt",
                "old_string": "x",
                "new_string": "y"
            }), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("15 occurrences"), "应报告总匹配数: {msg}");
        // 最多报告 10 个位置
        let location_count = msg.matches("line").count();
        assert!(
            location_count <= 10,
            "超过 10 个匹配时应截断位置列表，实际 {location_count} 个: {msg}"
        );
    }
