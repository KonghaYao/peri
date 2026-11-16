// Responsibility-specific cases share the original test namespace and fixtures.

// ── 输出截断（truncate_bytes）──

#[test]
fn test_truncate_bytes_ascii() {
    let s = "hello world";
    assert_eq!(
        crate::agent::async_tasks::shell::truncate_bytes(s, 5),
        "hello"
    );
}

#[test]
fn test_truncate_bytes_within_limit() {
    let s = "hello";
    assert_eq!(
        crate::agent::async_tasks::shell::truncate_bytes(s, 100),
        "hello"
    );
}

#[test]
fn test_truncate_bytes_utf8_safe() {
    let s = "你好世界";
    assert_eq!(
        crate::agent::async_tasks::shell::truncate_bytes(s, 6),
        "你好"
    );
}

#[test]
fn test_truncate_bytes_utf8_mid_character() {
    let s = "你好";
    let result = crate::agent::async_tasks::shell::truncate_bytes(s, 5);
    // 5 字节位于 "好"（3 字节）中间，应回退到 "你"（3 字节）
    assert_eq!(result, "你");
}

#[test]
fn test_truncate_bytes_empty_string() {
    assert_eq!(crate::agent::async_tasks::shell::truncate_bytes("", 10), "");
}

#[test]
fn test_truncate_bytes_zero_max() {
    assert_eq!(
        crate::agent::async_tasks::shell::truncate_bytes("hello", 0),
        ""
    );
}

// ── bg shell 执行链纯函数（parse_foreground/background_timeout / bg_shell_task_id）──

/// 前台 timeout 恒有界（禁止无界同步执行）：
/// - 未传 / `null` → 默认 15000
/// - 显式 0 → 前台上限 120000（`0` 不能被解释为"不超时"），原始请求值随结果回传
/// - 显式 >上限 → 上限，原始请求值随结果回传
/// - 其余 → 请求值，按平台下限兜底（Unix 1；Windows 5000）
/// - 整数值浮点（`15000.0`）归一化为整数；小数/负数/字符串/bool/对象/数组显式报错
#[test]
fn test_parse_foreground_timeout_is_always_bounded() {
    let min = if cfg!(target_os = "windows") { 5000 } else { 1 };
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(&serde_json::json!({}))
            .expect("the parse foreground timeout fixture result is required for this assertion"),
        (FOREGROUND_DEFAULT_TIMEOUT_MS, None)
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(
            &serde_json::json!({"timeout": null})
        )
        .expect("the parse foreground timeout fixture result is required for this assertion"),
        (FOREGROUND_DEFAULT_TIMEOUT_MS, None)
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(
            &serde_json::json!({"timeout": 0})
        )
        .expect("the parse foreground timeout fixture result is required for this assertion"),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(0)),
        "timeout: 0 必须界到前台上限，不能禁用超时"
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(
            &serde_json::json!({"timeout": 600_000})
        )
        .expect("the parse foreground timeout fixture result is required for this assertion"),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(600_000))
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(
            &serde_json::json!({"timeout": 2_000_000})
        )
        .expect("the parse foreground timeout fixture result is required for this assertion"),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(2_000_000))
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(
            &serde_json::json!({"timeout": 2000})
        )
        .expect("the parse foreground timeout fixture result is required for this assertion"),
        (2000.max(min), None)
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_foreground_timeout(
            &serde_json::json!({"timeout": 15000.0})
        )
        .expect("the parse foreground timeout fixture result is required for this assertion"),
        (15_000, None),
        "整数值浮点应归一化为整数毫秒"
    );
    for invalid in [
        serde_json::json!({"timeout": "1000"}),
        serde_json::json!({"timeout": -5}),
        serde_json::json!({"timeout": 1.5}),
        serde_json::json!({"timeout": true}),
        serde_json::json!({"timeout": {}}),
        serde_json::json!({"timeout": []}),
    ] {
        let error = crate::agent::async_tasks::shell::parse_foreground_timeout(&invalid)
            .expect_err("invalid timeout must fail");
        let message = error.to_string();
        assert!(
            message.contains("timeout") && message.contains("received"),
            "错误应包含参数名与收到值: {message}"
        );
    }
}

/// 后台 timeout 语义（不阻塞 Agent，允许"不超时"）：
/// - 未传 / `null` / 显式 0 → None；显式 >0 → clamp 到 [min, 600000]
/// - 整数值浮点（`15000.0`）归一化为 15000；小数/负数/字符串显式报错
#[test]
fn test_parse_background_timeout_semantics() {
    let min = if cfg!(target_os = "windows") { 5000 } else { 1 };
    assert_eq!(
        crate::agent::async_tasks::shell::parse_background_timeout(&serde_json::json!({}))
            .expect("the parse background timeout fixture result is required for this assertion"),
        None
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_background_timeout(
            &serde_json::json!({"timeout": null})
        )
        .expect("the parse background timeout fixture result is required for this assertion"),
        None
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_background_timeout(
            &serde_json::json!({"timeout": 0})
        )
        .expect("the parse background timeout fixture result is required for this assertion"),
        None
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_background_timeout(
            &serde_json::json!({"timeout": 2000})
        )
        .expect("the parse background timeout fixture result is required for this assertion"),
        Some(2000.max(min))
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_background_timeout(
            &serde_json::json!({"timeout": 999_999})
        )
        .expect("the parse background timeout fixture result is required for this assertion"),
        Some(BACKGROUND_MAX_TIMEOUT_MS)
    );
    assert_eq!(
        crate::agent::async_tasks::shell::parse_background_timeout(
            &serde_json::json!({"timeout": 15000.0})
        )
        .expect("the parse background timeout fixture result is required for this assertion"),
        Some(15_000)
    );
    for invalid in [
        serde_json::json!({"timeout": 1.5}),
        serde_json::json!({"timeout": -1}),
        serde_json::json!({"timeout": "5"}),
    ] {
        let error = crate::agent::async_tasks::shell::parse_background_timeout(&invalid)
            .expect_err("invalid timeout must fail");
        assert!(
            error.to_string().contains("timeout"),
            "错误应包含参数名: {error}"
        );
    }
}

/// bg shell 任务 id 唯一性（issue 2026-08-05 回归防护）：
/// 旧实现取 UUID v7 前 8 字符（毫秒时间戳高 32 位），同一毫秒内多次调用
/// 必然碰撞 → registry 覆盖注册 → 后续 complete() 被静默跳过 → TUI 条目残留。
/// 连续生成（大概率落在同一毫秒）必须全部唯一，且保留 `shell-` 前缀。
#[test]
fn test_bg_shell_task_id_uniqueness() {
    let ids: std::collections::HashSet<String> = (0..64)
        .map(|_| crate::agent::async_tasks::shell::bg_shell_task_id())
        .collect();
    assert_eq!(ids.len(), 64, "同一毫秒内生成的 bg shell task_id 必须唯一");
    assert!(ids.iter().all(|id| id.starts_with("shell-")));
}
