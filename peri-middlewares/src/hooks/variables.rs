use std::{collections::HashSet, path::Path};

/// 替换插件路径变量
///
/// 支持的变量：
/// - `${CLAUDE_PLUGIN_ROOT}` / `$CLAUDE_PLUGIN_ROOT` → 插件安装路径
/// - `${CLAUDE_PLUGIN_DATA}` / `$CLAUDE_PLUGIN_DATA` → 插件数据路径
///
/// [TRAP] 不再支持 `$ARGUMENTS` / `${ARGUMENTS}` 文本替换（H4 参数隔离）：
/// HookInput 数据只经 stdin JSON 传递，command 字符串里的旧写法由 executor
/// 报迁移错误；这里绝不能悄悄把数据拼回命令文本。
pub fn resolve_hook_variables(input: &str, plugin_root: &Path, plugin_data_dir: &Path) -> String {
    let mut result = input.to_string();

    // 替换 ${CLAUDE_PLUGIN_ROOT} 和 $CLAUDE_PLUGIN_ROOT
    let root_str = path_to_posix(plugin_root);
    result = result.replace("${CLAUDE_PLUGIN_ROOT}", &root_str);
    result = result.replace("$CLAUDE_PLUGIN_ROOT", &root_str);

    // 替换 ${CLAUDE_PLUGIN_DATA} 和 $CLAUDE_PLUGIN_DATA
    let data_str = path_to_posix(plugin_data_dir);
    result = result.replace("${CLAUDE_PLUGIN_DATA}", &data_str);
    result = result.replace("$CLAUDE_PLUGIN_DATA", &data_str);

    result
}

/// 替换变量并增加环境变量白名单替换
///
/// 在 resolve_hook_variables 基础上，额外支持环境变量展开。
/// 仅白名单内的环境变量会被替换，白名单外的保持原样。
pub fn resolve_hook_variables_with_env(
    input: &str,
    plugin_root: &Path,
    plugin_data_dir: &Path,
    allowed_env_vars: &HashSet<String>,
) -> String {
    // 先完成插件路径替换
    let intermediate = resolve_hook_variables(input, plugin_root, plugin_data_dir);

    // 使用 shellexpand 进行 env var 展开，白名单限制
    let allowed = allowed_env_vars.clone();
    match shellexpand::env_with_context::<_, String, _, std::convert::Infallible>(
        &intermediate,
        |var| {
            if allowed.contains(var) {
                Ok(Some(std::env::var(var).unwrap_or_default()))
            } else {
                Ok(None)
            }
        },
    ) {
        Ok(resolved) => resolved.to_string(),
        Err(_) => intermediate, // 展开失败时返回中间结果
    }
}

/// 将路径转换为 POSIX 格式（Windows 上 \ → /）
fn path_to_posix(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        s.replace('\\', "/")
    }
    #[cfg(not(target_os = "windows"))]
    {
        s
    }
}

#[cfg(test)]
#[path = "variables_test.rs"]
mod tests;
