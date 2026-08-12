//! Tests for read

use super::*;

#[tokio::test]
async fn test_read_file_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "hello\nworld").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "file.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
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
async fn test_read_empty_file_returns_explicit_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.txt");
    std::fs::write(&path, "").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "empty.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(
        result.contains("[EMPTY FILE]"),
        "应明确标记空文件: {result}"
    );
    assert!(result.contains("0 bytes"), "应明确报告文件大小: {result}");
    assert!(
        !result.contains("     1\t"),
        "空文件不应伪装成一个空白行: {result}"
    );
}

#[tokio::test]
async fn test_read_file_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "nonexistent.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("File not found"),
        "should report not found: {err_msg}"
    );
}

#[tokio::test]
async fn test_read_long_line_reports_line_truncation_before_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long-line.txt");
    std::fs::write(&path, "x".repeat(MAX_CHARS_PER_LINE + 1)).unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "long-line.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(
        result.contains("[LINE TRUNCATED:")
            && result.contains(&(MAX_CHARS_PER_LINE + 1).to_string())
            && result.contains(&MAX_CHARS_PER_LINE.to_string()),
        "长行应在可见前缀中报告原始和保留字符数: {result}"
    );
}

#[tokio::test]
async fn test_read_large_output_reports_truncation_and_persisted_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large-output.txt");
    std::fs::write(&path, "line content\n".repeat(1000)).unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "large-output.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(
        result.contains("[Output truncated:") && result.contains("bytes total"),
        "总输出截断应报告原始字节数: {result}"
    );
    assert!(
        result.contains("[Full output saved to"),
        "总输出截断应保留完整输出路径提示: {result}"
    );
}

#[tokio::test]
async fn test_read_file_offset_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "L1\nL2\nL3\nL4\nL5").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "lines.txt", "offset": 2, "limit": 2}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // offset 是 1-based 行号：offset=2 → 从第 2 行 L2 开始，limit=2 → L2 和 L3
    assert!(result.contains("2\tL2"), "should contain line 2: {result}");
    assert!(result.contains("3\tL3"), "should contain line 3: {result}");
    assert!(!result.contains("L1"), "should not contain L1");
    assert!(!result.contains("L4"), "should not contain L4");
    assert!(!result.contains("L5"), "should not contain L5");
}

#[tokio::test]
async fn test_read_file_offset_one_equals_full_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "L1\nL2\nL3").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let full = tool
        .invoke(
            serde_json::json!({"file_path": "lines.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let from_one = tool
        .invoke(
            serde_json::json!({"file_path": "lines.txt", "offset": 1}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(full, from_one, "offset=1 应与缺省读取结果一致");
}

#[tokio::test]
async fn test_read_file_offset_last_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "L1\nL2\nL3\nL4\nL5").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "lines.txt", "offset": 5}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("5\tL5"), "应能读取最后一行: {result}");
    assert!(!result.contains("L4"), "不应包含前一行: {result}");
}

#[tokio::test]
async fn test_read_file_offset_fraction_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    for bad in [serde_json::json!(1.5), serde_json::json!(139.0000000001)] {
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "f.txt", "offset": bad}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("positive integer"),
            "非整数 offset 应报错而非静默回退: {err_msg}"
        );
    }
}

#[tokio::test]
async fn test_read_file_offset_zero_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt", "offset": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("positive integer"),
        "offset=0 在 1-based 语义下应报错: {err_msg}"
    );
}

#[tokio::test]
async fn test_read_file_limit_zero_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb").unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt", "limit": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("positive integer"),
        "limit=0 应报错而非静默输出空: {err_msg}"
    );
}

#[tokio::test]
async fn test_read_file_binary_extension() {
    let dir = tempfile::tempdir().unwrap();
    // Binary extension check happens before file read, no need to create the file
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "image.png"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
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
        .invoke(
            serde_json::json!({"file_path": path.to_str().unwrap()}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
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
        .invoke(
            serde_json::json!({"file_path": "short.txt", "offset": 999}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("exceeds file length"),
        "offset 超出文件长度应返回错误而非 panic: {err_msg}"
    );
    assert!(
        err_msg.contains("Valid offsets are 1..=2"),
        "越界错误应返回实际有效范围，避免继续猜测: {err_msg}"
    );
    assert!(
        err_msg.contains("omit offset") && err_msg.contains("Do not guess"),
        "越界错误应给出确定恢复动作并禁止继续猜 offset: {err_msg}"
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
        .invoke(
            serde_json::json!({"file_path": "huge.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("File too large"),
        "超大文件应返回 File too large 错误: {err_msg}"
    );
    assert!(
        err_msg.contains("offset/limit cannot bypass"),
        "不应误导 Agent 用大 offset 绕过文件大小限制: {err_msg}"
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
        desc.contains("Never guess or estimate an offset")
            && desc.contains("last line number actually shown plus 1"),
        "description 应禁止猜 offset，并要求只按实际可见行号续读"
    );
    assert!(
        !desc.contains("especially handy for long files"),
        "description 不应继续把 offset 宣传成长文件探测手段"
    );
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

#[test]
fn test_offset_schema_forbids_guessed_large_values() {
    let tool = ReadFileTool::new("/tmp");
    let params = tool.parameters();
    let desc = params["properties"]["offset"]["description"]
        .as_str()
        .unwrap();
    assert!(
        desc.contains("OMIT by default"),
        "应默认省略 offset: {desc}"
    );
    assert!(
        desc.contains("NEVER guess or estimate") && desc.contains("already observed"),
        "schema 应只允许使用已有证据的行号，禁止猜大 offset: {desc}"
    );
    assert!(
        desc.contains("last line actually shown plus 1"),
        "schema 应基于实际显示的最后行续读: {desc}"
    );
}

#[tokio::test]
async fn test_pdf_with_pages_returns_placeholder() {
    let tool = ReadFileTool::new("/tmp");
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "test.pdf", "pages": "1-5"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
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
        .invoke(
            serde_json::json!({"file_path": "test.pdf"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("BINARY FILE DETECTED"),
        "should return binary: {result}"
    );
}

#[tokio::test]
async fn test_read_directory_returns_listing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": dir.path().to_str().unwrap()}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // 应返回 Ok 而非 Err
    assert!(
        result.contains("DIRECTORY DETECTED"),
        "should contain directory hint: {result}"
    );
    assert!(
        result.contains("converted it to a directory listing")
            && result.contains("folder_operations")
            && result.contains("operation=\"list\""),
        "目录转换应显式说明转换行为及专用工具: {result}"
    );
    assert!(result.contains("a.txt"), "should list a.txt: {result}");
    assert!(result.contains("subdir"), "should list subdir: {result}");
}

#[tokio::test]
async fn test_read_directory_returns_listing_for_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("readme.md"), "docs").unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let tool = ReadFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "."}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("DIRECTORY DETECTED"),
        "should detect directory for relative path: {result}"
    );
    assert!(
        result.contains("readme.md") || result.contains("src"),
        "should list contents: {result}"
    );
}
