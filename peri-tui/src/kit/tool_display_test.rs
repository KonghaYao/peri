//! Tests for tool_display
#[cfg(test)]
use super::*;
use peri_acp_types::builtin_mcp::{find, original_tool_name_of_effective};

#[test]
fn test_bash_maps_to_shell() {
    assert_eq!(format_tool_name("Bash"), "Shell");
}

#[test]
fn test_folder_operations_maps_to_folder() {
    assert_eq!(format_tool_name("folder_operations"), "Folder");
}

#[test]
fn test_unknown_passthrough() {
    // 大部分工具名保留原样（不再映射为别名）
    assert_eq!(format_tool_name("WebSearch"), "WebSearch");
    assert_eq!(format_tool_name("WebFetch"), "WebFetch");
    assert_eq!(format_tool_name("TodoWrite"), "TodoWrite");
    assert_eq!(format_tool_name("AskUserQuestion"), "AskUserQuestion");
    assert_eq!(format_tool_name("AgentResult"), "AgentResult");
    assert_eq!(format_tool_name("artifact"), "artifact");
    assert_eq!(format_tool_name("CustomTool"), "CustomTool");
}

#[test]
fn test_format_tool_args_bash_extracts_command() {
    let args = serde_json::json!({"command": "cargo build -p peri-tui"});
    assert_eq!(format_tool_args("Bash", &args), "cargo build -p peri-tui");
}

#[test]
fn test_format_tool_args_read_extracts_file_path() {
    let args = serde_json::json!({"file_path": "src/main.rs"});
    assert_eq!(format_tool_args("Read", &args), "src/main.rs");
}

#[test]
fn test_format_tool_args_glob_truncates_pattern() {
    let long = "a".repeat(250);
    let args = serde_json::json!({"pattern": &long});
    let result = format_tool_args("Glob", &args);
    assert!(result.len() <= 203, "应为 200 字符 + '...'");
    assert!(result.ends_with("..."));
}

#[test]
fn test_format_tool_args_websearch_truncates_query() {
    let long = "q".repeat(80);
    let args = serde_json::json!({"query": &long});
    let result = format_tool_args("WebSearch", &args);
    assert!(result.len() <= 63);
}

#[test]
fn test_format_tool_args_unknown_returns_empty() {
    let args = serde_json::json!({"x": "y"});
    assert_eq!(format_tool_args("UnknownTool", &args), "");
}

#[test]
fn test_format_tool_args_folder_operations() {
    let args = serde_json::json!({"operation": "list", "folder_path": "/tmp"});
    assert_eq!(format_tool_args("folder_operations", &args), "list /tmp");
}

#[test]
fn test_format_websearch_query_truncated() {
    // WebSearch query 超过 60 字符时应截断
    let long = "q".repeat(80);
    let args = serde_json::json!({"query": &long});
    let result = format_tool_args("WebSearch", &args);
    assert!(result.len() <= 63, "应为 60 字符 + '...'");
    assert!(result.ends_with("..."));
}

#[test]
fn test_format_webfetch_url_not_truncated() {
    // WebFetch url 不截断，返回原始字符串
    let long_url =
        "https://example.com/very/long/path/that/exceeds/sixty/characters/total/here.txt";
    assert!(long_url.chars().count() > 60, "测试用 url 长度应 > 60");
    let args = serde_json::json!({"url": &long_url});
    let result = format_tool_args("WebFetch", &args);
    assert_eq!(result, long_url, "WebFetch url 不应被截断");
}

// ── A4 / A8 匹配型归一：builtin 一等工具的 effective name 走同一分支 ────────────

