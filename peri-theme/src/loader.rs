//! JSON 主题加载器。
//!
//! 支持扁平键路径、`$ref` 引用和 `extends` 继承。继承先合并父主题，
//! 再由子主题覆盖叶子键，最后解析引用，因此父主题引用也能使用子主题的新值。
//! 引用与继承分别最多允许 10 层，并检测当前解析链中的循环。

use std::sync::Arc;

use crate::theme::ThemeDefinition;

mod decode;
mod resolve;
mod source;

/// 主题加载错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ThemeLoadError {
    #[error("theme not found: {0}")]
    ThemeNotFound(String),
    #[error("JSON parse error: {0}")]
    ParseError(String),
    #[error("circular reference: {0}")]
    CircularRef(String),
    #[error("unresolved reference: {0}")]
    UnresolvedRef(String),
    #[error("missing field: {0}")]
    MissingField(String),
    #[error("invalid color: {0}")]
    InvalidColor(String),
}

/// 从用户目录 `~/.peri/themes/` 或内置 JSON 加载主题。
///
/// 用户主题优先于内置主题，`dark` / `light` 是内置主题的别名。
/// 用户文件按 `{name}.json` 查找，保留路径和符号链接的既有支持。
/// JSON 的 `extends` 只接受主题名，不能包含路径组件；继承也按用户目录优先查找。
/// 子主题缺省的 `mode` 继承父主题；缺省的 `name` 仍为 `unnamed`。
pub fn load_theme(name: &str) -> Result<Arc<ThemeDefinition>, ThemeLoadError> {
    source::ThemeSources::from_environment()
        .load(name)
        .map(Arc::new)
}

/// 列出内置主题和用户目录中的 JSON 文件名称，排序并去重；不预先解析文件。
pub fn list_available_themes() -> Vec<String> {
    source::ThemeSources::from_environment().list()
}

#[cfg(test)]
#[path = "loader_test.rs"]
mod tests;
