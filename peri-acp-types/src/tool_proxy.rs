//! 宿主工具代理（ToolSearch / ToolInvoke）的名称与字段契约。
//!
//! 模型面只公布 canonical 名；旧名只作为 resolver alias 保留。常量下沉到契约层，
//! 避免 `peri-agent` / `peri-middlewares` 各自维护一份字面量。

/// 搜索代理的 canonical 模型面名称。
pub const TOOL_SEARCH_NAME: &str = "ToolSearch";
/// 调用代理的 canonical 模型面名称。
pub const TOOL_INVOKE_NAME: &str = "ToolInvoke";
/// 搜索代理的迁移期旧名（仅 alias，不进入模型 schema）。
pub const SEARCH_EXTRA_TOOLS_NAME: &str = "SearchExtraTools";
/// 调用代理的迁移期旧名（仅 alias，不进入模型 schema）。
pub const EXECUTE_EXTRA_TOOL_NAME: &str = "ExecuteExtraTool";

/// 搜索代理的名称集合（canonical + 迁移期 alias）。
pub const SEARCH_PROXY_NAMES: &[&str] = &[TOOL_SEARCH_NAME, SEARCH_EXTRA_TOOLS_NAME];
/// 调用代理的名称集合（canonical + 迁移期 alias）。
pub const INVOKE_PROXY_NAMES: &[&str] = &[TOOL_INVOKE_NAME, EXECUTE_EXTRA_TOOL_NAME];

/// canonical 调用代理的外层字段：目标工具名。
pub const PROXY_NAME_FIELD: &str = "name";
/// canonical 调用代理的外层字段：目标工具参数。
pub const PROXY_ARGUMENTS_FIELD: &str = "arguments";
/// 迁移期调用代理的外层字段：目标工具名。
pub const LEGACY_PROXY_NAME_FIELD: &str = "tool_name";
/// 迁移期调用代理的外层字段：目标工具参数。
pub const LEGACY_PROXY_ARGUMENTS_FIELD: &str = "params";

/// 名称是否属于搜索代理（含迁移期 alias，大小写不敏感）。
#[must_use]
pub fn is_search_proxy_name(name: &str) -> bool {
    SEARCH_PROXY_NAMES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// 名称是否属于调用代理（含迁移期 alias，大小写不敏感）。
#[must_use]
pub fn is_invoke_proxy_name(name: &str) -> bool {
    INVOKE_PROXY_NAMES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// 名称是否属于宿主工具代理（搜索或调用，含迁移期 alias）。
#[must_use]
pub fn is_host_proxy_tool_name(name: &str) -> bool {
    is_search_proxy_name(name) || is_invoke_proxy_name(name)
}

/// 工具对象是否为宿主调用代理：名字命中代理集合，且身份是宿主 meta 工具。
///
/// 外部 MCP 同名工具没有宿主身份，不得按代理处理（否则会丢 settled render 或
/// 被错误地禁止直接调用）。
#[must_use]
pub fn is_host_invoke_proxy(tool: &dyn crate::tools::BaseTool) -> bool {
    is_invoke_proxy_name(tool.name())
        && tool.mcp_server_name().is_none()
        && tool.namespace() == Some("meta")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_name_sets_are_unique_and_cover_legacy_aliases() {
        assert!(is_search_proxy_name(TOOL_SEARCH_NAME));
        assert!(is_search_proxy_name(SEARCH_EXTRA_TOOLS_NAME));
        assert!(is_invoke_proxy_name(TOOL_INVOKE_NAME));
        assert!(is_invoke_proxy_name(EXECUTE_EXTRA_TOOL_NAME));
        assert!(is_host_proxy_tool_name("toolinvoke"));
        assert!(!is_host_proxy_tool_name("Read"));
        assert_eq!(
            SEARCH_PROXY_NAMES
                .iter()
                .filter(|name| **name == TOOL_SEARCH_NAME)
                .count(),
            1
        );
        assert_eq!(
            INVOKE_PROXY_NAMES
                .iter()
                .filter(|name| **name == TOOL_INVOKE_NAME)
                .count(),
            1
        );
    }
}
