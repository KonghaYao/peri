//! Grep 搜索语义：四输出模式、上下文、过滤、截断、排序与错误（FC-GREP-01..04）。
//!
//! 全部使用合成临时 fixture；不访问仓库真实文件，不执行外部命令。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use local_mcp_server::error::CapabilityError;
use local_mcp_server::tasks::log::DirOutputPersist;
use local_mcp_server::tools::grep::{self, GrepContext};
use local_mcp_server::wire::ToolResponse;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// 合成工作区。
struct Fixture {
    dir: TempDir,
    persist_dir: TempDir,
}

impl Fixture {
    /// 基础 fixture：普通文件、嵌套目录、隐藏文件、gitignore、.ignore、二进制、超长行。
    fn basic() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).expect("git dir");
        std::fs::create_dir_all(root.join("nested/deeper")).expect("nested");
        write(root.join("a.txt"), "alpha\nbeta\nALPHA\n");
        write(
            root.join("nested/b.rs"),
            "fn main() {\n    let alpha = 1;\n}\n",
        );
        write(root.join("nested/deeper/c.md"), "alpha deep\n");
        write(root.join(".hidden.txt"), "alpha hidden\n");
        write(root.join(".gitignore"), "ignored.txt\n");
        write(root.join("ignored.txt"), "alpha gitignored\n");
        write(root.join(".ignore"), "also-ignored.txt\n");
        write(root.join("also-ignored.txt"), "alpha ignore-file\n");
        std::fs::write(root.join("binary.dat"), b"alpha\x00binary\n").expect("binary");
        let long_line = format!("{}alpha{}\n", "x".repeat(1_200), "y".repeat(1_200));
        write(root.join("long.txt"), &long_line);
        write(root.join("no-match.txt"), "nothing here\n");
        Self {
            dir,
            persist_dir: TempDir::new().expect("persist tempdir"),
        }
    }

    /// 大目录 fixture：用于超时与字节预算路径。
    fn bulk(files: usize, lines_per_file: usize) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        for index in 0..files {
            let mut content = String::new();
            for line in 0..lines_per_file {
                content.push_str(&format!("alpha {index} {line}\n"));
            }
            write(root.join(format!("f{index:04}.txt")), &content);
        }
        Self {
            dir,
            persist_dir: TempDir::new().expect("persist tempdir"),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// 追加一个文件到工作区根（用于构造针对性反例）。
    fn write_file(&self, relative: &str, content: &str) {
        write(self.root().join(relative), content);
    }

    fn persist(&self) -> DirOutputPersist {
        DirOutputPersist::new(self.persist_dir.path())
    }

    /// 解析器：只允许工作区内的路径（测试替身，生产由 capability 提供）。
    fn resolver(&self) -> impl Fn(&str) -> Result<PathBuf, CapabilityError> + '_ {
        let root = self.root().to_path_buf();
        move |requested: &str| {
            let candidate = PathBuf::from(requested);
            let joined = if candidate.is_absolute() {
                candidate
            } else {
                root.join(candidate)
            };
            let normalized = normalize(&joined);
            if normalized.starts_with(&root) {
                Ok(normalized)
            } else {
                Err(CapabilityError::OutsideRoot {
                    requested: requested.to_string(),
                    root: root.clone(),
                })
            }
        }
    }

    async fn run(&self, arguments: serde_json::Value) -> ToolResponse {
        self.run_with_timeout(arguments, None).await
    }

    async fn run_with_timeout(
        &self,
        arguments: serde_json::Value,
        timeout: Option<std::time::Duration>,
    ) -> ToolResponse {
        let resolver = self.resolver();
        let persist = self.persist();
        let persist: Arc<dyn local_mcp_server::tasks::log::OutputPersist> = Arc::new(persist);
        let mut context = GrepContext::new(
            self.root().to_path_buf(),
            root_real(self.root()),
            &resolver,
            persist,
        );
        if let Some(timeout) = timeout {
            context = context.with_timeout(timeout);
        }
        grep::invoke(&arguments, &context, None).await
    }

    async fn text(&self, arguments: serde_json::Value) -> String {
        self.run(arguments).await.text
    }
}

fn write(path: PathBuf, content: &str) {
    std::fs::write(path, content).expect("write fixture file");
}

