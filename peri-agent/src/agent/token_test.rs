use super::*;

fn make_usage(
    input: u32,
    output: u32,
    cache_creation: Option<u32>,
    cache_read: Option<u32>,
) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        request_id: None,
    }
}

#[test]
fn test_accumulate_sums_tokens() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(100, 50, Some(30), Some(20)));
    tracker.accumulate(&make_usage(200, 80, Some(10), Some(40)));
    assert_eq!(tracker.total_input_tokens, 300);
    assert_eq!(tracker.total_output_tokens, 130);
    assert_eq!(tracker.total_cache_creation_tokens, 40);
    assert_eq!(tracker.total_cache_read_tokens, 60);
    assert_eq!(tracker.llm_call_count, 2);
}

#[test]
fn test_accumulate_with_none_cache() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(100, 50, None, None));
    assert_eq!(tracker.total_input_tokens, 100);
    assert_eq!(tracker.total_output_tokens, 50);
    assert_eq!(tracker.total_cache_creation_tokens, 0);
    assert_eq!(tracker.total_cache_read_tokens, 0);
    assert_eq!(tracker.llm_call_count, 1);
}

#[test]
fn test_estimated_context_tokens_none() {
    let tracker = TokenTracker::default();
    assert!(tracker.estimated_context_tokens().is_none());
}

#[test]
fn test_accumulate_zero_input_tokens_does_not_overwrite_last_usage() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(50000, 2000, None, None));
    assert_eq!(tracker.estimated_context_tokens(), Some(50000));

    // 异常 API 响应 input_tokens=0，不应覆盖 last_usage
    tracker.accumulate(&make_usage(0, 100, None, None));
    assert_eq!(tracker.total_input_tokens, 50000, "total 仍累积");
    assert_eq!(tracker.total_output_tokens, 2100, "total 仍累积");
    assert_eq!(tracker.llm_call_count, 2);
    assert_eq!(
        tracker.estimated_context_tokens(),
        Some(50000),
        "last_usage 不应被 input_tokens=0 覆盖"
    );
}

#[test]
fn test_estimated_context_tokens_some() {
    let mut tracker = TokenTracker::default();
    // input 已在 adapter 层规范化：raw(1000) + cache_creation(200) + cache_read(300) = 1500
    tracker.accumulate(&make_usage(1500, 500, Some(200), Some(300)));
    // estimated_context_tokens 只返回 input_tokens
    assert_eq!(tracker.estimated_context_tokens(), Some(1500));
}

#[test]
fn test_estimated_context_tokens_no_cache() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(1000, 500, None, None));
    // estimated_context_tokens 只返回 input_tokens
    assert_eq!(tracker.estimated_context_tokens(), Some(1000));
}

#[test]
fn test_estimated_context_tokens_openai_with_cached_tokens() {
    // OpenAI API: prompt_tokens 已包含 cached_tokens，adapter 层无需额外处理
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(150_000, 10_000, None, Some(120_000)));
    // estimated_context_tokens 只返回 input_tokens = 150K
    assert_eq!(tracker.estimated_context_tokens(), Some(150_000),);
    let pct = tracker.context_usage_percent(200_000).unwrap();
    assert!((pct - 75.0).abs() < 0.01, "应为 75%，实际 {}%", pct);
}

#[test]
fn test_context_usage_percent() {
    let mut tracker = TokenTracker::default();
    // input 已规范化：raw(50000) + cache(12500) + cache(12500) = 75000
    tracker.accumulate(&make_usage(75000, 25000, Some(12500), Some(12500)));
    // estimated_context_tokens 只返回 input_tokens = 75000 → 37.5%
    let pct = tracker.context_usage_percent(200_000).unwrap();
    assert!((pct - 37.5).abs() < 0.01);
}

#[test]
fn test_context_budget_should_auto_compact() {
    let budget = ContextBudget::new(200_000);
    let mut tracker = TokenTracker::default();
    // input=170K → 170K/200K = 85% → 达到 auto-compact 阈值
    tracker.accumulate(&make_usage(170000, 40000, None, None));
    assert!(budget.should_auto_compact(&tracker));
    // input=150K → 150K/200K = 75% < 85%
    let mut tracker2 = TokenTracker::default();
    tracker2.accumulate(&make_usage(150000, 40000, None, None));
    assert!(!budget.should_auto_compact(&tracker2));
}

