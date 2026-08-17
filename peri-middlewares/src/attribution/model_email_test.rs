use super::*;

#[test]
fn test_peri_model() {
    assert_eq!(
        get_attribution_email("peri-sonnet"),
        "noreply@peri-code.dev"
    );
}

#[test]
fn test_claude() {
    assert_eq!(
        get_attribution_email("claude-sonnet-4-20250514"),
        "noreply@anthropic.com"
    );
}

#[test]
fn test_gpt() {
    assert_eq!(
        get_attribution_email("gpt-4o"),
        "openai@claude-code-best.win"
    );
}

#[test]
fn test_o_series() {
    assert_eq!(
        get_attribution_email("o4-mini"),
        "openai@claude-code-best.win"
    );
}

#[test]
fn test_gemini() {
    assert_eq!(
        get_attribution_email("gemini-2.5-flash"),
        "google-gemini@claude-code-best.win"
    );
}

#[test]
fn test_grok() {
    assert_eq!(
        get_attribution_email("grok-3"),
        "xai-org@claude-code-best.win"
    );
}

#[test]
fn test_glm() {
    assert_eq!(
        get_attribution_email("glm-4-plus"),
        "zai-org@claude-code-best.win"
    );
}

#[test]
fn test_deepseek() {
    assert_eq!(
        get_attribution_email("deepseek-v3"),
        "deepseek-ai@claude-code-best.win"
    );
}

#[test]
fn test_qwen() {
    assert_eq!(
        get_attribution_email("qwen-max"),
        "QwenLM@claude-code-best.win"
    );
}

#[test]
fn test_minimax() {
    assert_eq!(
        get_attribution_email("minimax-m1"),
        "MiniMax-AI@claude-code-best.win"
    );
}

#[test]
fn test_mimo() {
    assert_eq!(
        get_attribution_email("mimo-v2"),
        "XiaomiMiMo@claude-code-best.win"
    );
}

#[test]
fn test_kimi() {
    assert_eq!(
        get_attribution_email("kimi-k2"),
        "MoonshotAI@claude-code-best.win"
    );
}

#[test]
fn test_case_insensitive() {
    assert_eq!(
        get_attribution_email("CLAUDE-3-OPUS"),
        "noreply@anthropic.com"
    );
    assert_eq!(
        get_attribution_email("GPT-4-TURBO"),
        "openai@claude-code-best.win"
    );
}

#[test]
fn test_unknown_fallback() {
    assert_eq!(
        get_attribution_email("unknown-model-xyz"),
        "noreply@anthropic.com"
    );
}

#[test]
fn test_dalle_matches_openai() {
    assert_eq!(
        get_attribution_email("dall-e-3"),
        "openai@claude-code-best.win"
    );
}
