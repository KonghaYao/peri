use super::*;

// ─── Grep 防护移植（glob.rs 蓝本：协作取消 / 线程 cap / 预算）───────────────

/// 防护常量存在性守护（线程 cap、超时、字节/行预算的取值）
#[test]
fn test_grep_guard_constants() {
    assert_eq!(SEARCH_THREADS_MAX, 8, "walker 线程上限应为 8");
    assert_eq!(SEARCH_TIMEOUT, Duration::from_secs(15), "超时应为 15s");
    assert_eq!(MAX_OUTPUT_BYTES, 20_000, "字节预算应与 glob 对齐");
    assert_eq!(MAX_LINE_BYTES, 1_000, "单行预算应为 1000 字节");
}

/// 协作取消：cancelled 预置时 walker 在第一个检查点退出，不产生任何结果
/// （invoke 超时路径无法确定性触发，此为超时置位语义的可测替代）
#[test]
fn test_grep_cancelled_skips_walk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle here").unwrap();
    let parsed = ParsedArgs {
        pattern: "needle".to_string(),
        path: Some("./".to_string()),
        glob_filters: vec![],
        _type_filters: vec![],
        _type_excludes: vec![],
        output_mode: OutputMode::Default,
        before_context: 0,
        after_context: 0,
        case_insensitive: false,
        whole_word: false,
        multiline: false,
        line_number: true,
        invert_match: false,
        fixed_strings: false,
        max_depth: None,
    };
    let cancelled = Arc::new(AtomicBool::new(true));
    let result = execute_search(&parsed, dir.path().to_str().unwrap(), 250, cancelled).unwrap();
    assert!(
        result.contains("No matches found"),
        "cancelled 预置应跳过整个遍历: {result}"
    );
}

/// 精确-fit 判定：恰好 head_limit 行匹配时不得误标 truncated
#[tokio::test]
async fn test_grep_exact_fit_no_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..5).map(|i| format!("line {i} needle")).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "head_limit": 5
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        !result.contains("truncated"),
        "恰好 5 行匹配不应标 truncated: {result}"
    );
    assert_eq!(result.lines().count(), 5, "应输出完整 5 行: {result}");
}

/// 精确-fit 判定：超过 head_limit 时截断到恰 N 行 + truncated 标记
#[tokio::test]
async fn test_grep_over_limit_truncates_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..6).map(|i| format!("line {i} needle")).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "head_limit": 5
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("truncated at 5 lines"),
        "超过 5 行应标 truncated: {result}"
    );
    let content: Vec<&str> = result
        .split("... (truncated")
        .next()
        .unwrap()
        .lines()
        .collect();
    assert_eq!(content.len(), 5, "内容部分应恰 5 行: {result}");
}

/// P0-3 修复：head_limit=0（unlimited）时 context 行不得提前置 stopped——
/// 10 匹配 + 后上下文必须全部输出
#[tokio::test]
async fn test_grep_unlimited_head_limit_with_context() {
    let dir = tempfile::tempdir().unwrap();
    let mut content = String::new();
    for i in 0..10 {
        content.push_str(&format!("needle {i}\n"));
        content.push_str(&format!("filler {i}\n"));
    }
    std::fs::write(dir.path().join("test.txt"), content).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "head_limit": 0,
                "-A": 1
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        !result.contains("truncated"),
        "head_limit=0 时不得截断: {result}"
    );
    for i in 0..10 {
        assert!(
            result.contains(&format!("needle {i}")),
            "匹配 {i} 应全部输出: {result}"
        );
        assert!(
            result.contains(&format!("filler {i}")),
            "后上下文 filler {i} 应全部输出: {result}"
        );
    }
}

/// 字节预算：head_limit=0 时输出仍受 MAX_OUTPUT_BYTES 保护——截断提示 + 落盘
#[tokio::test]
async fn test_grep_byte_budget_truncates() {
    let dir = tempfile::tempdir().unwrap();
    // 200 行 × ~300 字节 ≈ 60KB > 20KB 预算
    let line = format!("needle {}", "x".repeat(300));
    let lines: Vec<String> = (0..200).map(|_| line.clone()).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "head_limit": 0
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("[Output truncated"),
        "字节超限应有截断提示: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "完整输出应落盘: {result}"
    );
    // 内联内容部分 ≤ 字节预算 + 提示余量
    let content = result.split("[Output truncated").next().unwrap();
    assert!(
        content.len() < MAX_OUTPUT_BYTES + 512,
        "内联内容应受限（实际 {} 字节）: {result}",
        content.len()
    );
}

