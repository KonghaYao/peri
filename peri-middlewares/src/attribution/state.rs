//! Git attribution 追踪状态。
//!
//! 通过 prefix/suffix 字符级匹配计算每次 Write/Edit 操作的实际变更区域，
//! 累积为文件贡献字符数。

use std::collections::HashMap;

use super::model_email;

/// 单个文件的贡献信息
#[derive(Debug)]
pub struct FileContribution {
    /// 累积贡献字符数
    pub claude_chars: usize,
    /// 文件哈希（SHA-256，预留用于版本追踪）
    pub file_hash: String,
}

/// Git attribution 追踪状态
#[derive(Debug)]
pub struct AttributionState {
    /// 相对路径 → 贡献信息
    pub contributions: HashMap<String, FileContribution>,
    /// 当前模型名称
    pub model_name: String,
    /// 模型对应的 attribution 邮箱
    pub email: String,
}

impl AttributionState {
    #[must_use]
    pub fn new(model_name: String) -> Self {
        let email = model_email::attribution_email(&model_name).to_string();
        Self {
            contributions: HashMap::new(),
            model_name,
            email,
        }
    }

    /// 计算字符级贡献：前缀/后缀匹配找出实际变更区域。
    pub fn track_change(&mut self, file_path: &str, old_content: &str, new_content: &str) {
        let old_chars = old_content.chars().count();
        let new_chars = new_content.chars().count();
        let contribution = if old_content.is_empty() || new_content.is_empty() {
            // 新文件或全量删除：贡献为存在内容的全部字符数
            if old_content.is_empty() {
                new_chars
            } else {
                old_chars
            }
        } else {
            // 前缀/后缀匹配找出差异化区域
            let prefix_len = old_content
                .chars()
                .zip(new_content.chars())
                .take_while(|(a, b)| a == b)
                .count();
            let suffix_len = old_content
                .chars()
                .rev()
                .zip(new_content.chars().rev())
                .take_while(|(a, b)| a == b)
                .count();
            // 防止 prefix + suffix 超过 min_len（内容重叠的情况）
            let min_len = old_chars.min(new_chars);
            let overlap_free_suffix = suffix_len.min(min_len.saturating_sub(prefix_len));
            let old_changed = old_chars.saturating_sub(prefix_len + overlap_free_suffix);
            let new_changed = new_chars.saturating_sub(prefix_len + overlap_free_suffix);
            old_changed.max(new_changed)
        };

        let entry = self
            .contributions
            .entry(file_path.to_string())
            .or_insert_with(|| FileContribution {
                claude_chars: 0,
                file_hash: String::new(),
            });
        entry.claude_chars += contribution;
    }

    /// 生成 Co-Authored-By trailer 文本
    #[must_use]
    pub fn co_authored_by(&self) -> String {
        format!("Co-Authored-By: {} <{}>", self.model_name, self.email)
    }
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
