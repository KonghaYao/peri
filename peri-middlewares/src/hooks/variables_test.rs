use std::path::PathBuf;

use super::*;

fn plugin_root() -> PathBuf {
    PathBuf::from("/tmp/plugin")
}

fn plugin_data() -> PathBuf {
    PathBuf::from("/tmp/data")
}

#[test]
fn test_basic_plugin_root_replacement() {
    let result =
        resolve_hook_variables("echo ${CLAUDE_PLUGIN_ROOT}", &plugin_root(), &plugin_data());
    assert_eq!(result, "echo /tmp/plugin");
}

#[test]
fn test_dollar_plugin_root_replacement() {
    let result = resolve_hook_variables("echo $CLAUDE_PLUGIN_ROOT", &plugin_root(), &plugin_data());
    assert_eq!(result, "echo /tmp/plugin");
}

#[test]
fn test_multi_variable_replacement() {
    let result = resolve_hook_variables(
        "${CLAUDE_PLUGIN_ROOT}/${CLAUDE_PLUGIN_DATA}",
        &plugin_root(),
        &plugin_data(),
    );
    assert_eq!(result, "/tmp/plugin//tmp/data");
}

#[test]
fn test_arguments_token_is_left_untouched() {
    // H4：命令文本中的 $ARGUMENTS / ${ARGUMENTS} 不再做任何替换，
    // 数据只走 stdin JSON；迁移检测在 executor 层负责报错。
    let input = "echo $ARGUMENTS ${ARGUMENTS}";
    let result = resolve_hook_variables(input, &plugin_root(), &plugin_data());
    assert_eq!(result, input);
}

#[test]
fn test_empty_input() {
    let result = resolve_hook_variables("", &plugin_root(), &plugin_data());
    assert_eq!(result, "");
}

#[test]
fn test_no_variables() {
    let input = "bash -c 'echo hello'";
    let result = resolve_hook_variables(input, &plugin_root(), &plugin_data());
    assert_eq!(result, input);
}

#[test]
fn test_windows_path_format() {
    // On non-Windows, just verify the path is passed through
    let root = PathBuf::from("/tmp/plugin");
    let result = resolve_hook_variables("${CLAUDE_PLUGIN_ROOT}", &root, &plugin_data());
    assert_eq!(result, "/tmp/plugin");
}

// === env var tests ===

#[test]
fn test_env_var_allowed() {
    std::env::set_var("TEST_HOOK_API_KEY_FOR_TEST", "sk-xxx");
    let allowed: HashSet<String> = ["TEST_HOOK_API_KEY_FOR_TEST".to_string()]
        .into_iter()
        .collect();
    let result = resolve_hook_variables_with_env(
        "Token: ${TEST_HOOK_API_KEY_FOR_TEST}",
        &plugin_root(),
        &plugin_data(),
        &allowed,
    );
    assert_eq!(result, "Token: sk-xxx");
    std::env::remove_var("TEST_HOOK_API_KEY_FOR_TEST");
}

#[test]
fn test_env_var_not_allowed() {
    let allowed: HashSet<String> = ["API_KEY".to_string()].into_iter().collect();
    let result =
        resolve_hook_variables_with_env("${SECRET_KEY}", &plugin_root(), &plugin_data(), &allowed);
    // shellexpand will fail to expand, returns original string
    assert_eq!(result, "${SECRET_KEY}");
}

#[test]
fn test_mixed_replacement() {
    std::env::set_var("TEST_HOOK_HOME_FOR_TEST", "/home/user");
    let allowed: HashSet<String> = ["TEST_HOOK_HOME_FOR_TEST".to_string()]
        .into_iter()
        .collect();
    let result = resolve_hook_variables_with_env(
        "${CLAUDE_PLUGIN_ROOT}/${TEST_HOOK_HOME_FOR_TEST}",
        &plugin_root(),
        &plugin_data(),
        &allowed,
    );
    assert_eq!(result, "/tmp/plugin//home/user");
    std::env::remove_var("TEST_HOOK_HOME_FOR_TEST");
}

#[test]
fn test_undefined_env_var() {
    let allowed: HashSet<String> = ["UNDEFINED_HOOK_TEST_VAR".to_string()]
        .into_iter()
        .collect();
    let result = resolve_hook_variables_with_env(
        "$UNDEFINED_HOOK_TEST_VAR",
        &plugin_root(),
        &plugin_data(),
        &allowed,
    );
    // shellexpand resolves to empty string for undefined vars
    assert_eq!(result, "");
}
