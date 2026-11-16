//! Tests for glob

use crate::filesystem::glob::GlobFilesTool;
use peri_agent::tools::BaseTool;
use std::path::Path;

/// schema 的 pattern 描述必须说明 `*`/`?` 跨 `/`（与 glob crate 实现一致），
/// 防止模型按 shell 直觉误以为 `*.rs` 只匹配当前目录
#[test]
fn test_glob_pattern_description_declares_cross_slash_semantics() {
    let tool = GlobFilesTool::new("/tmp");
    let params = tool.parameters();
    let desc = params["properties"]["pattern"]["description"]
        .as_str()
        .unwrap();
    assert!(
        desc.contains("match across `/`"),
        "pattern 描述应说明 `*`/`?` 跨 `/`，实际: {desc}"
    );
    assert!(
        !desc.contains("Use ** for recursive matching"),
        "描述不应暗示 `*` 不递归（实现中 `*` 已跨 `/`），实际: {desc}"
    );
}

#[tokio::test]
async fn test_glob_match_simple() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    std::fs::write(dir.path().join("b.rs"), "").unwrap();
    std::fs::write(dir.path().join("c.txt"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("a.rs"), "should find a.rs: {result}");
    assert!(result.contains("b.rs"), "should find b.rs: {result}");
    assert!(!result.contains("c.txt"), "should not find c.txt: {result}");
}

#[tokio::test]
async fn test_glob_no_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.go"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(result, "No files found.");
}

#[tokio::test]
async fn test_glob_recursive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/d.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("d.rs"), "should find nested d.rs: {result}");
}

#[tokio::test]
async fn test_glob_dir_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs", "path": "nonexistent_dir"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Directory not found"),
        "should report missing dir: {err_msg}"
    );
}

#[test]
fn test_description_extended() {
    let tool = GlobFilesTool::new("/tmp");
    let desc = tool.description();
    assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
    assert!(
        desc.contains("modification time"),
        "description 应提及排序规则"
    );
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_Glob() {
    let tool = GlobFilesTool::new("/tmp");
    assert_eq!(tool.name(), "Glob");
}

#[tokio::test]
async fn test_glob_truncation_persists_collected_output() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..1001 {
        std::fs::write(dir.path().join(format!("file_{:04}.rs", i)), "").unwrap();
    }
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Output truncated"),
        "应显示截断信息: {result}"
    );
    // 落盘提示必须用**模型面名字**引导读取（裸名已无提供面）：逐字断言注册表的冻
    // 结字面量，不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        result.contains("use `Read` to view complete content"),
        "应包含模型面名字的读取提示: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "应包含文件路径: {result}"
    );
}

// ─── 软警告 pattern ───────────────────────────────────────────────

#[test]
fn test_soft_warn_pattern_bare_star() {
    // 纯 `*` 应触发软警告（提示用 folder_operations 列目录）
    assert!(
        crate::filesystem::glob::soft_warn_pattern("*").is_some(),
        "纯 `*` 应触发软警告"
    );
    let msg = crate::filesystem::glob::soft_warn_pattern("*").unwrap();
    assert!(
        msg.contains("folder_operations"),
        "警告文案应提及 folder_operations: {msg}"
    );
}

#[test]
fn test_soft_warn_pattern_recursive_all() {
    // `**` 和 `**/*` 全递归都应触发软警告
    assert!(crate::filesystem::glob::soft_warn_pattern("**").is_some());
    assert!(crate::filesystem::glob::soft_warn_pattern("**/*").is_some());
}

#[test]
fn test_soft_warn_pattern_legitimate_patterns_not_warned() {
    // 合法递归 pattern 不应触发软警告
    assert!(
        crate::filesystem::glob::soft_warn_pattern("**/*.rs").is_none(),
        "**/*.rs 不应被警告"
    );
    assert!(crate::filesystem::glob::soft_warn_pattern("*.config.json").is_none());
    assert!(crate::filesystem::glob::soft_warn_pattern("src/**/*.ts").is_none());
    assert!(crate::filesystem::glob::soft_warn_pattern("README.md").is_none());
}

#[test]
fn test_soft_warn_pattern_trims_whitespace() {
    // 前后空白应被 trim
    assert!(crate::filesystem::glob::soft_warn_pattern("  *  ").is_some());
    assert!(crate::filesystem::glob::soft_warn_pattern("\t**/*\n").is_some());
}