/// **启动时锚定**的授权根真实路径（GAP-034 的判定基准）。
///
/// 生产由 `RootDir::real_base()` 在 open 时算出；测试在夹具构造后立刻算一次，
/// 之后即使目录被替换也不会重算——这正是"基准不得每请求重算"的形态。
fn root_real(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).expect("授权根真实路径")
}

/// 词法归一（测试替身用；真实 capability 还会做 symlink 校验）。
fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    for part in parts {
        result.push(part);
    }
    result
}

fn lines_of(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

#[tokio::test]
async fn content_mode_defaults_and_sorted_order() {
    let fixture = Fixture::basic();
    let response = fixture.run(json!({ "pattern": "alpha" })).await;
    assert!(!response.is_error, "应成功: {}", response.text);
    let lines = lines_of(&response.text);
    assert_eq!(
        lines.len(),
        4,
        "跨文件按 display_path 字典序且只命中 4 个文件"
    );
    assert_eq!(lines[0], "a.txt:1: alpha");
    assert!(
        lines[1].starts_with("long.txt:1: x") && lines[1].ends_with("… [line truncated]"),
        "超长行按 1000 字节截断并加标记: {}",
        &lines[1][..lines[1].len().min(40)]
    );
    assert_eq!(lines[2], "nested/b.rs:2:     let alpha = 1;");
    assert_eq!(lines[3], "nested/deeper/c.md:1: alpha deep");
    assert!(!response.text.contains("ALPHA"), "默认大小写敏感");
    assert!(
        !response.text.contains("hidden"),
        "hidden(true) 跳过隐藏文件"
    );
    assert!(!response.text.contains("gitignored"), "git_ignore 生效");
    assert!(!response.text.contains("ignore-file"), ".ignore 生效");
    assert_eq!(response.structured["tool"], json!("Grep"));
    assert_eq!(response.structured["ok"], json!(true));
}

#[tokio::test]
async fn long_line_is_trimmed_with_visible_marker() {
    let fixture = Fixture::basic();
    let text = fixture
        .text(json!({ "pattern": "alpha", "glob": "long.txt" }))
        .await;
    assert!(text.contains("… [line truncated]"), "应带截断标记: {text}");
    let line = text.split('\n').next().unwrap();
    // 1000 字节单行上限 + 标记 + 行号前缀。
    assert!(
        line.len() < 1_100,
        "单行应被压到 1000 字节附近: {}",
        line.len()
    );
}

#[tokio::test]
async fn case_insensitive_and_line_number_toggle() {
    let fixture = Fixture::basic();
    let text = fixture
        .text(json!({ "pattern": "alpha", "case_insensitive": true, "glob": "a.txt" }))
        .await;
    assert!(
        text.contains("a.txt:3: ALPHA"),
        "大小写不敏感应命中: {text}"
    );

    let text = fixture
        .text(json!({ "pattern": "alpha", "show_line_numbers": false, "glob": "a.txt" }))
        .await;
    assert_eq!(text, "a.txt: alpha");

    let text = fixture
        .text(json!({ "pattern": "alpha", "-n": false, "glob": "a.txt" }))
        .await;
    assert_eq!(text, "a.txt: alpha", "CLI 别名 -n 生效");
}

#[tokio::test]
async fn context_markers_and_asymmetric_windows() {
    let fixture = Fixture::basic();
    let text = fixture
        .text(json!({ "pattern": "alpha", "context": 1, "glob": "a.txt" }))
        .await;
    // 行号格式 `{path}:{line}{sep}: {content}`（源 grep_format.rs 逐字）。
    assert_eq!(
        lines_of(&text),
        vec!["a.txt:1: alpha", "a.txt:2+: beta"],
        "匹配后上下文用 `+` 标记；ALPHA 大小写不匹配"
    );

    let text = fixture
        .text(json!({ "pattern": "beta", "context": 1, "glob": "a.txt" }))
        .await;
    assert_eq!(
        lines_of(&text),
        vec!["a.txt:1-: alpha", "a.txt:2: beta", "a.txt:3+: ALPHA"],
        "匹配前上下文用 `-` 标记"
    );

    let text = fixture
        .text(json!({ "pattern": "beta", "before_context": 1, "glob": "a.txt" }))
        .await;
    assert_eq!(
        lines_of(&text),
        vec!["a.txt:1-: alpha", "a.txt:2: beta"],
        "只给 before 时没有 after"
    );

    // 无行号时上下文标记仍在。
    let text = fixture
        .text(
            json!({ "pattern": "beta", "context": 1, "show_line_numbers": false, "glob": "a.txt" }),
        )
        .await;
    assert_eq!(
        lines_of(&text),
        vec!["a.txt-: alpha", "a.txt: beta", "a.txt+: ALPHA"]
    );
}

#[tokio::test]
async fn output_modes_files_count_and_without_matches() {
    let fixture = Fixture::basic();
    let files = fixture
        .text(json!({ "pattern": "alpha", "output_mode": "files_with_matches" }))
        .await;
    assert_eq!(
        lines_of(&files),
        vec!["a.txt", "long.txt", "nested/b.rs", "nested/deeper/c.md"]
    );

    let counts = fixture
        .text(json!({ "pattern": "alpha", "output_mode": "count", "glob": "*.txt" }))
        .await;
    assert_eq!(lines_of(&counts), vec!["a.txt:1", "long.txt:1"]);

    let without = fixture
        .text(
            json!({ "pattern": "alpha", "output_mode": "files_without_matches", "glob": "*.txt" }),
        )
        .await;
    assert_eq!(lines_of(&without), vec!["no-match.txt"]);
}

#[tokio::test]
async fn glob_type_and_depth_filters() {
    let fixture = Fixture::basic();
    let text = fixture
        .text(json!({ "pattern": "alpha", "glob": "*.rs" }))
        .await;
    assert_eq!(lines_of(&text), vec!["nested/b.rs:2:     let alpha = 1;"]);

    let text = fixture
        .text(json!({ "pattern": "alpha", "type": "rust" }))
        .await;
    assert_eq!(lines_of(&text), vec!["nested/b.rs:2:     let alpha = 1;"]);

    let text = fixture
        .text(json!({ "pattern": "alpha", "type": "md" }))
        .await;
    assert_eq!(lines_of(&text), vec!["nested/deeper/c.md:1: alpha deep"]);

    let text = fixture
        .text(json!({ "pattern": "alpha", "max_depth": 1, "glob": "*.txt" }))
        .await;
    let lines = lines_of(&text);
    assert_eq!(lines.len(), 2, "max_depth=1 只遍历顶层: {lines:?}");
    assert_eq!(lines[0], "a.txt:1: alpha");
    assert!(lines[1].starts_with("long.txt:1: x"), "{}", lines[1]);

    // 未知 type 映射为空 glob 列表 = 不过滤（源事实）。
    let text = fixture
        .text(json!({ "pattern": "alpha", "type": "unknown-type" }))
        .await;
    assert!(text.contains("nested/b.rs"), "未知 type 不产生过滤: {text}");
}

#[tokio::test]
async fn pattern_flags_invert_whole_word_fixed_strings_and_multiline() {
    let fixture = Fixture::basic();
    let text = fixture
        .text(json!({ "pattern": "alpha", "invert_match": true, "glob": "a.txt" }))
        .await;
    assert_eq!(
        lines_of(&text),
        vec!["a.txt:2: beta", "a.txt:3: ALPHA"],
        "反转匹配排除 line 1"
    );

    let text = fixture
        .text(json!({ "pattern": "alph", "whole_word": true, "glob": "*.txt" }))
        .await;
    assert_eq!(text, "No matches found.", "整词匹配不接受前缀");

    let text = fixture
        .text(json!({ "pattern": "alpha.x", "fixed_strings": true, "glob": "a.txt" }))
        .await;
    assert_eq!(text, "No matches found.", "字面量模式不解释 `.`");

    let text = fixture
        .text(json!({ "pattern": "let\\s+alpha", "multiline": false, "glob": "*.rs" }))
        .await;
    assert!(text.contains("nested/b.rs:2"), "单行正则正常: {text}");
}

#[tokio::test]
async fn head_limit_exact_fit_and_truncation_persist_full_output() {
    // 基线 fixture：命中 4 行但含超长行 → 单行截断本身就置 truncated。
    let fixture = Fixture::basic();
    let response = fixture.run(json!({ "pattern": "alpha" })).await;
    assert!(!response.text.contains("truncated at"));
    assert_eq!(
        response.structured["truncated"],
        json!(true),
        "单行 1000 字节截断属于截断事实"
    );

    // 恰好等于 head_limit：不标 truncated。
    let exact = Fixture::bulk(1, 3); // 单文件 3 行，避免并行遍历的不确定性
    let response = exact
        .run(json!({ "pattern": "alpha", "head_limit": 3 }))
        .await;
    assert!(!response.text.contains("truncated at"), "{}", response.text);
    assert_eq!(response.structured["truncated"], json!(false));
    assert_eq!(lines_of(&response.text).len(), 3);

    // 超过 head_limit：截断 + 落盘全量（收集到的完整输出）。
    let bulk = Fixture::bulk(1, 12);
    let response = bulk
        .run(json!({ "pattern": "alpha", "head_limit": 5 }))
        .await;
    let lines = lines_of(&response.text);
    assert_eq!(lines.len(), 8, "5 行 + 截断行 + 空行 + 落盘提示: {lines:?}");
    assert_eq!(lines[0], "f0000.txt:1: alpha 0 0");
    assert_eq!(lines[4], "f0000.txt:5: alpha 0 4");
    assert_eq!(lines[5], "... (truncated at 5 lines)");
    assert!(response.text.contains("[Full output saved to"));
    assert!(response.text.contains("[Full output saved to"));
    assert_eq!(response.structured["truncated"], json!(true));

    // 落盘文件包含收集到的全量匹配（源：`persist_truncated_output(&joined)`）。
    let persisted = extract_persisted_path(&response.text);
    let content = std::fs::read_to_string(&persisted).expect("落盘文件应存在");
    assert!(
        lines_of(content.trim_end()).len() >= 6,
        "落盘应含未截断部分"
    );
}

#[tokio::test]
async fn head_limit_zero_is_unlimited_and_offset_skips_after_truncation() {
    let bulk = Fixture::bulk(2, 3); // 6 行
    let text = bulk
        .text(json!({ "pattern": "alpha", "head_limit": 0 }))
        .await;
    assert_eq!(lines_of(&text).len(), 6);
    assert!(!text.contains("truncated at"));

    let text = bulk
        .text(json!({ "pattern": "alpha", "head_limit": 0, "offset": 2 }))
        .await;
    assert_eq!(lines_of(&text).len(), 4);
    assert_eq!(lines_of(&text)[0], "f0000.txt:3: alpha 0 2");

    // offset 在截断之后应用：先截 3 行，再跳过 1 行 → 2 行 + 提示行。
    // 使用单文件 fixture：多文件并行遍历下预算停止点不确定（源亦如此）。
    let single = Fixture::bulk(1, 3);
    let text = single
        .text(json!({ "pattern": "alpha", "head_limit": 3, "offset": 1 }))
        .await;
    assert!(text.starts_with("f0000.txt:2: alpha 0 1"), "{text}");
}

#[tokio::test]
async fn byte_budget_truncates_and_persists() {
    let bulk = Fixture::bulk(60, 40); // 2400 行，超过 20000 字节
    let response = bulk
        .run(json!({ "pattern": "alpha", "head_limit": 0 }))
        .await;
    assert!(
        response.text.contains("[Output truncated:"),
        "应触发字节预算: {}",
        &response.text[..response.text.len().min(200)]
    );
    assert!(response.text.contains("lines total"));
    assert!(response.text.contains("exceeds 20000 byte limit"));
    assert_eq!(response.structured["truncated"], json!(true));
    let persisted = extract_persisted_path(&response.text);
    assert!(persisted.exists(), "完整输出应落盘");
}

// ───────── GAP-023：截断判据是结构化的，不是正文子串 ─────────

/// 正文里出现截断字面量（`[Output truncated:` / `… [line truncated]`）时的**反例**：
/// 这些字面量是**匹配行内容**的一部分，不构成任何截断事实。
///
/// 旧实现在交付文本上做子串匹配，因此一次 133 字节、无截断的调用被误报
/// `structured.truncated=true`（GAP-023）。
#[tokio::test]
async fn truncation_flag_is_not_forged_by_matched_text() {
    let fixture = Fixture::basic();
    let forged_line =
        "[Output truncated: 99999 lines total, 1 bytes; showing first 1] and … [line truncated]";
    fixture.write_file("forged.txt", &format!("{forged_line}\n"));

    let response = fixture
        .run(json!({ "pattern": "Output truncated", "glob": "forged.txt" }))
        .await;
    // 前置条件：交付文本**确实**含旧判据要匹配的两个字面量（否则本用例不构成反例）。
    assert!(
        response.text.contains("[Output truncated:"),
        "{}",
        response.text
    );
    assert!(
        response.text.contains("… [line truncated]"),
        "{}",
        response.text
    );
    assert!(
        response.text.len() < grep::MAX_LINE_BYTES,
        "正文 {} 字节：既不足单行上限也不足字节预算",
        response.text.len()
    );
    assert_eq!(
        response.structured["truncated"],
        json!(false),
        "匹配行内容不得左右截断标志: {}",
        response.text
    );
    assert!(
        response.structured.get("persisted_path").is_none(),
        "没有截断就没有产物: {}",
        response.structured
    );

    // 正向对照：三种**真实**截断都必须置位（判据改成结构化字段后不得漏报）。
    let limited = Fixture::bulk(1, 12);
    let response = limited
        .run(json!({ "pattern": "alpha", "head_limit": 5 }))
        .await;
    assert_eq!(
        response.structured["truncated"],
        json!(true),
        "真实行数截断必须置位"
    );

    let budget = Fixture::bulk(60, 40);
    let response = budget
        .run(json!({ "pattern": "alpha", "head_limit": 0 }))
        .await;
    assert_eq!(
        response.structured["truncated"],
        json!(true),
        "真实字节预算截断必须置位"
    );

    let trimmed = Fixture::basic();
    let response = trimmed
        .run(json!({ "pattern": "alpha", "glob": "long.txt" }))
        .await;
    assert!(
        response.text.contains("… [line truncated]"),
        "{}",
        response.text
    );
    assert_eq!(
        response.structured["truncated"],
        json!(true),
        "真实单行截断必须置位"
    );
}

// ───────── GAP-024：落盘路径结构化外发 ─────────

/// 截断发生时，落盘路径必须出现在**结构化字段**里（与正文提示指向同一个文件）。
///
/// 旧实现只在正文提示里给出路径，`structured.persisted_path` 恒缺失：调用方无法结构化地
/// 取回落盘产物（GAP-024）。落盘路径与交付文本同为宿主路径单表示（D-003）。
#[tokio::test]
async fn truncated_output_reports_the_persisted_path_structurally() {
    // 字节预算截断：结构化路径 == 正文提示路径 == 真实产物。
    let bulk = Fixture::bulk(60, 40);
    let response = bulk
        .run(json!({ "pattern": "alpha", "head_limit": 0 }))
        .await;
    assert_eq!(response.structured["truncated"], json!(true));
    let structured = response.structured["persisted_path"]
        .as_str()
        .expect("字节预算截断必须结构化外发落盘路径")
        .to_string();
    let hinted = extract_persisted_path(&response.text);
    assert_eq!(
        PathBuf::from(&structured),
        hinted,
        "结构化字段与正文提示必须指向同一产物"
    );
    assert!(hinted.exists(), "产物必须真实存在: {}", hinted.display());
    let content = std::fs::read_to_string(&hinted).expect("读取产物");
    assert!(
        content.lines().count() > 250,
        "产物必须是未截断的全量（实际 {} 行）",
        content.lines().count()
    );

    // 行数截断：同样结构化外发。
    let limited = Fixture::bulk(1, 12);
    let response = limited
        .run(json!({ "pattern": "alpha", "head_limit": 5 }))
        .await;
    assert_eq!(response.structured["truncated"], json!(true));
    let structured = response.structured["persisted_path"]
        .as_str()
        .expect("行数截断也要给出落盘路径")
        .to_string();
    assert!(
        PathBuf::from(&structured).exists(),
        "行数截断的产物必须存在: {structured}"
    );

    // 反向对照：无截断的调用不产生该字段（缺失，而不是空串或占位）。
    let small = Fixture::bulk(1, 2);
    let response = small.run(json!({ "pattern": "alpha" })).await;
    assert_eq!(response.structured["truncated"], json!(false));
    assert!(
        response.structured.get("persisted_path").is_none(),
        "无截断不得出现落盘路径: {}",
        response.structured
    );
}

#[tokio::test]
async fn no_matches_and_error_paths() {
    let fixture = Fixture::basic();
    let response = fixture.run(json!({ "pattern": "zzz-not-here" })).await;
    assert!(!response.is_error);
    assert_eq!(response.text, "No matches found.");

    let response = fixture
        .run(json!({ "pattern": "alpha", "path": "does-not-exist" }))
        .await;
    assert!(response.is_error);
    assert!(
        response
            .text
            .starts_with("Error: Search path does not exist: "),
        "错误文案: {}",
        response.text
    );

    let response = fixture
        .run(json!({ "pattern": "alpha(", "path": "." }))
        .await;
    assert!(response.is_error, "非法正则应报错");
    assert!(response.text.starts_with("Error: "), "{}", response.text);

    let response = fixture.run(json!({ "path": "." })).await;
    assert!(response.is_error);
    assert_eq!(response.text, "Error: Missing required parameter 'pattern'");

    let response = fixture
        .run(json!({ "pattern": "alpha", "output_mode": "files" }))
        .await;
    assert!(response.is_error);
    assert!(
        response
            .text
            .starts_with("Error: Invalid output_mode: 'files'"),
        "{}",
        response.text
    );

    let response = fixture
        .run(json!({ "pattern": "alpha", "head_limit": 1.5 }))
        .await;
    assert!(response.is_error);
    assert!(response.text.contains("must be a non-negative integer"));
}

#[tokio::test]
async fn capability_rejection_uses_public_message() {
    let fixture = Fixture::basic();
    let response = fixture
        .run(json!({ "pattern": "alpha", "path": "../../etc" }))
        .await;
    assert!(response.is_error);
    assert_eq!(
        response.text,
        "Path is outside the authorized workspace: ../../etc"
    );
    assert!(
        !response
            .text
            .contains(fixture.root().to_string_lossy().as_ref()),
        "不得回显宿主真实路径"
    );
}

#[tokio::test]
async fn search_timeout_reports_source_message() {
    let bulk = Fixture::bulk(1_200, 2);
    let response = bulk
        .run_with_timeout(
            json!({ "pattern": "alpha", "head_limit": 0 }),
            Some(std::time::Duration::from_millis(1)),
        )
        .await;
    assert!(response.is_error);
    assert_eq!(
        response.text,
        "Error: Search timed out after 0 seconds. Please use a more specific pattern."
    );
}

#[tokio::test]
async fn client_cancellation_is_deterministic_and_stops_walk() {
    let fixture = Fixture::basic();
    let token = CancellationToken::new();
    token.cancel();
    let resolver = fixture.resolver();
    let persist = fixture.persist();
    let persist: Arc<dyn local_mcp_server::tasks::log::OutputPersist> = Arc::new(persist);
    let context = GrepContext::new(
        fixture.root().to_path_buf(),
        root_real(fixture.root()),
        &resolver,
        persist,
    );
    let response = grep::invoke(&json!({ "pattern": "alpha" }), &context, Some(&token)).await;
    assert!(response.is_error);
    assert_eq!(response.text, "Error: Search cancelled.");
}

#[tokio::test]
async fn multiline_matches_across_lines_and_interacts_with_invert_and_context() {
    let fixture_dir = TempDir::new().expect("tempdir");
    let persist_dir = TempDir::new().expect("persist");
    std::fs::write(fixture_dir.path().join("test.txt"), "foo\nbar\nbaz\n").expect("write");
    let fixture = Fixture {
        dir: fixture_dir,
        persist_dir,
    };

    // 单行模式：`foo.*bar` 不跨行 → 无匹配。
    let text = fixture
        .text(json!({ "pattern": "foo.*bar", "glob": "test.txt" }))
        .await;
    assert_eq!(text, "No matches found.");

    // 多行模式：跨行匹配（源 `test_grep_multiline`）。
    let text = fixture
        .text(json!({ "pattern": "foo.*bar", "multiline": true, "glob": "test.txt" }))
        .await;
    assert!(text.contains("foo"), "multiline 应跨行匹配: {text}");

    // 多行 + 反转：模式覆盖整个文件内容 → 反转后无结果（源 `test_grep_multiline_with_invert_match`）。
    let text = fixture
        .text(json!({
            "pattern": "foo.*baz",
            "multiline": true,
            "invert_match": true,
            "glob": "test.txt",
        }))
        .await;
    assert!(
        text.contains("No matches found"),
        "跨行匹配整个文件后反转应无结果: {text}"
    );

    // 多行 + 上下文：匹配行与上下文都出现（源 `test_grep_multiline_with_context`）。
    std::fs::write(
        fixture.dir.path().join("ctx.txt"),
        ["before\n", "START\n", "middle\n", "END\n", "after\n"].concat(),
    )
    .expect("write");
    let text = fixture
        .text(json!({
            "pattern": "START.*END",
            "multiline": true,
            "-A": 1,
            "glob": "ctx.txt",
        }))
        .await;
    assert!(text.contains("START") && text.contains("END"), "{text}");
}

#[tokio::test]
async fn files_and_count_modes_respect_head_limit_without_corrupting_counts() {
    let dir = TempDir::new().expect("tempdir");
    let persist_dir = TempDir::new().expect("persist");
    for index in 0..10 {
        std::fs::write(
            dir.path().join(format!("f{index:02}.txt")),
            ["needle"; 5].join("\n"),
        )
        .expect("write");
    }
    let fixture = Fixture { dir, persist_dir };

    // files 模式：head_limit = 前 N 个文件（源 `test_grep_files_mode_head_limit`）。
    let text = fixture
        .text(json!({
            "pattern": "needle",
            "output_mode": "files_with_matches",
            "head_limit": 3,
        }))
        .await;
    let body: Vec<&str> = text
        .split("... (truncated")
        .next()
        .unwrap()
        .lines()
        .collect();
    assert!(!body.is_empty());
    assert!(body.len() <= 3, "files 模式行数受 head_limit 限制: {text}");
    assert!(body.iter().all(|line| line.ends_with(".txt")), "{text}");

    // count 模式：计数完整（每文件 5 个匹配），不被预算截断（源 `test_grep_count_not_corrupted_by_stop`）。
    let text = fixture
        .text(json!({
            "pattern": "needle",
            "output_mode": "count",
            "head_limit": 2,
        }))
        .await;
    let body: Vec<&str> = text
        .split("... (truncated")
        .next()
        .unwrap()
        .lines()
        .collect();
    assert!(!body.is_empty());
    assert!(body.len() <= 2, "count 模式行数受 head_limit 限制: {text}");
    assert!(
        body.iter().all(|line| line.ends_with(":5")),
        "计数必须完整: {text}"
    );
}

#[tokio::test]
async fn guard_constants_match_source() {
    use local_mcp_server::tools::grep::{
        DEFAULT_HEAD_LIMIT, MAX_LINE_BYTES, MAX_OUTPUT_BYTES, SEARCH_THREADS_MAX, SEARCH_TIMEOUT,
    };
    assert_eq!(SEARCH_TIMEOUT, std::time::Duration::from_secs(15));
    assert_eq!(MAX_OUTPUT_BYTES, 20_000);
    assert_eq!(MAX_LINE_BYTES, 1_000);
    assert_eq!(SEARCH_THREADS_MAX, 8);
    assert_eq!(DEFAULT_HEAD_LIMIT, 250);
}

#[tokio::test]
async fn results_are_deterministic_across_runs() {
    let bulk = Fixture::bulk(40, 3);
    let first = bulk
        .text(json!({ "pattern": "alpha", "head_limit": 0 }))
        .await;
    let second = bulk
        .text(json!({ "pattern": "alpha", "head_limit": 0 }))
        .await;
    assert_eq!(first, second);
    assert_eq!(lines_of(&first).len(), 120);
}

/// 从截断提示中取出落盘路径。
fn extract_persisted_path(text: &str) -> PathBuf {
    let marker = "[Full output saved to ";
    let start = text.find(marker).expect("应含落盘提示") + marker.len();
    let rest = &text[start..];
    let end = rest.find(" — use Read tool").expect("提示格式");
    PathBuf::from(&rest[..end])
}
