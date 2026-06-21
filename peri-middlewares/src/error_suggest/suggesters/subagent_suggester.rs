use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::format::did_you_mean_summary;
use crate::error_suggest::matcher::fuzzy_filter;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};

/// C3：subagent_type 不存在建议
pub struct SubagentSuggester;

impl ErrorSuggester for SubagentSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        if ctx.tool_name != "Agent" {
            return None;
        }

        let lower = ctx.error_message.to_lowercase();
        let is_missing = lower.contains("please provide subagent_type");
        let is_unknown = lower.contains("cannot find agent definition");
        if !is_missing && !is_unknown {
            return None;
        }

        // 已知 subagent types 来自 ToolRegistrySnapshot，sort 保证顺序稳定
        let mut known: Vec<String> = ctx.tool_registry.subagent_types.iter().cloned().collect();
        known.sort();

        if is_missing {
            if known.is_empty() {
                return Some(Suggestion::new(
                    "Missing subagent_type parameter. Please provide the agent type explicitly.",
                ));
            }
            let bullet = known
                .iter()
                .map(|s| format!("  • {s}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Some(Suggestion::new(format!(
                "Missing subagent_type parameter. Available values:\n{bullet}"
            )));
        }

        // is_unknown：fuzzy 匹配
        let target = ctx
            .tool_input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let matched = fuzzy_filter(&known, target);
        let top3: Vec<String> = matched.into_iter().take(3).collect();

        if top3.is_empty() {
            let bullet = known
                .iter()
                .map(|s| format!("  • {s}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Some(Suggestion::new(format!(
                "No matching subagent found. Known types:\n{bullet}"
            )));
        }

        Some(Suggestion::new(did_you_mean_summary(
            "subagent_type",
            &top3,
        )))
    }
}
