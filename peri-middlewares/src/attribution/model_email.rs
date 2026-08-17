//! 模型名称 → 邮箱映射表，用于 git commit Co-Authored-By trailer。
//!
//! 参照 claude-code 的域名方案，使用 `@claude-code-best.win` 构造虚拟邮箱。
//! GitHub 组织不支持 Co-Authored-By，因此使用自有域名。

/// 模型关键词匹配表。（匹配的关键词列表，邮箱地址）
const MODEL_EMAIL_MAP: &[(&[&str], &str)] = &[
    (&["peri-"], "noreply@peri-code.dev"),
    (&["claude"], "noreply@anthropic.com"),
    (
        &["gpt", "dall-e", "o1-", "o3-", "o4-"],
        "openai@claude-code-best.win",
    ),
    (&["gemini"], "google-gemini@claude-code-best.win"),
    (&["grok"], "xai-org@claude-code-best.win"),
    (&["glm"], "zai-org@claude-code-best.win"),
    (&["deepseek"], "deepseek-ai@claude-code-best.win"),
    (&["qwen"], "QwenLM@claude-code-best.win"),
    (&["minimax"], "MiniMax-AI@claude-code-best.win"),
    (&["mimo"], "XiaomiMiMo@claude-code-best.win"),
    (&["kimi"], "MoonshotAI@claude-code-best.win"),
];

/// 根据模型名称查找对应的 attribution 邮箱。
/// 匹配不区分大小写，匹配第一个命中的关键词条目。
/// 无匹配时回退到 Anthropic 邮箱。
pub fn get_attribution_email(model_name: &str) -> &str {
    let lower = model_name.to_lowercase();
    for (keywords, email) in MODEL_EMAIL_MAP {
        if keywords.iter().any(|kw| lower.contains(kw)) {
            return email;
        }
    }
    "noreply@anthropic.com"
}

#[cfg(test)]
#[path = "model_email_test.rs"]
mod tests;
