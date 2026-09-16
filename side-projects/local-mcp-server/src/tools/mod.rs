//! 工具实现总入口（WP-002/WP-003 的语义 + 交付表示层的公共改写）。
//!
//! 子模块所有权（声明由 WP-001 冻结，实现互不交叉）：
//! - [`fs`]：Read/Write/Edit/Glob/folder_operations。
//! - [`grep`]：Grep 语义与遍历。
//! - [`bash`]：Bash 三字段解析与结果投影。
//!
//! ## 本文件承担的路由职责
//!
//! 七工具的执行分派已收敛到进程内执行内核 [`crate::runtime::InProcessExecutor`]：
//! 它按工具名直接调用本模块的语义实现（[`fs::FsRuntime`]、[`grep::invoke`]、
//! [`bash::BashTool`]），因此这里只保留**与具体工具无关**的交付表示层：
//! 交付字节预算复核与超预算落盘。
//!
//! 执行路径上没有独立执行进程、没有 RPC：文件类工具在进程内直接读工作区根内
//! 的路径，`Bash` 经唯一任务注册表（[`crate::tasks::TaskRegistry`]）。**路径表示只有一种**
//! （宿主路径）：D-003 之后不存在第二种表示，因此交付层不做任何路径改写——工具自己产出的
//! 路径（落盘产物、搜索根、任务日志）本来就是宿主绝对路径，逐字外发即可。

pub mod bash;
pub mod fs;
pub mod grep;

use crate::capability::RootDir;
use crate::output::{Persister, DEFAULT_ARTIFACT_DIR};
use crate::wire::ToolResponse;
use std::path::PathBuf;

// ───────────────────── 交付侧字节预算（FC-GLOB-02 / FC-GREP-04）─────────────────────

/// 交付字节预算：20000 字节总量。
///
/// 与工具侧两个同类常量同值（[`fs::limits::GLOB_MAX_OUTPUT_BYTES`] 与
/// [`grep::MAX_OUTPUT_BYTES`]）：工具自身的截断发生在**同一份宿主表示**上，本层复核的是
/// 交付出去的那份文本，使 20000 字节对**调用方可见的输出**成立。
pub(crate) const DELIVERY_MAX_BYTES: usize = 20_000;

/// 受交付字节预算约束的工具（工具身份取结构化字段 `tool`，不取正文文本）。
pub(crate) const DELIVERY_BUDGET_TOOLS: [&str; 2] = ["Glob", "Grep"];

