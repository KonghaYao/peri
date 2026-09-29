//! 项目指令资源面（`peri-instruction://workspace/{main|local|index}`）。
//!
//! 语义口径（W1 冻结，E14 / §6.1）：
//! - **主文档候选**：`AGENTS.md` → `CLAUDE.md` → `.claude/AGENTS.md`（相对
//!   cwd 的工作区绑定根），取**首个存在**的文件；选中文件为空（trim 空）时
//!   main 视为不存在且**不继续尝试后继候选**（逐字保持 `read_frozen_content`
//!   的既有语义，不借迁移改变优先级）；
//! - **本地叠加**：`CLAUDE.local.md` 独立成资源，不与 main 合并（组合归宿主）；
//! - **`@import` 展开**：仅文件名以 `CLAUDE` 开头的主文档解析（现状同口径）；
//!   深度上限 3、visited 防环、`<!-- @import path -->` 原样语法；
//!   **W1 冻结的授权范围** = 被导入文件的 canonical 路径必须位于 cwd（工作区
//!   根，canonical）子树内——越界（含绝对路径与 `..` 逃逸）保留原始占位符并
//!   记 warn（比现状更严：现状 `Path::join` 会接受绝对路径与任意越界）；
//! - main 读取允许候选文件自身是 symlink（保持存量用法）；import 目标经
//!   canonical 校验，symlink 指向工作区内是允许的；
//! - 预算：单文档超过 `max_file_bytes` 视为不可用（不列、read 按找不到处理）；
//!   import 数量上限 `max_imports`，超限的 import 保留占位符并记 warn。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use peri_acp_types::workspace_resources::{
    digest_bytes, InstructionDocument, INSTRUCTION_LOCAL_URI, INSTRUCTION_MAIN_URI, MIME_JSON,
    MIME_MARKDOWN, MIME_TEXT,
};
use serde::Serialize;

use super::{ResourceBudget, ResourceError};

/// 主文档候选（顺序即优先级；prompt 面只选首个存在的文件，不向父目录继承）。
pub(crate) const MAIN_CANDIDATES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", ".claude/AGENTS.md"];
/// 本地叠加文档。
pub(crate) const LOCAL_FILE: &str = "CLAUDE.local.md";
/// `@import` 递归深度上限（与既有 `read_frozen_content` 的 `3` 同值）。
const IMPORT_DEPTH: u32 = 3;

/// 已解析的指令文档。
#[derive(Debug, Clone)]
pub(crate) struct InstructionDoc {
    /// 文档正文（main 为 import 展开后的文本；local/index 为原文）。
    pub text: String,
    /// 正文 digest（读取面复算一致）。
    pub digest: String,
    /// 来源标签（相对 cwd 的路径，如 `CLAUDE.md`；不含绝对路径）。
    pub source_label: String,
}

/// 单条 `@import` 依赖记录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportRecord {
    /// 被导入文件相对 cwd 的路径。
    pub relative: String,
    /// 被导入文件的原始字节 digest。
    pub digest: String,
}

/// `peri-instruction://workspace/index` 的 JSON manifest。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionIndex {
    /// 固定 `"workspace"`（authority；不是 cwd 的绝对路径）。
    pub scope: &'static str,
    /// 主文档候选顺序。
    pub candidates: Vec<&'static str>,
    /// 选中主文档的相对名；无主文档时 `None`。
    pub selected: Option<String>,
    /// 主/本地文档的存在性与 digest（index 自身不列入）。
    pub documents: Vec<InstructionIndexDocument>,
    /// 主文档的 import 依赖（深度优先顺序）。
    pub imports: Vec<ImportRecord>,
}

/// index 中的单个文档条目。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionIndexDocument {
    pub uri: &'static str,
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// 一次扫描的结果（main / local / index）。
pub(crate) struct InstructionBundle {
    pub main: Option<InstructionDoc>,
    pub local: Option<InstructionDoc>,
    pub index: InstructionIndex,
}

