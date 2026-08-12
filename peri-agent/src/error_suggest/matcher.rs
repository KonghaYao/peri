use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

/// 通用 fuzzy：候选 + 查询，返回 top-N 候选（按 score 降序）
/// 复用 at-mention 的 SkimMatcherV2 算法，泛化为 &[String]
pub fn fuzzy_top_n<'a>(candidates: &'a [String], query: &str, n: usize) -> Vec<&'a String> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(&String, i64)> = candidates
        .iter()
        .filter_map(|c| matcher.fuzzy_match(c, query).map(|s| (c, s)))
        .collect();
    scored.sort_by_key(|&(_, s)| std::cmp::Reverse(s));
    scored.iter().take(n).map(|(c, _)| *c).collect()
}

/// 仅保留 score > 0 的候选（剔除完全不匹配的）
pub fn fuzzy_filter(candidates: &[String], query: &str) -> Vec<String> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(String, i64)> = candidates
        .iter()
        .filter_map(|c| matcher.fuzzy_match(c, query).map(|s| (c.clone(), s)))
        .collect();
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(c, _)| c).collect()
}

/// 带相似度下限的 fuzzy 过滤：仅保留 score >= min_score 的候选。
///
/// SkimMatcherV2 只做子序列匹配：丢字符类拼错（dockr→docker）分数通常 >= 90，
/// 而短查询/泛化子序列噪声（xy→xylophone）分数 <= 51——绝对阈值即可干净分隔。
/// 用于"Did you mean"类建议：无合格候选时调用方应回退到兜底文案，
/// 而不是给出与命令名毫不相关的硬凑候选。
pub fn fuzzy_filter_min(candidates: &[String], query: &str, min_score: i64) -> Vec<String> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(String, i64)> = candidates
        .iter()
        .filter_map(|c| matcher.fuzzy_match(c, query).map(|s| (c.clone(), s)))
        .filter(|(_, s)| *s >= min_score)
        .collect();
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(c, _)| c).collect()
}
