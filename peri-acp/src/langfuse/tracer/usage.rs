//! TokenUsage → langfuse_usage_details 转换。
//!
//! 纯函数无状态。提取自原 tracer.rs::on_llm_end。

use std::collections::HashMap;

use peri_model::TokenUsage;

/// 将 TokenUsage 转换为 Langfuse usage_details HashMap。
///
/// [TRAP] input_tokens 已被适配器规范化（Anthropic: raw + cache_creation + cache_read），
/// Langfuse 要求 input 为不含缓存的原始值，需减去缓存部分。**禁止简化此减法**，
/// 否则 Langfuse token 统计会翻倍。
pub(crate) fn build_usage_details(usage: &TokenUsage) -> HashMap<String, i32> {
    let mut map = HashMap::new();
    let cache_creation = usage.cache_creation_input_tokens.unwrap_or(0);
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
    // input_tokens 已被适配器规范化（Anthropic: raw + cache_creation + cache_read），
    // Langfuse 要求 input 为不含缓存的原始值，需减去缓存部分。
    let raw_input = usage
        .input_tokens
        .saturating_sub(cache_creation + cache_read);
    let total = raw_input + usage.output_tokens + cache_creation + cache_read;
    map.insert("input".to_string(), raw_input as i32);
    map.insert("output".to_string(), usage.output_tokens as i32);
    map.insert("total".to_string(), total as i32);
    if cache_creation > 0 {
        map.insert(
            "cache_creation_input_tokens".to_string(),
            cache_creation as i32,
        );
    }
    if cache_read > 0 {
        map.insert("cache_read_input_tokens".to_string(), cache_read as i32);
    }
    map
}