/// 扫描工作区指令（`cwd` 为工作区绑定根；每次读取实时扫描，无跨请求缓存）。
pub(crate) fn scan(cwd: &Path, budget: &ResourceBudget) -> InstructionBundle {
    let scope_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    let mut imports: Vec<ImportRecord> = Vec::new();
    let (main, selected) = scan_main(cwd, &scope_canonical, budget, &mut imports);
    let local = scan_local(cwd, &scope_canonical, budget);

    let index = InstructionIndex {
        scope: "workspace",
        candidates: MAIN_CANDIDATES.to_vec(),
        selected,
        documents: vec![
            InstructionIndexDocument {
                uri: INSTRUCTION_MAIN_URI,
                present: main.is_some(),
                relative: main.as_ref().map(|doc| doc.source_label.clone()),
                digest: main.as_ref().map(|doc| doc.digest.clone()),
            },
            InstructionIndexDocument {
                uri: INSTRUCTION_LOCAL_URI,
                present: local.is_some(),
                relative: local.as_ref().map(|doc| doc.source_label.clone()),
                digest: local.as_ref().map(|doc| doc.digest.clone()),
            },
        ],
        imports,
    };

    InstructionBundle { main, local, index }
}

/// 主文档：首个存在的候选 + `@import` 展开。
fn scan_main(
    cwd: &Path,
    scope_canonical: &Path,
    budget: &ResourceBudget,
    imports: &mut Vec<ImportRecord>,
) -> (Option<InstructionDoc>, Option<String>) {
    for relative in MAIN_CANDIDATES {
        let path = cwd.join(relative);
        if !path.is_file() {
            continue;
        }
        let Some(raw) = read_bounded_text(&path, budget.max_file_bytes) else {
            tracing::warn!(
                candidate = relative,
                "指令候选读取失败或超预算，main 视为不存在"
            );
            return (None, None);
        };
        if raw.trim().is_empty() {
            // 首个存在但为空：不继续尝试后继候选（现状语义）。
            return (None, Some(relative.to_string()));
        }
        let text = if is_claude_series(relative) {
            let base_dir = path.parent().unwrap_or(cwd);
            let mut visited: HashSet<PathBuf> = HashSet::new();
            if let Ok(canonical) = path.canonicalize() {
                visited.insert(canonical);
            }
            resolve_imports(
                &raw,
                base_dir,
                scope_canonical,
                IMPORT_DEPTH,
                budget,
                &mut visited,
                imports,
            )
        } else {
            raw
        };
        let digest = digest_bytes(text.as_bytes());
        return (
            Some(InstructionDoc {
                text,
                digest,
                source_label: relative.to_string(),
            }),
            Some(relative.to_string()),
        );
    }
    (None, None)
}

/// 本地叠加文档：`CLAUDE.local.md`（空 → 不存在）。
fn scan_local(
    cwd: &Path,
    _scope_canonical: &Path,
    budget: &ResourceBudget,
) -> Option<InstructionDoc> {
    let path = cwd.join(LOCAL_FILE);
    if !path.is_file() {
        return None;
    }
    let text = read_bounded_text(&path, budget.max_file_bytes)?;
    if text.trim().is_empty() {
        return None;
    }
    let digest = digest_bytes(text.as_bytes());
    Some(InstructionDoc {
        text,
        digest,
        source_label: LOCAL_FILE.to_string(),
    })
}

