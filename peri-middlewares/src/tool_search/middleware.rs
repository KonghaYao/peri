//! ToolSearchMiddleware — 注册元工具并注入延迟工具列表到 system prompt

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock as StdRwLock},
};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    tools::BaseTool,
};

use super::{
    declaration::collect_declarations, execute_tool::ExecuteExtraTool,
    search_tool::SearchExtraTools, tool_index::ToolSearchIndex,
};

/// ToolSearch 中间件
///
/// 职责：
/// 1. 注册 SearchExtraTools 和 ExecuteExtraTool 两个元工具
/// 2. 在 before_agent 时注入延迟工具列表到 system prompt
pub struct ToolSearchMiddleware {
    tool_search_index: Arc<ToolSearchIndex>,
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// Cached prompt contribution (populated in before_agent, returned by prompt_contribution).
    cached_contribution: Arc<StdRwLock<Option<String>>>,
    /// 当前索引绑定的 deferred snapshot 指纹。
    deferred_fingerprint: Arc<StdRwLock<Option<String>>>,
}

impl ToolSearchMiddleware {
    pub fn new(
        tool_search_index: Arc<ToolSearchIndex>,
        shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    ) -> Self {
        Self {
            tool_search_index,
            shared_tools,
            cached_contribution: Arc::new(StdRwLock::new(None)),
            deferred_fingerprint: Arc::new(StdRwLock::new(None)),
        }
    }

    fn deferred_fingerprint(tools: &[Arc<dyn BaseTool>]) -> String {
        serde_json::to_string(
            &tools
                .iter()
                .map(|tool| (tool.name(), tool.description(), tool.parameters()))
                .collect::<Vec<_>>(),
        )
        .expect("tool snapshot fingerprint must serialize")
    }
}

#[async_trait]
impl Middleware for ToolSearchMiddleware {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![
            Box::new(SearchExtraTools::new(Arc::clone(&self.tool_search_index))),
            Box::new(ExecuteExtraTool::new(Arc::clone(&self.shared_tools))),
        ]
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution.read().unwrap().clone()
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 优先读取 v2 每 turn 本地工具视图（stage_builder 构建，含当前链全部
        // 工具）；无本地视图时回退宿主级 shared_tools（v1 / 测试路径）。
        // 背景：宿主级 shared_tools 生产路径写入点归零后恒为空表，仅读它会
        // 导致 deferred 索引永不构建（issue 2026-08-15-workflow-deferred-
        // tool-missing）。
        // 一次加锁同时收集 deferred（搜索索引面）与 direct（LLM 可见面，
        // 声明段数据源，design v2 §2.5.2）两个集合。
        let deferred_arcs: Vec<Arc<dyn BaseTool>>;
        let direct_arcs: Vec<Arc<dyn BaseTool>>;
        {
            let local = state.local_tools();
            let mut guard = match local {
                Some(tools) => tools.write(),
                None => self.shared_tools.write(),
            };
            let direct_names: Vec<String> = guard
                .iter()
                .filter(|(_, tool)| tool.is_direct())
                .map(|(name, _)| name.clone())
                .collect();
            if guard.contains_key(super::core_tools::SEARCH_EXTRA_TOOLS_NAME) {
                guard.insert(
                    super::core_tools::SEARCH_EXTRA_TOOLS_NAME.to_string(),
                    Arc::new(SearchExtraTools::with_direct_tools(
                        Arc::clone(&self.tool_search_index),
                        direct_names.iter().map(String::as_str),
                    )),
                );
            }
            if guard.contains_key(super::core_tools::EXECUTE_EXTRA_TOOL_NAME) {
                guard.insert(
                    super::core_tools::EXECUTE_EXTRA_TOOL_NAME.to_string(),
                    Arc::new(ExecuteExtraTool::with_direct_tools(
                        Arc::clone(&self.shared_tools),
                        direct_names.iter().map(String::as_str),
                    )),
                );
            }
            deferred_arcs = guard
                .iter()
                .filter(|(_, tool)| !tool.is_direct())
                .map(|(_, tool)| Arc::clone(tool))
                .collect();
            direct_arcs = guard
                .iter()
                .filter(|(_, tool)| tool.is_direct())
                .map(|(_, tool)| Arc::clone(tool))
                .collect();
        }

        let fingerprint = Self::deferred_fingerprint(&deferred_arcs);
        let should_rebuild =
            self.deferred_fingerprint.read().unwrap().as_ref() != Some(&fingerprint);

        if should_rebuild {
            let old_count = self.tool_search_index.total_count();
            self.tool_search_index.build(deferred_arcs);
            *self.deferred_fingerprint.write().unwrap() = Some(fingerprint);

            let new_count = self.tool_search_index.total_count();
            if old_count > 0 && new_count != old_count {
                state.push_recall(format!(
                    "[ToolSearch] Deferred tools updated: {} tools available (was {})",
                    new_count, old_count
                ));
            }
            let list = self.tool_search_index.format_deferred_list();
            if list.is_empty() {
                self.tool_search_index.clear_cached_prompt();
            } else {
                self.tool_search_index.set_cached_prompt(list);
            }
        }

        // 缓存 prompt 贡献（由 prompt_contribution 同步返回）。
        // 合并策略（design v2 §2.5.2）：deferred 列表在前、声明段在后，`\n\n` 分隔；
        // 任一段为空时只保留另一段。声明段不走索引 content_version 失效路径——
        // 每轮 before_agent 独立重渲染，输出仅依赖工具静态字段。
        let list = self.tool_search_index.cached_prompt();
        let declarations = collect_declarations(&direct_arcs);
        *self.cached_contribution.write().unwrap() = match (list, declarations) {
            (Some(l), Some(d)) => Some(format!("{l}\n\n{d}")),
            (Some(l), None) => Some(l),
            (None, Some(d)) => Some(d),
            (None, None) => None,
        };
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