#[test]
fn test_context_budget_should_warn() {
    let budget = ContextBudget::new(200_000);
    let mut tracker = TokenTracker::default();
    // input=140K → 140K/200K = 70% → 达到警告阈值
    tracker.accumulate(&make_usage(140000, 60000, None, None));
    assert!(budget.should_warn(&tracker));
    // input=110K → 110K/200K = 55% < 70%
    let mut tracker2 = TokenTracker::default();
    tracker2.accumulate(&make_usage(110000, 40000, None, None));
    assert!(!budget.should_warn(&tracker2));
}

#[test]
fn test_context_budget_new_uses_defaults() {
    let budget = ContextBudget::new(128_000);
    assert_eq!(budget.context_window, 128_000);
    assert!((budget.auto_compact_threshold - 0.85).abs() < 0.001);
    assert!((budget.warning_threshold - 0.70).abs() < 0.001);
}

#[test]
fn test_context_budget_with_auto_compact_threshold() {
    let budget = ContextBudget::new(200_000).with_auto_compact_threshold(0.9);
    // input 已规范化：raw(85000) + cache(21250) + cache(21250) = 127500 → 127500 + 42500 = 170K (85%)
    // 90% threshold → 170K/200K = 85% < 90% → should NOT auto-compact
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(127500, 42500, Some(21250), Some(21250)));
    assert!(
        !budget.should_auto_compact(&tracker),
        "85% should not trigger at 90% threshold"
    );
}

#[test]
fn test_context_budget_with_warning_threshold() {
    let budget = ContextBudget::new(200_000).with_warning_threshold(0.5);
    // input 已规范化：raw(60000) + cache(13750) + cache(13750) = 87500 → 87500 + 40000 = 127500 (63.75%)
    // 但用原始 input(60000) 模拟 OpenAI（无 cache_creation）：60000 + 40000 = 100K (50%)
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(100000, 0, None, None));
    assert!(
        budget.should_warn(&tracker),
        "50% should trigger warning at 50% threshold"
    );
}

#[test]
fn test_token_tracker_reset() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(51500, 2000, Some(1000), Some(500)));
    assert!(tracker.llm_call_count > 0);
    tracker.reset();
    assert_eq!(tracker.total_input_tokens, 0);
    assert_eq!(tracker.total_output_tokens, 0);
    assert_eq!(tracker.total_cache_creation_tokens, 0);
    assert_eq!(tracker.total_cache_read_tokens, 0);
    assert!(tracker.last_usage.is_none());
    assert_eq!(tracker.llm_call_count, 0);
}

#[test]
fn test_context_budget_zero_context_window() {
    let budget = ContextBudget::new(0);
    let tracker = TokenTracker::default();
    assert!(!budget.should_warn(&tracker));
    assert!(!budget.should_auto_compact(&tracker));
}

#[test]
fn test_cache_hit_rate_zero_when_no_cache_data() {
    let tracker = TokenTracker::default();
    assert_eq!(tracker.cache_hit_rate(), 0.0);

    // OpenAI 兼容 API：cache 字段为 None
    let mut tracker2 = TokenTracker::default();
    tracker2.accumulate(&make_usage(1000, 500, None, None));
    assert_eq!(tracker2.cache_hit_rate(), 0.0);
}

#[test]
fn test_cache_hit_rate_zero_on_first_creation() {
    // 首次调用仅有 cache_creation，cache_read=0 → 返回 0.0
    // input 已规范化：raw(1000) + cache_creation(5000) + cache_read(0) = 6000
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(6000, 500, Some(5000), Some(0)));
    assert_eq!(tracker.cache_hit_rate(), 0.0, "无 cache hit 应返回 0.0");
}

