use crate::error_suggest::registry::Suggestion;

/// 把建议格式化进原错误文本
/// 风格：中文自然语言，无 emoji，与 Edit 工具的 hint 风格一致
pub fn format_suggestion(original_error: &str, sug: &Suggestion) -> String {
    let mut out = format!("{}\n\n---\n{}", original_error, sug.summary);
    if let Some(d) = &sug.details {
        out.push('\n');
        out.push_str(d);
    }
    out.push_str("\n---");
    out
}

/// 把候选列表格式化为 bullet 文本
pub fn format_candidates(candidates: &[String]) -> String {
    candidates
        .iter()
        .map(|c| format!("  • {c}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// "Did you mean" 风格的 summary
pub fn did_you_mean_summary(resource_kind: &str, candidates: &[String]) -> String {
    if candidates.is_empty() {
        return format!("No similar {resource_kind} found.");
    }
    let bullet = format_candidates(candidates);
    format!("Did you mean one of these {resource_kind}?\n{bullet}")
}