/// 在**交付文本**上复核 20000 字节预算。
///
/// 判据全部**结构化**：只看结构化字段 `tool` 与 `count`，**绝不**用正文文本里的字面量
/// （`[Output truncated:`）判断——工作区里的**文件名**就能制造同样的字符串，从而把这条
/// 预算守卫整个绕过去（round 4 的 R4-01）。
///
/// 触发条件是工具已经截断过、但**截断后的文本**仍超过预算：典型形状是条数截断（Glob 内联
/// 前 1000 条）在长文件名下的输出。这里不改工具语义（工具自身的条数与字节兜底保持与源实现
/// 逐字一致），只在交付侧把内联条数继续按字节收敛，使调用方看到 ≤ 20000 字节：
/// - Glob 的上限仍是 [`fs::limits::GLOB_HEAD_RESULTS_ON_BYTES_OVERFLOW`]（100 条），
///   必要时按剩余字节继续减少；
/// - Grep 与工具侧同形，按「头部 + 提示 + 落盘指引」的字节预算收敛；
/// - 两者都至少保留一行（单行本身超预算时不再产出空正文，与源实现的兜底一致）。
///
/// 落盘语义：响应已带 `persisted_path`（工具自己的产物，内容就是同一份宿主表示的完整结果）
/// 时**复用**它——重复落盘只会得到一份更小的副本；没有产物时才落盘交付文本的全量。
pub(crate) fn enforce_delivery_budget(root: &RootDir, response: &mut ToolResponse) {
    if response.is_error || response.text.len() <= DELIVERY_MAX_BYTES {
        return;
    }
    let Some(structured) = response.structured.as_object() else {
        return;
    };
    let tool = match structured.get("tool").and_then(serde_json::Value::as_str) {
        Some(tool) if DELIVERY_BUDGET_TOOLS.contains(&tool) => tool.to_string(),
        _ => return,
    };
    let existing_persisted = structured
        .get("persisted_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let glob_count = structured
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);

    // 正文与软告警前缀分离：Glob 的 `Note: …` 前缀不占内联名额；两者的截断提示与落盘
    // 指引都在**尾部**，重新整形时必须剥掉，否则会出现两条提示与两个落盘指引。
    let (prefix, body) = match tool.as_str() {
        "Glob" => split_glob_warning_prefix(&response.text),
        _ => ("", strip_truncation_tail(&response.text)),
    };

    // 落盘指引先确定：它参与头部行的预算计算。
    let (persisted, hint) = match existing_persisted {
        Some(path) => (Some(path.clone()), full_output_hint(&path)),
        None => {
            let (path, hint) = persist_delivered_text(root, body);
            (path.map(|path| path.to_string_lossy().to_string()), hint)
        }
    };

    let (head, head_count) = match tool.as_str() {
        // Glob：与工具侧同一条规则——字节超限时最多内联前 100 条；这里再按字节收紧，
        // 使「头部 + 提示 + 落盘指引」落在预算内（长文件名下 100 条本身就超预算）。
        "Glob" => glob_delivery_head(body, &hint, prefix),
        // Grep：按同一预算保留头部行（与工具侧的字节兜底同形）。
        _ => grep_delivery_head(body, &hint, prefix),
    };
    let count = glob_count.unwrap_or_else(|| body.lines().count());
    let delivered = match tool.as_str() {
        "Glob" => format!(
            "{prefix}{}",
            fs::glob_byte_overflow_text(&head, count, body.len(), head_count, &hint)
        ),
        _ => format!(
            "{prefix}{}",
            grep::byte_overflow_text(&head, body.lines().count(), body.len(), head_count, &hint)
        ),
    };
    response.text = delivered;
    if let Some(object) = response.structured.as_object_mut() {
        object.insert("truncated".to_string(), serde_json::json!(true));
        if let Some(path) = persisted {
            object.insert("persisted_path".to_string(), serde_json::json!(path));
        }
    }
}

/// Grep 的头部行选择：保留尽可能多的行，使「前缀 + 头部 + 提示 + 落盘指引」落在预算内。
///
/// 至少保留一行（与工具侧的字节兜底一致：单行超长也不会产出空正文）。
fn grep_delivery_head(body: &str, hint: &str, prefix: &str) -> (String, usize) {
    let total_lines = body.lines().count();
    let bytes = body.len();
    let budget = DELIVERY_MAX_BYTES.saturating_sub(prefix.len());
    let mut head: Vec<&str> = Vec::new();
    let mut head_bytes = 0usize;
    for line in body.lines() {
        let candidate_bytes = head_bytes + line.len() + usize::from(!head.is_empty());
        let head_count = head.len() + 1;
        let tail_bytes = grep::byte_overflow_text("", total_lines, bytes, head_count, hint).len();
        if !head.is_empty() && candidate_bytes + tail_bytes > budget {
            break;
        }
        head.push(line);
        head_bytes = candidate_bytes;
    }
    (head.join("\n"), head.len())
}

/// Glob 的头部行选择：先按工具侧的条数上限（100 条）取头，再按剩余字节收紧。
///
/// 条数上限与 [`fs::limits::GLOB_HEAD_RESULTS_ON_BYTES_OVERFLOW`] 一致（不放大），字节收紧
/// 是本层的附加保证：长文件名下 100 条本身就超过 20000 字节，此时继续减少条数，使调用方
/// 看到的交付文本落在预算内。至少保留一条。
fn glob_delivery_head(body: &str, hint: &str, prefix: &str) -> (String, usize) {
    let budget = DELIVERY_MAX_BYTES.saturating_sub(prefix.len());
    let cap = fs::limits::GLOB_HEAD_RESULTS_ON_BYTES_OVERFLOW.min(body.lines().count());
    let mut head: Vec<&str> = Vec::new();
    let mut head_bytes = 0usize;
    for line in body.lines().take(cap) {
        let candidate_bytes = head_bytes + line.len() + usize::from(!head.is_empty());
        let head_count = head.len() + 1;
        let tail_bytes =
            fs::glob_byte_overflow_text("", body.lines().count(), body.len(), head_count, hint)
                .len();
        if !head.is_empty() && candidate_bytes + tail_bytes > budget {
            break;
        }
        head.push(line);
        head_bytes = candidate_bytes;
    }
    (head.join("\n"), head.len())
}