#[test]
fn test_cache_hit_rate_reflects_latest_call() {
    let mut tracker = TokenTracker::default();
    // 首次调用：无缓存
    tracker.accumulate(&make_usage(10000, 500, None, Some(0)));
    assert_eq!(tracker.cache_hit_rate(), 0.0);

    // 第二次调用：高缓存命中 34230/34820 ≈ 98.3%
    tracker.accumulate(&make_usage(34820, 423, None, Some(34230)));
    let rate = tracker.cache_hit_rate();
    assert!(
        (rate - 34230.0 / 34820.0).abs() < 1e-9,
        "expected ≈98.3%, got {rate}"
    );

    // 第三次调用：低缓存命中
    tracker.accumulate(&make_usage(20000, 1000, None, Some(5000)));
    let rate = tracker.cache_hit_rate();
    assert!(
        (rate - 5000.0 / 20000.0).abs() < 1e-9,
        "expected 25%, got {rate}"
    );
}

#[test]
fn test_cache_hit_rate_none_when_no_cache_field() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(10000, 500, None, None));
    assert_eq!(tracker.cache_hit_rate(), 0.0);
}

#[test]
fn test_cache_hit_rate_after_reset() {
    let mut tracker = TokenTracker::default();
    // input 已规范化：raw(1000) + cache_creation(5000) + cache_read(5000) = 11000
    tracker.accumulate(&make_usage(11000, 500, Some(5000), Some(5000)));
    let rate = tracker.cache_hit_rate();
    assert!((rate - 5000.0 / 11000.0).abs() < 1e-9);

    tracker.reset();
    assert_eq!(tracker.cache_hit_rate(), 0.0, "reset 后应返回 0.0");
}

#[test]
fn test_cache_hit_rate_anthropic_pattern() {
    // Anthropic prompt caching 典型模式：
    // 首次请求写入缓存，后续请求全部命中缓存
    // input 已在 adapter 层规范化（含缓存 token）
    let mut tracker = TokenTracker::default();

    // 首次：创建缓存。input=500+8000+0=8500, cache_read=0 → 0.0
    tracker.accumulate(&make_usage(8500, 200, Some(8000), Some(0)));
    assert_eq!(
        tracker.cache_hit_rate(),
        0.0,
        "首次创建缓存，无 cache hit 应返回 0.0"
    );

    // 后续：全部命中。当次：8000/8500 ≈ 94.12%
    tracker.accumulate(&make_usage(8500, 200, Some(0), Some(8000)));
    let rate = tracker.cache_hit_rate();
    assert!(
        (rate - 8000.0 / 8500.0).abs() < 1e-9,
        "8000 cache_read / 8500 input ≈ 94.12%, got {rate}"
    );

    // 第三次命中：同样是 8000/8500 ≈ 94.12%（当次值，非累计）
    tracker.accumulate(&make_usage(8500, 200, Some(0), Some(8000)));
    let rate = tracker.cache_hit_rate();
    assert!(
        (rate - 8000.0 / 8500.0).abs() < 1e-9,
        "8000 cache_read / 8500 input ≈ 94.12%, got {rate}"
    );
}

#[test]
fn test_cache_hit_rate_openai_pattern() {
    // OpenAI 风格：cache_creation 始终 None，
    // prompt_tokens 已含 cached_tokens，input 已规范化
    let mut tracker = TokenTracker::default();

    // 首次调用：prompt_tokens=10000, cached_tokens=0 → 0.0
    tracker.accumulate(&make_usage(10000, 500, None, Some(0)));
    assert_eq!(tracker.cache_hit_rate(), 0.0, "cache_read=0 应返回 0.0");

    // 第二次调用：prompt_tokens=10000, cached_tokens=8000 → 8000/10000 = 80%
    tracker.accumulate(&make_usage(10000, 500, None, Some(8000)));
    let rate = tracker.cache_hit_rate();
    assert!(
        (rate - 0.8).abs() < 1e-9,
        "8000 cached / 10000 input = 80%, got {rate}"
    );

    // 第三次调用：prompt_tokens=10000, cached_tokens=9500 → 9500/10000 = 95%
    tracker.accumulate(&make_usage(10000, 500, None, Some(9500)));
    let rate = tracker.cache_hit_rate();
    assert!(
        (rate - 0.95).abs() < 1e-9,
        "9500 cached / 10000 input = 95%, got {rate}"
    );
}

#[test]
fn test_context_usage_percent_zero_window() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(100, 50, None, None));
    let pct = tracker.context_usage_percent(0);
    // Division by zero → should return Some(infinity) or handle gracefully
    // The actual behavior is: 150.0 / 0.0 * 100.0 = inf
    assert!(
        pct.is_some(),
        "should return Some even with 0 context window"
    );
}

