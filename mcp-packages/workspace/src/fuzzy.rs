//! 候选模糊排序：供工具在失败点生成 did-you-mean 恢复建议
//! （Bash 命令候选、文件路径候选）。排序用 Skim 子序列匹配。

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

/// 返回所有可匹配的候选（score 降序）。无匹配返回空。
pub(crate) fn fuzzy_filter(candidates: &[String], query: &str) -> Vec<String> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(String, i64)> = candidates
        .iter()
        .filter_map(|c| matcher.fuzzy_match(c, query).map(|s| (c.clone(), s)))
        .collect();
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(c, _)| c).collect()
}

/// 仅保留 score >= min_score 的候选（score 降序）。
///
/// SkimMatcherV2 只做子序列匹配：丢字符类拼错（dockr→docker）分数通常 >= 90，
/// 而短查询/泛化子序列噪声（xy→xylophone）分数 <= 51——绝对阈值即可干净分隔。
/// 用于命令建议：无合格候选时调用方应回退到兜底文案，
/// 而不是给出与命令名毫不相关的硬凑候选。
pub(crate) fn fuzzy_filter_min(candidates: &[String], query: &str, min_score: i64) -> Vec<String> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(String, i64)> = candidates
        .iter()
        .filter_map(|c| matcher.fuzzy_match(c, query).map(|s| (c.clone(), s)))
        .filter(|(_, s)| *s >= min_score)
        .collect();
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(c, _)| c).collect()
}