/// 读取上限内的原始文本（UTF-8 且非空字节校验；失败返回 `None`）。
fn read_bounded_text(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > max_bytes {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn is_claude_series(relative: &str) -> bool {
    Path::new(relative)
        .file_name()
        .map(|name| name.to_string_lossy().starts_with("CLAUDE"))
        .unwrap_or(false)
}

/// 递归解析 `<!-- @import path -->`（深度上限、防环、工作区根子树授权）。
#[allow(clippy::too_many_arguments)]
fn resolve_imports(
    content: &str,
    base_dir: &Path,
    scope_canonical: &Path,
    depth: u32,
    budget: &ResourceBudget,
    visited: &mut HashSet<PathBuf>,
    imports: &mut Vec<ImportRecord>,
) -> String {
    if depth == 0 {
        return content.to_string();
    }
    let mut result = String::with_capacity(content.len());
    let mut pos = 0;
    while pos < content.len() {
        let Some(offset) = content[pos..].find("<!-- @import ") else {
            result.push_str(&content[pos..]);
            break;
        };
        let abs_pos = pos + offset;
        result.push_str(&content[pos..abs_pos]);
        let after = &content[abs_pos + 13..]; // 13 = "<!-- @import ".len()
        let Some(end) = after.find(" -->") else {
            result.push_str("<!-- @import ");
            pos = abs_pos + 13;
            continue;
        };
        let placeholder_end = abs_pos + 13 + end + 4; // 4 = " -->".len()
        let import_path = after[..end].trim();
        let resolved = base_dir
            .join(import_path)
            .canonicalize()
            .unwrap_or_else(|_| base_dir.join(import_path));

        // 越界 / 环 / 不存在：保留原始占位符（越界额外记 warn——W1 冻结的授权范围）。
        if !resolved.starts_with(scope_canonical) {
            tracing::warn!(
                import = import_path,
                "指令 @import 目标在工作区根之外，保留原始占位符"
            );
            result.push_str(&content[abs_pos..placeholder_end]);
            pos = placeholder_end;
            continue;
        }
        if visited.contains(&resolved) || !resolved.is_file() {
            result.push_str(&content[abs_pos..placeholder_end]);
            pos = placeholder_end;
            continue;
        }
        if imports.len() >= budget.max_imports {
            tracing::warn!(
                import = import_path,
                "指令 @import 数量超过预算，保留原始占位符"
            );
            result.push_str(&content[abs_pos..placeholder_end]);
            pos = placeholder_end;
            continue;
        }
        let Some(imported) = read_bounded_text(&resolved, budget.max_file_bytes) else {
            result.push_str(&content[abs_pos..placeholder_end]);
            pos = placeholder_end;
            continue;
        };

        visited.insert(resolved.clone());
        let relative_label = resolved
            .strip_prefix(scope_canonical)
            .ok()
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| import_path.to_string());
        imports.push(ImportRecord {
            relative: relative_label,
            digest: digest_bytes(imported.as_bytes()),
        });

        let import_dir = resolved.parent().unwrap_or(base_dir);
        let resolved_content = resolve_imports(
            &imported,
            import_dir,
            scope_canonical,
            depth - 1,
            budget,
            visited,
            imports,
        );
        result.push_str(&resolved_content);
        pos = placeholder_end;
    }
    result
}

/// 一次读取请求的产物。
#[derive(Debug)]
pub(crate) struct InstructionRead {
    pub mime: &'static str,
    pub text: String,
    pub digest: String,
}

/// 读取指定指令文档（实时扫描；main/local 按找不到处理，index 恒可读）。
pub(crate) fn read(
    cwd: &Path,
    budget: &ResourceBudget,
    document: InstructionDocument,
) -> Result<InstructionRead, ResourceError> {
    let bundle = scan(cwd, budget);
    match document {
        InstructionDocument::Main => {
            let doc = bundle.main.ok_or(ResourceError::NotFound)?;
            Ok(InstructionRead {
                mime: MIME_MARKDOWN,
                text: doc.text,
                digest: doc.digest,
            })
        }
        InstructionDocument::Local => {
            let doc = bundle.local.ok_or(ResourceError::NotFound)?;
            Ok(InstructionRead {
                mime: MIME_TEXT,
                text: doc.text,
                digest: doc.digest,
            })
        }
        InstructionDocument::Index => {
            let text =
                serde_json::to_string_pretty(&bundle.index).map_err(|_| ResourceError::Io)?;
            let digest = digest_bytes(text.as_bytes());
            Ok(InstructionRead {
                mime: MIME_JSON,
                text,
                digest,
            })
        }
    }
}

#[cfg(test)]
#[path = "instructions_test.rs"]
mod tests;