/// 剥掉交付正文末尾的「截断提示 / 落盘指引」段落（与工具侧共用同一套格式）。
///
/// 这是**表示层**的规整，不是安全判据：只剥**单行段落**且必须以提示的固定后缀 `]` 收尾，
/// 因此正文里的普通匹配行（含同名文件名）不会被误剥。
fn strip_truncation_tail(text: &str) -> &str {
    const NOTICE_PREFIXES: [&str; 3] = [
        "[Output truncated: ",
        "[Full output saved to ",
        "[Failed to save full output to ",
    ];
    let mut body = text.trim_end_matches('\n');
    while let Some(index) = body.rfind("\n\n") {
        let paragraph = &body[index + 2..];
        let is_notice = paragraph.ends_with(']')
            && !paragraph.contains('\n')
            && NOTICE_PREFIXES
                .iter()
                .any(|prefix| paragraph.starts_with(prefix));
        if !is_notice {
            break;
        }
        body = body[..index].trim_end_matches('\n');
    }
    body
}

/// 复用既有产物时重建落盘指引（格式与 [`Persister::persist`] 逐字一致）。
fn full_output_hint(path: &str) -> String {
    format!("\n\n[Full output saved to {path} — use Read tool to view complete content]")
}

/// 把交付侧全量文本落盘到根内产物目录（与工具侧同一目录约定与命名），返回路径与提示。
///
/// 复用 [`Persister`] 而不是另写一份写文件逻辑：两条落盘路径的文件命名、提示文案与失败
/// 降级因此逐字一致。产物目录是根内固定常量，不接受调用方输入。
fn persist_delivered_text(root: &RootDir, full: &str) -> (Option<PathBuf>, String) {
    let outcome = Persister::new(DEFAULT_ARTIFACT_DIR).persist(root, full);
    (outcome.path.map(PathBuf::from), outcome.hint)
}

