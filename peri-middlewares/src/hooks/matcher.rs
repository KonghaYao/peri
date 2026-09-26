use peri_acp_types::builtin_mcp::original_tool_name_of_effective;
use regex::Regex;

/// 匹配型归一（IF-D6 ⑥ / A4）：`matcher` 与 `tool_name` 两侧都按
/// 「**原样优先，未命中再用原始名**」生成候选名，任一组合命中即算命中。
///
/// 归一表是 [`original_tool_name_of_effective`]（IF-D15 唯一入口）：只有声明表里的
/// builtin 一等工具名字（模型面 effective name）会被换成原始工具名；未命中
/// （未知 / 外部 `mcp__*`）时两侧候选都只有原样 ⇒ 与迁移前的单次比较逐位一致。
/// 名字字面量只在声明表里声明一份，本模块不得复制或反拆。
fn any_candidate_match(
    matcher: &str,
    tool_name: &str,
    matches: impl Fn(&str, &str) -> bool,
) -> bool {
    let matcher_original = original_tool_name_of_effective(matcher);
    let tool_original = original_tool_name_of_effective(tool_name);
    matches(matcher, tool_name)
        || tool_original.is_some_and(|original| matches(matcher, original))
        || matcher_original.is_some_and(|original| matches(original, tool_name))
        || matcher_original
            .zip(tool_original)
            .is_some_and(|(matcher_original, tool_original)| {
                matches(matcher_original, tool_original)
            })
}

/// 粗粒度匹配：matcher 字段
///
/// 支持三种匹配模式：
/// - `"*"` 或空字符串 → 匹配所有
/// - `"Write|Edit"` → 管道分隔的精确匹配列表
/// - `"^Bash.*"` → 正则表达式
/// - `"Write"` → 精确匹配（仅字母数字+下划线时）
///
/// builtin 一等工具的 effective name（`mcp__<实例>__<原始名>`）与裸名 `WebFetch`
/// 两种写法都命中（IF-D6 ⑥ 匹配型归一，见 [`any_candidate_match`]）。
pub fn matches_matcher(matcher: &str, tool_name: &str) -> bool {
    if matcher == "*" || matcher.is_empty() {
        return true;
    }
    any_candidate_match(matcher, tool_name, matches_matcher_name)
}

/// [`matches_matcher`] 的单名匹配本体（逐字保留迁移前的三种模式语义）。
fn matches_matcher_name(matcher: &str, tool_name: &str) -> bool {
    // 管道分隔的精确匹配
    if matcher.contains('|') {
        return matcher.split('|').any(|p| p.trim() == tool_name);
    }
    // 纯字母数字+下划线 → 精确匹配
    if matcher.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return matcher == tool_name;
    }
    // 否则按正则匹配
    Regex::new(matcher)
        .map(|re| re.is_match(tool_name))
        .unwrap_or(false)
}