#[test]
fn test_accumulate_records_request_id() {
    let mut tracker = TokenTracker::default();
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: Some("req_01ABC".to_string()),
    };
    tracker.accumulate(&usage);
    assert_eq!(tracker.last_request_id.as_deref(), Some("req_01ABC"));
}

#[test]
fn test_accumulate_overwrites_request_id() {
    let mut tracker = TokenTracker::default();
    let usage1 = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: Some("req_01ABC".to_string()),
    };
    tracker.accumulate(&usage1);
    let usage2 = TokenUsage {
        input_tokens: 200,
        output_tokens: 80,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: Some("req_02DEF".to_string()),
    };
    tracker.accumulate(&usage2);
    assert_eq!(tracker.last_request_id.as_deref(), Some("req_02DEF"));
}

#[test]
fn test_accumulate_none_request_id() {
    let mut tracker = TokenTracker::default();
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: None,
    };
    tracker.accumulate(&usage);
    assert!(tracker.last_request_id.is_none());
}

#[test]
fn test_reset_clears_request_id() {
    let mut tracker = TokenTracker::default();
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: Some("req_01ABC".to_string()),
    };
    tracker.accumulate(&usage);
    tracker.reset();
    assert!(tracker.last_request_id.is_none());
}

#[test]
fn test_request_record_from_usage() {
    let usage = TokenUsage {
        input_tokens: 8500,
        output_tokens: 200,
        cache_creation_input_tokens: Some(8000),
        cache_read_input_tokens: Some(0),
        request_id: Some("req_01".to_string()),
    };
    let record = RequestRecord::from_usage(&usage);
    assert_eq!(record.input_tokens, 8500);
    assert_eq!(record.output_tokens, 200);
    assert_eq!(record.cache_creation_input_tokens, 8000);
    assert_eq!(record.cache_read_input_tokens, 0);
}

#[test]
fn test_request_record_cache_hit_rate() {
    let record = RequestRecord {
        input_tokens: 8500,
        output_tokens: 200,
        cache_creation_input_tokens: 8000,
        cache_read_input_tokens: 0,
    };
    assert_eq!(record.cache_hit_rate(), 0.0);

    let record2 = RequestRecord {
        input_tokens: 8500,
        output_tokens: 200,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 8000,
    };
    assert!((record2.cache_hit_rate() - 8000.0 / 8500.0).abs() < 1e-9);
}

#[test]
fn test_accumulate_appends_to_history() {
    let mut tracker = TokenTracker::default();
    let u1 = make_usage(100, 50, Some(30), Some(20));
    let u2 = make_usage(200, 80, Some(10), Some(40));
    tracker.accumulate(&u1);
    tracker.accumulate(&u2);
    assert_eq!(tracker.request_history.len(), 2);
    assert_eq!(tracker.request_history[0].input_tokens, 100);
    assert_eq!(tracker.request_history[1].input_tokens, 200);
    assert_eq!(tracker.request_history[0].cache_read_input_tokens, 20);
}

#[test]
fn test_accumulate_from_usage_with_none_cache() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(100, 50, None, None));
    assert_eq!(tracker.request_history.len(), 1);
    assert_eq!(tracker.request_history[0].cache_creation_input_tokens, 0);
    assert_eq!(tracker.request_history[0].cache_read_input_tokens, 0);
}

#[test]
fn test_reset_clears_history() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(100, 50, Some(30), Some(20)));
    assert_eq!(tracker.request_history.len(), 1);
    tracker.reset();
    assert!(tracker.request_history.is_empty());
}

#[test]
fn test_request_history_capped_at_1000() {
    let mut tracker = TokenTracker::default();
    // 推入 1500 条记录
    for i in 0..1500u32 {
        tracker.accumulate(&make_usage(i, i / 2, None, None));
    }
    // request_history 不应超过 1000 条
    assert_eq!(tracker.request_history.len(), 1000);
    // 保留的应是最新的 1000 条（idx 500..1499）
    assert_eq!(tracker.request_history[0].input_tokens, 500);
    assert_eq!(tracker.request_history[999].input_tokens, 1499);
    // 累计值不受裁剪影响
    let expected_total_input: u64 = (0..1500u64).sum();
    assert_eq!(
        tracker.total_input_tokens, expected_total_input,
        "累计值不受 history 裁剪影响"
    );
    assert_eq!(tracker.llm_call_count, 1500);
    // last_usage 应为最后一次调用
    assert_eq!(tracker.estimated_context_tokens(), Some(1499));
}

