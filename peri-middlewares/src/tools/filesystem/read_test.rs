    #[tokio::test]
    async fn test_read_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "hello\nworld").unwrap();
        let tool = ReadFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "file.txt"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("1\thello"),
            "should contain line 1: {result}"
        );
        assert!(
            result.contains("2\tworld"),
            "should contain line 2: {result}"
        );
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ReadFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "nonexistent.txt"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("File not found"),
            "should report not found: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_read_file_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lines.txt");
        std::fs::write(&path, "L1\nL2\nL3\nL4\nL5").unwrap();
        let tool = ReadFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "lines.txt", "offset": 2, "limit": 2}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        // offset=2 → starts at index 2 (L3), limit=2 → L3 and L4
        assert!(result.contains("3\tL3"), "should contain line 3: {result}");
        assert!(result.contains("4\tL4"), "should contain line 4: {result}");
        assert!(!result.contains("L1"), "should not contain L1");
        assert!(!result.contains("L5"), "should not contain L5");
    }

    #[tokio::test]
    async fn test_read_file_binary_extension() {
        let dir = tempfile::tempdir().unwrap();
        // Binary extension check happens before file read, no need to create the file
        let tool = ReadFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "image.png"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("BINARY FILE DETECTED"),
            "should detect binary: {result}"
        );
    }

    #[tokio::test]
    async fn test_read_file_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abs.txt");
        std::fs::write(&path, "absolute").unwrap();
        let tool = ReadFileTool::new("/tmp");
        let result = tool
            .invoke(serde_json::json!({"file_path": path.to_str().unwrap()}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("absolute"),
            "should read via absolute path: {result}"
        );
    }

    #[tokio::test]
    async fn test_read_file_offset_exceeds_length() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("short.txt"), "one\ntwo").unwrap();
        let tool = ReadFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "short.txt", "offset": 999}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds file length"),
            "offset 超出文件长度应返回错误而非 panic: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_read_file_too_large() {
        let dir = tempfile::tempdir().unwrap();
        // 创建一个超过 MAX_FILE_SIZE 的稀疏文件
        let large_path = dir.path().join("huge.txt");
        let f = std::fs::File::create(&large_path).unwrap();
        f.set_len(MAX_FILE_SIZE + 1).unwrap();
        drop(f);
        let tool = ReadFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "huge.txt"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("File too large"),
            "超大文件应返回 File too large 错误: {err_msg}"
        );
    }

    #[test]
    fn test_description_extended() {
        let tool = ReadFileTool::new("/tmp");
        let desc = tool.description();
        assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
        assert!(
            desc.contains("Error handling:"),
            "description 应包含 Error handling 段落"
        );
        assert!(desc.contains("line numbers"), "description 应提及行号格式");
        assert!(
            desc.len() > 200,
            "description 应为扩展后的多段落文本，长度 > 200 字符"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_tool_name_is_Read() {
        let tool = ReadFileTool::new("/tmp");
        assert_eq!(tool.name(), "Read");
    }

    #[tokio::test]
    async fn test_pdf_with_pages_returns_placeholder() {
        let tool = ReadFileTool::new("/tmp");
        let result = tool
            .invoke(serde_json::json!({"file_path": "test.pdf", "pages": "1-5"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("PDF READING NOT YET SUPPORTED"),
            "should return placeholder: {result}"
        );
    }

    #[tokio::test]
    async fn test_pdf_without_pages_returns_binary() {
        let tool = ReadFileTool::new("/tmp");
        let result = tool
            .invoke(serde_json::json!({"file_path": "test.pdf"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("BINARY FILE DETECTED"),
            "should return binary: {result}"
        );
    }