/// 单行 trim：超长行按 MAX_LINE_BYTES 截断 + 可见标记，输出行受限
#[tokio::test]
async fn test_grep_long_line_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let long = format!("needle {}", "x".repeat(200_000));
    std::fs::write(dir.path().join("test.txt"), long).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "head_limit": 5
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("[line truncated]"),
        "超长行应有截断标记: {result}"
    );
    let content_line = result
        .lines()
        .find(|l| l.contains("[line truncated]"))
        .unwrap();
    assert!(
        content_line.len() <= MAX_LINE_BYTES + 64,
        "截断后输出行应受限（实际 {} 字节）",
        content_line.len()
    );
}

/// 单行 trim：短行不被截断
#[tokio::test]
async fn test_grep_short_line_not_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle short line").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        !result.contains("[line truncated]"),
        "短行不应被截断: {result}"
    );
    assert!(
        result.contains("needle short line"),
        "短行应完整输出: {result}"
    );
}

/// P1-3：head_limit 在文件模式 = 输出文件数上限（此前完全失效）
#[tokio::test]
async fn test_grep_files_mode_head_limit() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        std::fs::write(dir.path().join(format!("f{i:02}.txt")), "needle").unwrap();
    }
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "files_with_matches",
                "path": "./",
                "head_limit": 3
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let content: Vec<&str> = result
        .split("... (truncated")
        .next()
        .unwrap()
        .lines()
        .collect();
    assert!(!content.is_empty(), "应输出匹配文件: {result}");
    assert!(
        content.len() <= 3,
        "files 模式输出行数应受 head_limit 限制: {result}"
    );
    for line in &content {
        assert!(line.ends_with(".txt"), "应输出文件路径而非内容: {line}");
    }
}

/// P1-3b：CountOnly 数完当前文件（忽略 stopped），计数不被其他线程的预算
/// 置位截断为错误的低计数
#[tokio::test]
async fn test_grep_count_not_corrupted_by_stop() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        let lines = ["needle"; 5].join("\n");
        std::fs::write(dir.path().join(format!("f{i:02}.txt")), lines).unwrap();
    }
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "count",
                "path": "./",
                "head_limit": 2
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let content: Vec<&str> = result
        .split("... (truncated")
        .next()
        .unwrap()
        .lines()
        .collect();
    assert!(!content.is_empty(), "应输出计数: {result}");
    assert!(
        content.len() <= 2,
        "count 模式输出行数应受 head_limit 限制: {result}"
    );
    for line in &content {
        assert!(
            line.ends_with(":5"),
            "计数必须完整（每文件 5 个匹配），不得被截断: {line}"
        );
    }
}

/// 输出确定性：并行遍历下按路径字典序稳定排序，两次搜索逐字节相等
#[tokio::test]
async fn test_grep_deterministic_order() {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("z.txt", "needle z"),
        ("a.txt", "needle a"),
        ("m.txt", "needle m"),
        ("b.txt", "needle b"),
    ] {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let run = || async {
        tool.invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "show_line_numbers": false
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap()
    };
    let r1 = run().await;
    let r2 = run().await;
    assert_eq!(r1, r2, "同一搜索两次运行应逐字节相等");
    let lines: Vec<&str> = r1.lines().collect();
    assert_eq!(lines.len(), 4, "应输出全部 4 个文件: {r1}");
    assert!(lines[0].contains("a.txt"), "应按路径字典序: {r1}");
    assert!(lines[1].contains("b.txt"), "应按路径字典序: {r1}");
    assert!(lines[2].contains("m.txt"), "应按路径字典序: {r1}");
    assert!(lines[3].contains("z.txt"), "应按路径字典序: {r1}");
}

/// 浮点 head_limit/offset 必须显式报错，不得被 as_u64() 静默吞掉回退默认值
#[tokio::test]
async fn test_grep_fractional_numeric_params_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "needle").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    for (key, value) in [
        ("head_limit", serde_json::json!(12.5)),
        ("offset", serde_json::json!(3.5)),
        ("max_depth", serde_json::json!(2.5)),
        ("context", serde_json::json!(1.5)),
        ("before_context", serde_json::json!(0.5)),
        ("after_context", serde_json::json!(0.5)),
    ] {
        let result = tool
            .invoke(
                serde_json::json!({"pattern": "needle", key: value}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("non-negative integer"),
            "浮点 {key}={value} 应报错而非静默回退: {err_msg}"
        );
    }
}