// ─── P0-5: 工具结果 token 估算测试 ───────────────────────────────────────

#[test]
fn test_add_estimated_tool_tokens_accumulates_chars_div_4() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(1000, 100, None, None));
    // 初始无工具结果
    assert_eq!(tracker.estimated_tool_tokens_since_last_llm, 0);
    assert_eq!(tracker.estimated_context_tokens(), Some(1000));

    // 400 字符 → 100 token
    tracker.add_estimated_tool_tokens(&"a".repeat(400));
    assert_eq!(tracker.estimated_tool_tokens_since_last_llm, 100);
    // estimated_context_tokens 应包含工具结果估算
    assert_eq!(tracker.estimated_context_tokens(), Some(1100));

    // 再加 800 字符 → +200 token（累计 300）
    tracker.add_estimated_tool_tokens(&"b".repeat(800));
    assert_eq!(tracker.estimated_tool_tokens_since_last_llm, 300);
    assert_eq!(tracker.estimated_context_tokens(), Some(1300));
}

#[test]
fn test_add_estimated_tool_tokens_cjk_uses_chars_not_bytes() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(1000, 100, None, None));
    // 4 个中文字符 = 12 字节 UTF-8，但 chars().count() = 4 → 1 token
    tracker.add_estimated_tool_tokens("你好世界");
    assert_eq!(
        tracker.estimated_tool_tokens_since_last_llm, 1,
        "CJK 应按字符数（4/4=1）而非字节数（12/4=3）"
    );
}

#[test]
fn test_accumulate_clears_estimated_tool_tokens() {
    // 工具结果 token 在下次 LLM accumulate 时已被 input_tokens 包含，必须清零避免双计
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(1000, 100, None, None));
    tracker.add_estimated_tool_tokens(&"x".repeat(400));
    assert_eq!(tracker.estimated_tool_tokens_since_last_llm, 100);

    // 模拟下一轮 LLM 调用——tool_result 已被包含进新的 input_tokens
    tracker.accumulate(&make_usage(1500, 100, None, None));
    assert_eq!(
        tracker.estimated_tool_tokens_since_last_llm, 0,
        "LLM accumulate 后 estimated_tool_tokens 应清零（避免双计）"
    );
    assert_eq!(
        tracker.estimated_context_tokens(),
        Some(1500),
        "estimated_context_tokens 不应包含已清零的工具估算"
    );
}

#[test]
fn test_reset_clears_estimated_tool_tokens() {
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(1000, 100, None, None));
    tracker.add_estimated_tool_tokens(&"x".repeat(400));
    tracker.reset();
    assert_eq!(
        tracker.estimated_tool_tokens_since_last_llm, 0,
        "reset 应清零 estimated_tool_tokens"
    );
}

#[test]
fn test_estimated_tool_tokens_enables_early_compact_warning() {
    // 场景：input=120K（60%），未达 compact 阈值（85%）。
    // 但本轮工具结果注入了 ~800K 字符（~200K token），下一轮将 overflow。
    // 修复后：estimated_context_tokens 应感知到这点，触发 compact。
    let budget = ContextBudget::new(200_000);
    let mut tracker = TokenTracker::default();
    tracker.accumulate(&make_usage(120_000, 100, None, None));
    assert!(
        !budget.should_auto_compact(&tracker),
        "120K/200K = 60% 不应触发 compact"
    );

    // 工具结果 ~800K 字符 ≈ 200K token
    tracker.add_estimated_tool_tokens(&"x".repeat(800_000));
    let estimated = tracker.estimated_context_tokens().unwrap();
    assert!(
        estimated >= 300_000,
        "估算应 ≥ 300K（120K input + 200K 工具估算），实际 {estimated}"
    );
    assert!(
        budget.should_auto_compact(&tracker),
        "工具结果 + input 已超 85%，应触发 compact 预警"
    );
}