#[test]
fn tui_web_tools_still_summarize_after_migration() {
    // effective name（模型面名字）与迁移前的裸名必须产出**同一**参数摘要：
    // 归一走 IF-D15 的 original_tool_name_of_effective，本模块不得硬编码名字字面量。
    let long_query = "q".repeat(80);
    let args = serde_json::json!({ "query": long_query });
    let effective = format_tool_args("mcp__web__WebSearch", &args);
    assert_eq!(effective, format_tool_args("WebSearch", &args));
    assert!(effective.len() <= 63, "仍按 60 字符截断: {effective:?}");

    let url = "https://example.com/very/long/path/that/exceeds/sixty/characters/total/here.txt";
    let args = serde_json::json!({ "url": url });
    assert_eq!(format_tool_args("mcp__web__WebFetch", &args), url);

    // artifact 走 file_path 专用分支（未命中会返回空串，对比 test_format_tool_args_unknown_returns_empty）
    let args = serde_json::json!({ "file_path": "peri-tui/src/lib.rs" });
    assert_eq!(
        format_tool_args("mcp__artifact__artifact", &args),
        "peri-tui/src/lib.rs"
    );
}

// ── wave 2（cron / lsp）注册表新增：新 effective name 必须复用**既有**显示分支 ──────

/// wave 2 新增的 builtin 实例在注册表中的**实例键**（不是 effective name 副本）。
const WAVE2_INSTANCE_KEYS: [&str; 2] = ["cron", "lsp"];

/// 按原始工具名给出代表性参数：键名取自 `peri-middlewares` 各自 `BaseTool::parameters()`。
/// 未登记的名字直接 panic —— 注册表条目变动时必须回来复核本回归。
fn wave2_args_by_original_name(original_name: &str) -> serde_json::Value {
    match original_name {
        "cron_register" => {
            serde_json::json!({ "expression": "*/5 * * * *", "prompt": "check status" })
        }
        "cron_list" => serde_json::json!({}),
        "cron_remove" => serde_json::json!({ "id": "task-1" }),
        "LSP" => serde_json::json!({ "operation": "documentSymbol" }),
        other => panic!("wave 2 有未登记的工具，需复核本回归: {other}"),
    }
}

#[test]
fn cron_lsp_effective_names_reuse_existing_display() {
    // S-03：本波只在 peri-acp-types 的声明表新增 cron / lsp 条目，**不新增 TUI 归一
    // 入口**。4 个新 effective name 必须经既有的「原样优先 → IF-D15 归一后重试」
    // 路径（见 format_tool_args）落到既有按名分支；等价值用可观察输出（返回值）比较。
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for key in WAVE2_INSTANCE_KEYS {
        let instance = find(key).unwrap_or_else(|| panic!("wave 2 实例必须在注册表中: {key}"));
        for tool in instance.tools {
            pairs.push((tool.effective_name, tool.original_name));
        }
    }
    // 从注册表取数（不在此复制 effective name 字面量）：cron 三项 + lsp 一项。
    assert_eq!(pairs.len(), 4, "wave 2 新增条目数: {pairs:?}");

    for (effective, original) in pairs {
        // 归一入口的前置条件：新 effective name 必须能被 IF-D15 反查命中；命中不了
        // ⇒ 重试分支拿不到原始名，归一入口对新名字失效。
        assert_eq!(
            original_tool_name_of_effective(effective),
            Some(original),
            "IF-D15 必须命中 wave 2 新条目: {effective}"
        );
        let args = wave2_args_by_original_name(original);
        assert_eq!(
            format_tool_args(effective, &args),
            format_tool_args(original, &args),
            "effective name 必须复用 {original} 的既有显示分支: {effective}"
        );
        // 注：cron 三项在 format_tool_args_by_name 里**没有**专有分支（裸名与 effective
        // name 都返回空串），故其等价断言当前只覆盖「未被误路由到别的分支」；待 cron
        // 获得专有分支后自动转为判别性断言。
    }

    // 判别点：LSP 是 4 个新条目中唯一命中**专有**显示分支的（operation 截断 40）。
    // 归一入口失效 ⇒ 退化为未知名分支的空串（见 tui_unknown_mcp_names_fall_back_to_generic），
    // 下面两条断言随之变红。
    let lsp_effective = find("lsp")
        .expect("lsp 实例必须在注册表中")
        .tools
        .iter()
        .find(|tool| tool.original_name == "LSP")
        .expect("lsp 实例必须声明 LSP 工具")
        .effective_name;
    let lsp_args = wave2_args_by_original_name("LSP");
    let lsp = format_tool_args(lsp_effective, &lsp_args);
    assert_eq!(lsp, "documentSymbol", "LSP 专有分支未被复用: {lsp:?}");
    assert_ne!(
        lsp,
        format_tool_args("mcp__unknown__LSP", &lsp_args),
        "专有分支必须与未知名分支在输出上可区分"
    );
}