/// 细粒度匹配：if 条件字段（permission rule 语法）
///
/// 语法：`"{ToolName}({pattern})"`
/// 仅适用于工具事件（PreToolUse / PostToolUse / PostToolUseFailure / PermissionRequest）
///
/// 工具名比较同样走 IF-D6 ⑥ 匹配型归一：条件侧写裸名 `WebFetch(...)` 与写
/// effective name（`mcp__<实例>__<原始名>(...)`）都命中（见 [`any_candidate_match`]）。
pub fn matches_if_condition(
    condition: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> bool {
    // 解析 "Bash(git commit)" → tool_name="Bash", rule="git commit"
    let (cond_tool, cond_rule) = match parse_permission_rule(condition) {
        Some(parsed) => parsed,
        None => return false,
    };

    if !any_candidate_match(&cond_tool, tool_name, |pattern, name| pattern == name) {
        return false;
    }

    if cond_rule.is_empty() {
        return true;
    }

    match_tool_rule(tool_name, tool_input, &cond_rule)
}

/// 解析 permission rule 语法：`"Bash(git commit)"` → `("Bash", "git commit")`
fn parse_permission_rule(rule: &str) -> Option<(String, String)> {
    let open = rule.find('(')?;
    let close = rule.rfind(')')?;
    if close <= open {
        return None;
    }
    let tool_name = rule[..open].trim().to_string();
    let pattern = rule[open + 1..close].trim().to_string();
    Some((tool_name, pattern))
}

/// 基于 tool_input 内容做字符串包含匹配
///
/// 将 tool_input 序列化为 JSON 字符串，检查 rule 是否为子串。
/// 与 Claude Code 行为一致：简单字符串包含匹配。
fn match_tool_rule(_tool_name: &str, tool_input: &serde_json::Value, rule: &str) -> bool {
    let input_str = serde_json::to_string(tool_input).unwrap_or_default();
    input_str.contains(rule)
}

#[cfg(test)]
#[path = "matcher_test.rs"]
mod tests;

/// A4 ⑥（匹配型归一）的专属断言。
///
/// 挂载在实现模块内而非 `matcher_test.rs`：主 plan §4 把 `hooks/` 下**仅**
/// `matcher.rs` 划给 S-01（`hooks/` 其它文件不碰），因此新断言随实现文件走。
#[cfg(test)]
mod parity_tests {
    use super::*;

    /// 声明表里的 effective name（字面量只在 `peri_acp_types::builtin_mcp` 声明一份，
    /// 由该 crate 的 `builtin_mcp_test.rs` 锁定；本文件不复写，以满足
    /// 「消费点不得硬编码 effective name 字面量」的可 grep 事实）。
    fn effective_name(instance: &str, original_name: &str) -> &'static str {
        peri_acp_types::builtin_mcp::find(instance)
            .and_then(|declared| {
                declared
                    .tools
                    .iter()
                    .find(|tool| tool.original_name == original_name)
            })
            .map(|tool| tool.effective_name)
            .expect("builtin 声明表应声明该 (实例, 原始工具名)")
    }

    /// IF-D6 ⑥：用户写裸名与写 effective name 都必须命中（两侧同规则）。
    #[test]
    fn hooks_matcher_accepts_both_naked_and_effective_names() {
        let effective = effective_name("web", "WebFetch");

        assert!(matches_matcher(effective, effective), "原样 × 原样");
        assert!(
            matches_matcher("WebFetch", effective),
            "裸名 matcher × effective 工具名"
        );
        assert!(
            matches_matcher(effective, "WebFetch"),
            "effective matcher × 裸工具名"
        );
        assert!(
            matches_matcher("Write|WebFetch", effective),
            "管道列表中的裸名"
        );
        assert!(
            matches_matcher(&format!("WebSearch|{effective}"), effective),
            "管道列表中的 effective name"
        );
        assert!(
            matches_matcher("^WebFetch$", effective),
            "正则 matcher 的裸名形态"
        );

        // if 条件式：条件侧两种写法都命中
        let input = serde_json::json!({"url": "https://example.com/a"});
        assert!(matches_if_condition(
            "WebFetch(https://example.com)",
            effective,
            &input
        ));
        assert!(matches_if_condition(
            &format!("{effective}(https://example.com)"),
            effective,
            &input
        ));

        // 非迁移对象的判定不因归一改变
        assert!(!matches_matcher("Write", effective));
        assert!(!matches_if_condition("Write(x)", effective, &input));
    }

    /// 反证（IF-D6 冻结约束 3）：未知 / 外部 `mcp__*` 的匹配行为与迁移前逐位一致。
    #[test]
    fn hooks_matcher_unknown_names_behave_exactly_as_before() {
        // 未命中归一表 ⇒ 候选名只有原样，等价于单次比较
        assert_eq!(
            original_tool_name_of_effective("mcp__filesystem__read_file"),
            None
        );
        assert!(matches_matcher(
            "mcp__filesystem__read_file",
            "mcp__filesystem__read_file"
        ));
        assert!(matches_matcher(
            "^mcp__filesystem__.*",
            "mcp__filesystem__read_file"
        ));
        assert!(!matches_matcher("WebFetch", "mcp__filesystem__read_file"));
        assert!(!matches_if_condition(
            "WebFetch(x)",
            "mcp__filesystem__read_file",
            &serde_json::json!({})
        ));

        // 大小写敏感保持：与 effective name 仅差大小写的名字不是同一个名字
        let effective = effective_name("web", "WebFetch");
        assert_eq!(
            original_tool_name_of_effective(&effective.to_lowercase()),
            None
        );
        assert!(!matches_matcher(&effective.to_lowercase(), effective));
        assert!(!matches_matcher("webfetch", "WebFetch"));
    }
}
