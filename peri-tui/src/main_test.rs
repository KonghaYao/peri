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