// ─── should_skip_dir 扩展 ────────────────────────────────────────

#[test]
fn test_should_skip_dir_worktrees() {
    // worktrees 目录应被跳过（避免扫到 git worktree 完整副本）
    assert!(
        crate::filesystem::should_skip_dir("worktrees"),
        "worktrees 应被跳过"
    );
}

#[test]
fn test_should_not_skip_claude_itself() {
    // `.claude` 目录本身不应被跳过——只跳 worktrees 子目录
    assert!(
        !crate::filesystem::should_skip_dir(".claude"),
        ".claude 本身不应跳过，避免误伤 skills/commands/agents"
    );
}

// ─── invoke 端到端：字节级落盘 ───────────────────────────────────

#[tokio::test]
async fn test_glob_byte_level_persists_when_over_20kb() {
    // 构造 < 1000 条但总字节 > 20KB 的结果，验证字节级落盘触发
    // 每条路径约 60 字节，400 条 ≈ 24KB
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::create_dir_all(base.join("deep/nested/path/to/pad/length")).unwrap();
    for i in 0..400 {
        // 文件名拼长一些，让路径平均 ≥ 50 字节
        let name = format!("file_with_quite_long_name_{:04}.rs", i);
        std::fs::write(base.join("deep/nested/path/to/pad/length").join(&name), "").unwrap();
    }
    let tool = GlobFilesTool::new(base.to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("peri-tool-output-"),
        "字节超限应触发落盘: {result}"
    );
    assert!(
        result.contains("exceeds 20000 byte limit"),
        "应说明字节阈值: {result}"
    );
}

#[tokio::test]
async fn test_glob_soft_warning_prepended_in_output() {
    // `*` pattern 应触发软警告前缀
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.starts_with("Note:"),
        "软警告应以 Note: 前缀开头: {result}"
    );
    assert!(
        result.contains("folder_operations"),
        "警告应提示 folder_operations: {result}"
    );
    // 仍应包含 a.rs（软警告不阻止执行）
    assert!(result.contains("a.rs"), "软警告下仍应返回结果");
}