#[test]
fn tui_unknown_mcp_names_fall_back_to_generic() {
    // 反证：未知 / 外部 `mcp__*` 两次都不命中 ⇒ 与迁移前逐位一致（无摘要）。
    let args = serde_json::json!({ "file_path": "peri-tui/src/lib.rs" });
    assert_eq!(format_tool_args("mcp__foo__bar", &args), "");
    // 归一表是冻结字面量的精确匹配：不做前缀、大小写或 sanitize 反拆
    assert_eq!(format_tool_args("mcp__web__WebSearchExtra", &args), "");
    assert_eq!(format_tool_args("mcp__web__websearch", &args), "");
}

#[test]
fn tui_has_no_hardcoded_effective_name() {
    // A4 / A8：名字字面量只在 peri-acp-types 的 builtin 声明表存一份——按名分支
    // 必须经 IF-D15 归一 helper 进入 builtin 名字空间，不得自建第二张反查表。
    let src = include_str!("tool_display.rs");
    assert!(
        src.contains("original_tool_name_of_effective"),
        "tool_display.rs 必须经 IF-D15 归一 helper"
    );
    for forbidden in ["mcp__web__", "mcp__artifact__"] {
        assert!(
            !src.contains(forbidden),
            "tool_display.rs 不得硬编码 effective name: {forbidden}"
        );
    }
}

// ── wave 3（workspace）注册表新增：display 名必须复用**既有**分支 ──────────────

/// workspace 的 effective name（模型面名字）与迁移前的裸名映射到**同一**显示名；
/// 未命中归一表的名字回落传入名原样（保守语义，与迁移前逐位一致）。
#[test]
fn workspace_effective_names_map_to_short_names() {
    // 判别点：归一入口失效 ⇒ 下面两条退化为 effective name 原样（断言变红）。
    assert_eq!(format_tool_name("mcp__workspace__Bash"), "Shell");
    assert_eq!(
        format_tool_name("mcp__workspace__folder_operations"),
        "Folder"
    );
    // 等价值：effective name 与裸名同结果（不新增 effective name 专用分支）。
    assert_eq!(
        format_tool_name("mcp__workspace__Bash"),
        format_tool_name("Bash")
    );
    assert_eq!(
        format_tool_name("mcp__workspace__folder_operations"),
        format_tool_name("folder_operations")
    );
    // 归一后仍无显示名分支的名字：必须回落**原样**（不是空串、也不是别名）。
    assert_eq!(
        format_tool_name("mcp__workspace__Read"),
        "mcp__workspace__Read"
    );
    assert_eq!(
        format_tool_name("mcp__workspace__Edit"),
        "mcp__workspace__Edit"
    );
    // 未命中归一表：未知 / 外部 `mcp__*`、前缀相似、大小写不匹配一律原样。
    assert_eq!(format_tool_name("mcp__foo__Bar"), "mcp__foo__Bar");
    assert_eq!(
        format_tool_name("mcp__workspace__BashExtra"),
        "mcp__workspace__BashExtra"
    );
    assert_eq!(
        format_tool_name("mcp__workspace__bash"),
        "mcp__workspace__bash"
    );
}
