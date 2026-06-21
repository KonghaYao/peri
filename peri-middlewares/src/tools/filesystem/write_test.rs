    #[tokio::test]
    async fn test_write_file_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "new.txt", "content": "hello"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn test_write_file_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old").unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "f.txt", "content": "new"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "new");
    }

    #[tokio::test]
    async fn test_write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "sub/dir/file.txt", "content": "deep"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(dir.path().join("sub/dir/file.txt").exists());
    }

    #[tokio::test]
    async fn test_write_file_missing_content_param() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        let result = tool.invoke(serde_json::json!({"file_path": "f.txt"}), peri_agent::tools::ToolContext::new(&[], ".")).await;
        assert!(result.is_err(), "missing content should return Err");
    }

    #[tokio::test]
    async fn test_write_file_success_message() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "msg.txt", "content": "x"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("Wrote 1 line"),
            "unexpected message: {result}"
        );
    }

    #[tokio::test]
    async fn test_write_file_multiline_message() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "multi.txt", "content": "a\nb\nc"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("Wrote 3 lines"),
            "unexpected message: {result}"
        );
    }

    #[tokio::test]
    async fn test_write_file_no_tmp_residual() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "clean.txt", "content": "data"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        // 原子写入后不应残留任何 .tmp.* 临时文件
        let tmp_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("clean.tmp."))
            .collect();
        assert!(tmp_files.is_empty(), "临时文件应在 rename 后被清除");
        assert!(dir.path().join("clean.txt").exists());
    }

    #[tokio::test]
    async fn test_write_file_error_propagates() {
        let dir = tempfile::tempdir().unwrap();
        // 在只读目录上写入应返回 Err
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o444))
                .unwrap();
        }
        let tool = WriteFileTool::new(readonly_dir.to_str().unwrap());
        let _result = tool
            .invoke(serde_json::json!({"file_path": "sub/nope.txt", "content": "x"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        #[cfg(unix)]
        assert!(_result.is_err(), "写入只读目录应返回 Err");
    }

    #[test]
    fn test_description_extended() {
        let tool = WriteFileTool::new("/tmp");
        let desc = tool.description();
        assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
        assert!(desc.contains("atomic write"), "description 应提及原子写入");
        assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_tool_name_is_Write() {
        let tool = WriteFileTool::new("/tmp");
        assert_eq!(tool.name(), "Write");
    }

    #[tokio::test]
    async fn test_write_append_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("log.txt"), "line1\n").unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "log.txt", "content": "line2\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert_eq!(content, "line1\nline2\n");
        assert!(result.contains("Appended 1 line"), "unexpected message: {result}");
        assert!(result.contains("file total: 2 lines"), "应包含总行数: {result}");
    }

    #[tokio::test]
    async fn test_write_append_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "new_append.txt", "content": "first line\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("new_append.txt")).unwrap();
        assert_eq!(content, "first line\n");
    }

    #[tokio::test]
    async fn test_write_append_multiline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(serde_json::json!({"file_path": "f.txt", "content": "b\nc\nd\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nb\nc\nd\n");
        assert!(result.contains("Appended 3 lines"), "unexpected message: {result}");
        assert!(result.contains("file total: 4 lines"), "应包含总行数: {result}");
    }

    #[tokio::test]
    async fn test_write_append_false_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old content").unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "f.txt", "content": "new", "append": false}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "new", "append=false 应覆写文件");
    }

    #[tokio::test]
    async fn test_write_append_sequential_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "chunked.txt", "content": "chunk1\n"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        tool.invoke(serde_json::json!({"file_path": "chunked.txt", "content": "chunk2\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        tool.invoke(serde_json::json!({"file_path": "chunked.txt", "content": "chunk3\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("chunked.txt")).unwrap();
        assert_eq!(content, "chunk1\nchunk2\nchunk3\n");
    }

    #[tokio::test]
    async fn test_write_append_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        tool.invoke(serde_json::json!({"file_path": "sub/dir/file.txt", "content": "deep\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("sub/dir/file.txt")).unwrap();
        assert_eq!(content, "deep\n");
    }
