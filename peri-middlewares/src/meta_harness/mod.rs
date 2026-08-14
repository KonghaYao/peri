//! MetaHarness 文档加载器：扫描 `{cwd}/.peri/meta/*.md`。
//!
//! 与 `skills/`、`agents_md/` 同构：加载逻辑在 middlewares，类型归契约层
//! （`peri-acp-types::meta_harness`）。
//!
//! 规则（设计 §2.2）：
//! - 仅扫描一级目录 `*.md`（**不递归**）；非 `.md` 文件忽略。
//! - 文件名即 key（`01_intro.md` → `01_intro`）。
//! - 读取失败（IO/权限）→ warn + 跳过该文件，不 fail 扫描。
//! - 目录不存在或 `read_dir` 失败 → warn + 返回空 map。
//! - 不解析 frontmatter，不 trim，全文字节语义保留。
//! - 不在加载器内判断配置开关或 section ID（那是调用方的职责）。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// 扫描 `{cwd}/.peri/meta/*.md`，返回 文件名(去 .md) → 全文。
///
/// 任何失败都不 fail 整体扫描（warn + 跳过/空 map）。调用方应保证 `cwd`
/// 合法；本函数内部路径 join 不做额外规范化。
pub fn scan_harness_docs(cwd: &str) -> HashMap<String, String> {
    let dir: PathBuf = Path::new(cwd).join(".peri").join("meta");
    let mut docs = HashMap::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                path = %dir.display(),
                %error,
                "meta_harness: scan skipped (cannot read .peri/meta)"
            );
            return docs;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "meta_harness: skipping unreadable dir entry");
                continue;
            }
        };
        let path = entry.path();
        // 只接受扩展名精确为 md 的**文件**（目录即使叫 x.md 也忽略，不递归）
        if !path.is_file() || path.extension().map(|e| e != "md").unwrap_or(true) {
            continue;
        }
        let file_name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(name) => name,
            None => {
                tracing::warn!(
                    path = %path.display(),
                    "meta_harness: skipping file with non-UTF-8 name"
                );
                continue;
            }
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                docs.insert(file_name.to_string(), content);
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "meta_harness: skipping unreadable doc"
                );
            }
        }
    }
    docs
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