#[tokio::test]
async fn test_glob_worktree_path_does_not_warn() {
    // 工作流：agent 始终在主项目根启动，通过 `path` 参数进入 worktree 操作。
    // 这种跨边界扫描不应被警告——是 agent 的正常工作模式，警告会变成持续噪音。
    // 防护交给：should_skip_dir（主项目根扫描时跳过 worktree 副本）+ 字节闸 + pattern 软警告。
    let dir = tempfile::tempdir().unwrap();
    let worktree_path = dir.path().join(".claude/worktrees/fake-branch");
    std::fs::create_dir_all(worktree_path.join("src")).unwrap();
    std::fs::write(worktree_path.join("src/a.rs"), "").unwrap();
    std::fs::write(worktree_path.join("src/b.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "src/**/*.rs",
                "path": ".claude/worktrees/fake-branch",
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("显式 path 进 worktree 应正常执行，不报错");
    assert!(
        !result.starts_with("Note:"),
        "agent 进 worktree 工作不应有任何警告前缀: {result}"
    );
    assert!(result.contains("a.rs"), "应找到 src/a.rs: {result}");
    assert!(result.contains("b.rs"), "应找到 src/b.rs: {result}");
}

#[tokio::test]
async fn test_glob_from_project_root_skips_worktree_copy() {
    // 工作流另一半：agent 在主项目根 Glob 时，应跳过 .claude/worktrees 副本。
    // 这是"默认绕过 worktree"的核心保证——靠 should_skip_dir("worktrees") 实现。
    let dir = tempfile::tempdir().unwrap();
    // 主项目的真实源码
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
    // worktree 副本（agent 不应扫到这里）
    let worktree_copy = dir.path().join(".claude/worktrees/feature-x/src");
    std::fs::create_dir_all(&worktree_copy).unwrap();
    std::fs::write(worktree_copy.join("main.rs"), "").unwrap();
    std::fs::write(worktree_copy.join("extra.rs"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "**/*.rs"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Windows 绝对路径使用 \ 分隔符，统一规范化为 / 再断言
    let normalized = result.replace('\\', "/");
    assert!(
        normalized.contains("src/main.rs"),
        "应找到主项目源码: {normalized}"
    );
    // 不应扫到 worktree 副本里的文件
    assert!(
        !normalized.contains("worktrees"),
        "不应扫到 worktree 副本路径: {normalized}"
    );
    assert!(
        !normalized.contains("extra.rs"),
        "不应扫到 worktree 副本里的 extra.rs: {normalized}"
    );
}

#[tokio::test]
async fn test_glob_invalid_pattern_returns_error() {
    let tool = GlobFilesTool::new(".");
    // 不合法的 glob pattern：[ 不闭合
    let input = serde_json::json!({
        "pattern": "[unclosed",
        "path": ".",
    });
    let result = tool
        .invoke(input, peri_agent::tools::ToolContext::new(&[], "."))
        .await;
    assert!(result.is_err(), "语法错误应该返回 Err");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Pattern syntax error"),
        "错误应该提到 Pattern syntax error，实际: {err}"
    );
}

/// 报错文案承诺的 `{foo,bar}` 枚举必须真的可用：glob crate 只把 `{` 当字面量，
/// 未展开时会静默返回 No files found。
#[tokio::test]
async fn test_glob_brace_enumeration_matches_each_alternative() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    std::fs::write(dir.path().join("b.txt"), "").unwrap();
    std::fs::write(dir.path().join("c.md"), "").unwrap();
    let tool = GlobFilesTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "*.{rs,txt}"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("a.rs"), "应命中 .rs 分支: {result}");
    assert!(result.contains("b.txt"), "应命中 .txt 分支: {result}");
    assert!(!result.contains("c.md"), "不应命中未列出分支: {result}");
}

// ─── plan_walk：walk 边界规划（白盒纯函数）──────────────────────

#[test]
fn test_plan_walk_literal_pattern_is_single_level() {
    // 无元字符的纯字面 pattern：只命中精确路径 → 单层扫描即可。
    let plan = crate::filesystem::glob::plan_walk("Cargo.toml");
    assert!(plan.prefix_dirs.is_empty());
    assert_eq!(plan.max_depth, Some(0));
}

#[test]
fn test_plan_walk_literal_pattern_narrows_to_parent_dirs() {
    let plan = crate::filesystem::glob::plan_walk("a/b.rs");
    assert_eq!(plan.prefix_dirs, vec!["a"]);
    assert_eq!(plan.max_depth, Some(1));
    let plan = crate::filesystem::glob::plan_walk("src/main.rs");
    assert_eq!(plan.prefix_dirs, vec!["src"]);
    // 1 个 `/`：`src/main.rs` 在 depth 2，其父目录 `src` 在 depth 1 不会被剪。
    assert_eq!(plan.max_depth, Some(1));
}

#[test]
fn test_plan_walk_metachar_pattern_has_no_depth_limit() {
    // glob::Pattern::matches 默认 require_literal_separator=false：`*`/`?` 可跨 `/`，
    // 含元字符的 pattern 可在任意深度命中 → 深度剪枝不安全，只保留前缀剪枝。
    for pat in [
        "*.rs",
        "**/*.rs",
        "src/**/*.rs",
        "src/*.rs",
        "a/b/*.rs",
        "?x",
    ] {
        let plan = crate::filesystem::glob::plan_walk(pat);
        assert_eq!(plan.max_depth, None, "pattern {pat:?} 不应有深度上限");
    }
}

#[test]
fn test_plan_walk_static_prefix_narrows_walk() {
    let plan = crate::filesystem::glob::plan_walk("src/**/*.rs");
    assert_eq!(plan.prefix_dirs, vec!["src"]);
    let plan = crate::filesystem::glob::plan_walk("src/*.rs");
    assert_eq!(plan.prefix_dirs, vec!["src"]);
    let plan = crate::filesystem::glob::plan_walk("a/b/*.rs");
    assert_eq!(plan.prefix_dirs, vec!["a", "b"]);
}

#[test]
fn test_plan_walk_unsafe_segment_keeps_full_walk() {
    // 元字符在段中 → 前缀不是完整目录链，不能提取
    let plan = crate::filesystem::glob::plan_walk("src*/*.rs");
    assert!(plan.prefix_dirs.is_empty());
    // 前导 `/` → 匹配绝对路径，相对路径永不命中 → 回退全遍历
    let plan = crate::filesystem::glob::plan_walk("/src/*.rs");
    assert!(plan.prefix_dirs.is_empty());
    assert_eq!(plan.max_depth, None);
    // 前缀含字面 `\`（glob 0.3.3 无转义，`\` 是普通字符）→ 目录名无法与原始名字比较，保守回退
    let plan = crate::filesystem::glob::plan_walk(r"\[x\]/*.rs");
    assert!(plan.prefix_dirs.is_empty());
    assert_eq!(plan.max_depth, None);
    // `\[a\]/b` 含字符类 `[a\]`，不是纯字面 → 无深度上限
    let plan = crate::filesystem::glob::plan_walk(r"\[a\]/b");
    assert!(plan.prefix_dirs.is_empty());
    assert_eq!(plan.max_depth, None);
    // 回归：`\` 后紧跟 `*` 时 `*` 仍是跨段元字符；若把 `\*` 当转义会误判为纯字面，
    // max_depth 错误地钳制为单层，子目录中的命中（如 `\sub/x.rs`）被静默剪掉。
    let plan = crate::filesystem::glob::plan_walk(r"\*.rs");
    assert!(plan.prefix_dirs.is_empty());
    assert_eq!(plan.max_depth, None);
    // 无元字符的纯字面 pattern 含 `\`：仍可按 `/` 数钉死深度（整串精确匹配）
    let plan = crate::filesystem::glob::plan_walk(r"a\b");
    assert!(plan.prefix_dirs.is_empty());
    assert_eq!(plan.max_depth, Some(0));
}

// ─── walk root 收窄（narrow_root 白盒纯函数）────────────────────

#[test]
fn test_narrow_root_consumes_static_prefix() {
    // 静态前缀目录链存在且安全时，root 直接收窄到 base+前缀，consumed = 前缀段数。
    let base = Path::new("/repo");
    let (root, consumed) = crate::filesystem::glob::narrow_root(
        base,
        &crate::filesystem::glob::plan_walk("src/**/*.rs"),
    )
    .unwrap();
    assert_eq!(root, Path::new("/repo/src"));
    assert_eq!(consumed, 1);
    let (root, consumed) =
        crate::filesystem::glob::narrow_root(base, &crate::filesystem::glob::plan_walk("a/b/*.rs"))
            .unwrap();
    assert_eq!(root, Path::new("/repo/a/b"));
    assert_eq!(consumed, 2);
    // 纯字面 pattern 的前缀链同样收窄（`src/main.rs` → root=base/src）
    let (root, consumed) = crate::filesystem::glob::narrow_root(
        base,
        &crate::filesystem::glob::plan_walk("src/main.rs"),
    )
    .unwrap();
    assert_eq!(root, Path::new("/repo/src"));
    assert_eq!(consumed, 1);
}

#[test]
fn test_narrow_root_falls_back_to_full_walk() {
    // 无静态前缀：`**`/`*.rs`/字面根层文件/前导 `/` → 不收窄
    for pat in ["**/*.rs", "*.rs", "Cargo.toml", "/src/*.rs"] {
        assert!(
            crate::filesystem::glob::narrow_root(
                Path::new("/repo"),
                &crate::filesystem::glob::plan_walk(pat)
            )
            .is_none(),
            "pattern {pat:?} 不应收窄"
        );
    }
    // 前缀链中段是被跳目录：收窄会让被跳目录子孙变为可遍历，行为反转
    assert!(crate::filesystem::glob::narrow_root(
        Path::new("/repo"),
        &crate::filesystem::glob::plan_walk("node_modules/pkg/*.rs")
    )
    .is_none());
    // 任一段含 `.`/`..`：拼接会改变遍历范围，保守回退
    assert!(crate::filesystem::glob::narrow_root(
        Path::new("/repo"),
        &crate::filesystem::glob::plan_walk("./src/*.rs")
    )
    .is_none());
    assert!(crate::filesystem::glob::narrow_root(
        Path::new("/repo"),
        &crate::filesystem::glob::plan_walk("../src/*.rs")
    )
    .is_none());
    assert!(crate::filesystem::glob::narrow_root(
        Path::new("/repo"),
        &crate::filesystem::glob::plan_walk("a/../*.rs")
    )
    .is_none());
}

include!("glob_more_test.rs");