/// 拆出 Glob 的软告警前缀（`Note: ...\n\n`）与结果正文；无前缀时前缀为空串。
fn split_glob_warning_prefix(text: &str) -> (&str, &str) {
    const NOTE: &str = "Note: ";
    const SEPARATOR: &str = "\n\n";
    if let Some(rest) = text.strip_prefix(NOTE) {
        if let Some(index) = rest.find(SEPARATOR) {
            let split = NOTE.len() + index + SEPARATOR.len();
            return (&text[..split], &text[split..]);
        }
    }
    ("", text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::RequestedPath;
    use crate::wire::StructuredOutput;

    /// 真实授权根（`tempdir` 在 macOS 上是 `/var/...`，canonicalize 后才是 `/private/var/...`：
    /// 生产路径由 `main` 规范化后交给 capability 根，这里与生产一致）。
    fn rooted() -> (tempfile::TempDir, RootDir) {
        let dir = tempfile::tempdir().expect("临时目录");
        let base = dir.path().canonicalize().expect("规范化临时目录");
        let root = RootDir::open(base).expect("打开授权根");
        (dir, root)
    }

    /// 构造一条「工具已按条数截断、但截断后的文本仍超预算」的 Glob 响应。
    ///
    /// 与源实现的条数兜底同形：正文是 1000 条宿主绝对路径（长文件名），尾部带条数提示与
    /// 落盘指引；同时像工具那样把**完整结果**落盘并在结构化字段里报出 `persisted_path`
    /// （结构化字段，不是正文子串）。
    fn count_truncated_glob(name_len: usize, root: &RootDir) -> ToolResponse {
        let lines: Vec<String> = (0..1_000)
            .map(|index| {
                format!(
                    "{}/bulk/{index:04}_{}.txt",
                    root.base().display(),
                    "n".repeat(name_len)
                )
            })
            .collect();
        let full = lines.join("\n");
        let artifact = root
            .base()
            .join(DEFAULT_ARTIFACT_DIR)
            .join("local-tool-output-test.txt");
        let requested = RequestedPath::parse(
            artifact
                .strip_prefix(root.base())
                .expect("产物在根内")
                .to_str()
                .expect("UTF-8"),
            root.base(),
        )
        .expect("产物路径");
        root.write_new_file(&requested, full.as_bytes())
            .expect("写入产物");
        let text = format!(
            "{full}\n\n[Output truncated: 1001 files total, showing first 1000]\n\n[Full output saved to {} — use Read tool to view complete content]",
            artifact.display()
        );
        let mut structured = StructuredOutput::ok("Glob");
        structured.truncated = true;
        structured.persisted_path = Some(artifact.to_string_lossy().to_string());
        let mut value = serde_json::to_value(structured).expect("结构化字段");
        value["count"] = serde_json::json!(1_001);
        value["early_stopped"] = serde_json::json!(true);
        let response = ToolResponse {
            text,
            structured: value,
            is_error: false,
            meta: None,
        };
        assert!(response.text.len() > DELIVERY_MAX_BYTES, "构造必须超预算");
        response
    }

    #[test]
    fn test_glob_delivery_budget_holds_for_long_file_names() {
        let (_dir, root) = rooted();
        let mut response = count_truncated_glob(240, &root);
        let artifact = response.structured["persisted_path"]
            .as_str()
            .expect("构造带产物")
            .to_string();
        enforce_delivery_budget(&root, &mut response);
        assert!(
            response.text.len() <= DELIVERY_MAX_BYTES,
            "交付文本必须落在预算内：{} 字节",
            response.text.len()
        );
        // 结构化判据（不是正文子串）：truncated 置位 + 复用工具自己的全量产物。
        assert_eq!(response.structured["truncated"], serde_json::json!(true));
        assert_eq!(
            response.structured["persisted_path"].as_str(),
            Some(artifact.as_str()),
            "已有产物必须复用，不重复落盘"
        );
        let full = std::fs::read_to_string(&artifact).expect("读取落盘产物");
        assert_eq!(full.lines().count(), 1_000, "落盘内容是完整结果");
    }

    #[test]
    fn test_glob_delivery_head_is_shrunk_by_bytes_not_by_text_markers() {
        let (_dir, root) = rooted();
        // 文件名里逐字放入截断提示：判定必须是结构化的，不受正文内容影响。
        let mut response = count_truncated_glob(240, &root);
        response.text = response.text.replace("/bulk/", "/bulk/[Output truncated: ");
        enforce_delivery_budget(&root, &mut response);
        assert!(response.text.len() <= DELIVERY_MAX_BYTES);
        assert_eq!(response.structured["truncated"], serde_json::json!(true));
    }

    #[test]
    fn test_glob_delivery_budget_keeps_short_results_untouched() {
        let (_dir, root) = rooted();
        let mut response = count_truncated_glob(3, &root);
        response.text = format!(
            "{}/a.txt\n{}/b.txt",
            root.base().display(),
            root.base().display()
        );
        response.structured = serde_json::to_value(StructuredOutput::ok("Glob")).expect("结构化");
        let original = response.text.clone();
        enforce_delivery_budget(&root, &mut response);
        assert_eq!(response.text, original, "预算内的交付文本不得被改动");
    }

    #[test]
    fn test_non_budget_tools_are_not_reshaped() {
        let (_dir, root) = rooted();
        let mut response = count_truncated_glob(240, &root);
        response.structured["tool"] = serde_json::json!("Read");
        let original = response.text.clone();
        enforce_delivery_budget(&root, &mut response);
        assert_eq!(response.text, original, "只约束 Glob/Grep");
    }
}
