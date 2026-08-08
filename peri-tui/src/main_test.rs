//! Tests for main

#[cfg(test)]
use super::*;

fn make_temp_file(content: &str) -> tempfile::TempPath {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(content.as_bytes()).unwrap();
    file.into_temp_path()
}

#[test]
fn test_inject_from_config_env() {
    // 测试 config.env 标准格式
    let path = make_temp_file(r#"{"config": {"env": {"TEST_C1": "v1"}}}"#);
    inject_env_from_file(&path, &[&["config", "env"]]);
    assert_eq!(std::env::var("TEST_C1").unwrap(), "v1");
    unsafe {
        std::env::remove_var("TEST_C1");
    }
}

#[test]
fn test_inject_from_top_level_env() {
    // 测试顶层 env 格式（兼容旧格式/Claude Code 格式）
    let path = make_temp_file(r#"{"env": {"TEST_T1": "v2"}}"#);
    inject_env_from_file(&path, &[&["env"]]);
    assert_eq!(std::env::var("TEST_T1").unwrap(), "v2");
    unsafe {
        std::env::remove_var("TEST_T1");
    }
}

#[test]
fn test_inject_fallback_order() {
    // 测试优先 config.env 再回退顶层 env
    // 只存在顶层 env 时应该回退成功
    let path = make_temp_file(r#"{"env": {"TEST_FB1": "from_fallback"}}"#);
    inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
    assert_eq!(std::env::var("TEST_FB1").unwrap(), "from_fallback");
    unsafe {
        std::env::remove_var("TEST_FB1");
    }
}

#[test]
fn test_inject_config_env_priority_over_top_level() {
    // config.env 存在时优先使用，不回退到顶层 env
    let path = make_temp_file(
        r#"{"config": {"env": {"TEST_PRI": "from_config"}}, "env": {"TEST_PRI": "from_top"}}"#,
    );
    inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
    assert_eq!(std::env::var("TEST_PRI").unwrap(), "from_config");
    unsafe {
        std::env::remove_var("TEST_PRI");
    }
}

#[test]
fn test_process_env_priority() {
    // 进程环境变量存在时不被 settings.json 覆盖
    unsafe {
        std::env::set_var("TEST_PROC_PRI", "from_process");
    }
    let path = make_temp_file(r#"{"env": {"TEST_PROC_PRI": "from_file"}}"#);
    inject_env_from_file(&path, &[&["env"]]);
    assert_eq!(std::env::var("TEST_PROC_PRI").unwrap(), "from_process");
    unsafe {
        std::env::remove_var("TEST_PROC_PRI");
    }
}

#[test]
fn test_skip_non_string_values() {
    // 非字符串值应跳过不 panic
    let path = make_temp_file(r#"{"env": {"TEST_NUM": 123, "TEST_STR": "ok"}}"#);
    inject_env_from_file(&path, &[&["env"]]);
    // 数字值不应被注入
    assert!(std::env::var("TEST_NUM").is_err());
    assert_eq!(std::env::var("TEST_STR").unwrap(), "ok");
    unsafe {
        std::env::remove_var("TEST_STR");
    }
}

#[test]
fn test_no_file_no_panic() {
    // 文件不存在时不应 panic
    let path = std::path::PathBuf::from("/nonexistent/path/settings.json");
    inject_env_from_file(&path, &[&["env"]]);
}

#[test]
fn test_no_env_field_no_panic() {
    // JSON 中没有 env 字段时不应 panic
    let path = make_temp_file(r#"{"other": "data"}"#);
    inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
}

/// 端到端测试：模拟顶层 env 格式 → 注入进程环境 → LlmProvider::from_env() 可用
#[test]
fn test_e2e_top_level_env_to_provider() {
    // 保存可能被覆盖的环境变量
    let save_keys = ["TEST_E2E_API_KEY", "TEST_E2E_BASE_URL", "MODEL_PROVIDER"];
    let saved: Vec<(&str, Option<String>)> = save_keys
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();

    // 创建一个顶层 env 格式的配置文件（模拟当前 ~/.peri/settings.json 的格式）
    let path = make_temp_file(
        r#"{"env": {"TEST_E2E_API_KEY": "sk-e2e-test-key", "TEST_E2E_BASE_URL": "https://e2e-test.example.com/v1"}}"#,
    );

    // 调用注入函数（使用 inject_env_from_settings 相同的查找策略）
    inject_env_from_file(&path, &[&["config", "env"], &["env"]]);

    // 验证环境变量已注入
    assert_eq!(
        std::env::var("TEST_E2E_API_KEY").unwrap(),
        "sk-e2e-test-key"
    );
    assert_eq!(
        std::env::var("TEST_E2E_BASE_URL").unwrap(),
        "https://e2e-test.example.com/v1"
    );

    // 清理测试环境变量
    unsafe {
        std::env::remove_var("TEST_E2E_API_KEY");
    }
    unsafe {
        std::env::remove_var("TEST_E2E_BASE_URL");
    }

    // 恢复之前保存的环境变量
    for (key, value) in saved {
        match value {
            Some(v) => unsafe {
                std::env::set_var(key, v);
            },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

/// [回归测试] 验证项目本地 ./.peri/settings.json 的 config.env 能被正常注入。
/// 修复：2026-07-24，此前 inject_env_from_settings 仅读取 ~/.peri/settings.json。
#[test]
fn test_project_local_settings_env_injection() {
    // 模拟项目本地 .peri/settings.json 的 config.env 标准格式
    let path = make_temp_file(r#"{"config": {"env": {"PRJ_LOCAL_KEY": "from_project"}}}"#);
    inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
    assert_eq!(std::env::var("PRJ_LOCAL_KEY").unwrap(), "from_project");
    unsafe {
        std::env::remove_var("PRJ_LOCAL_KEY");
    }
}

// ─── pre_scan_config_file（Slice C1，gate 决策 Option A）─────────────────────

#[test]
fn test_prescan_config_file_space_form() {
    let args = ["--config-file", "/tmp/cfg.json"].map(std::ffi::OsString::from);
    assert_eq!(
        pre_scan_config_file(args.into_iter()),
        Some(PathBuf::from("/tmp/cfg.json"))
    );
}

#[test]
fn test_prescan_config_file_equals_form() {
    let args = ["--config-file=/tmp/cfg.json"].map(std::ffi::OsString::from);
    assert_eq!(
        pre_scan_config_file(args.into_iter()),
        Some(PathBuf::from("/tmp/cfg.json"))
    );
}

#[test]
fn test_prescan_config_file_camel_alias() {
    let args = ["--configFile", "/tmp/cfg.json"].map(std::ffi::OsString::from);
    assert_eq!(
        pre_scan_config_file(args.into_iter()),
        Some(PathBuf::from("/tmp/cfg.json"))
    );
}

#[test]
fn test_prescan_config_file_camel_equals_form() {
    let args = ["--configFile=/tmp/cfg.json"].map(std::ffi::OsString::from);
    assert_eq!(
        pre_scan_config_file(args.into_iter()),
        Some(PathBuf::from("/tmp/cfg.json"))
    );
}

/// 构造一个非 UTF-8 的 OsString（`to_str()` 返回 None），按平台区分：
/// - Unix：OsString 是任意字节序列，直接嵌入非法 UTF-8 字节
/// - Windows：OsString 是 WTF-16，孤立 surrogate（0xD800）合法但非 UTF-8
#[cfg(unix)]
fn non_utf8_os_string() -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(vec![0x2f, 0x74, 0x6d, 0x70, 0xff, 0xfe])
}

#[cfg(windows)]
fn non_utf8_os_string() -> std::ffi::OsString {
    use std::os::windows::ffi::OsStringExt;
    std::ffi::OsString::from_wide(&[0x2f, 0x74, 0x6d, 0x70, 0xd800])
}

#[test]
fn test_prescan_config_file_non_utf8_value() {
    // 非 UTF-8 值直接构造 PathBuf，保留原始字节
    let value = non_utf8_os_string();
    let result = pre_scan_config_file(
        [std::ffi::OsString::from("--config-file"), value.clone()].into_iter(),
    )
    .unwrap();
    assert_eq!(result.as_os_str(), value.as_os_str());
}

#[test]
fn test_prescan_config_file_missing_value_none() {
    // 下一 token 是 option-like（--db-path）→ 缺值，fail-open 返回 None 交给 clap 报错
    let args = ["--config-file", "--db-path"].map(std::ffi::OsString::from);
    assert_eq!(pre_scan_config_file(args.into_iter()), None);
}

#[test]
fn test_prescan_config_file_followed_by_short_flag_none() {
    // M2 增补：后跟短 flag（-p）→ 缺值 → None
    let args = ["--config-file", "-p", "x"].map(std::ffi::OsString::from);
    assert_eq!(pre_scan_config_file(args.into_iter()), None);
}

#[test]
fn test_prescan_config_file_followed_by_db_flag_none() {
    // M2 增补：后跟 --db-path → 缺值 → None
    let args = ["--config-file", "--db-path", "/x"].map(std::ffi::OsString::from);
    assert_eq!(pre_scan_config_file(args.into_iter()), None);
}

#[test]
fn test_prescan_stops_after_double_dash() {
    // `--` 之后停止扫描，`--config-file` 不再被识别
    let args = ["--", "--config-file", "x"].map(std::ffi::OsString::from);
    assert_eq!(pre_scan_config_file(args.into_iter()), None);
}

#[test]
fn test_prescan_last_occurrence_wins() {
    // 重复 flag 取最后一次（last-wins）
    let args = ["--config-file", "a", "--config-file", "b"].map(std::ffi::OsString::from);
    assert_eq!(
        pre_scan_config_file(args.into_iter()),
        Some(PathBuf::from("b"))
    );
}
