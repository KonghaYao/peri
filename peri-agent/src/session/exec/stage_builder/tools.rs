//! Session-local 工具视图与动态目录注册；不写宿主共享表。
use super::{StageBuildError, StageBuildInput};
use crate::{
    agent::stages::SharedToolMap,
    session::tool_catalog::{CatalogRefreshError, SessionToolCatalog, StartupCatalogRegistration},
    tools::BaseTool,
};
use parking_lot::RwLock;
use peri_acp_types::{
    dynamic_mcp::{DynamicMcpCatalogTool, DynamicMcpErrorCode},
    ports::DynamicMcpDeploymentPort,
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

/// 构造 session/turn 级工具视图（MetaHarness 设计 §2.5 关闭语义防御面）。
///
/// 基础 `shared_tools` 是宿主级共享 registry（2026-08-15 职责拆分后生产
/// 路径写入点归零，见 `MIDDLEWARE_TOOL_NAMES` 注释的事实核查更新；
/// middleware 工具从不写入——只经 `chain.collect_tools()` 进入本函数产出
/// 的每 turn 本地视图）。本函数从基础表复制时剔除"middleware 静态工具名
/// 且不在当前链工具集合"的条目，再 merge 当前链工具：disabled session
/// 的本地视图不得看到残留的 middleware 工具，enabled session 视图不受
/// 影响。动态 MCP bridge 工具（`mcp__{server}__{tool}`）不进入共享
/// registry，无需剔除。
pub(super) fn build_session_tool_view(
    shared_tools: &RwLock<BTreeMap<String, Arc<dyn BaseTool>>>,
    middleware_tools: Vec<Box<dyn BaseTool>>,
) -> SharedToolMap {
    let live_names: HashSet<&str> = middleware_tools.iter().map(|t| t.name()).collect();
    let mut local: BTreeMap<String, Arc<dyn BaseTool>> = shared_tools
        .read()
        .iter()
        .filter(|(name, _)| {
            // 剔除按名匹配（工具对象不携带来源信息）：非 middleware 来源的
            // 同名工具（如 plugin/外部注册的 "Bash"）在对应 middleware 关闭
            // 时也会被剔出本地视图——保守方向（宁可误伤不可泄漏），
            // `live_names` 保护当前链注册的同名工具。
            !peri_acp_types::meta_harness::MIDDLEWARE_TOOL_NAMES.contains(&name.as_str())
                || live_names.contains(name.as_str())
        })
        .map(|(k, v)| (k.clone(), Arc::clone(v)))
        .collect();
    for tool in middleware_tools {
        let arc: Arc<dyn BaseTool> = Arc::from(tool);
        // 使用 insert：有状态工具（如 SubAgentTool）需每 turn 更新。
        local.insert(arc.name().to_string(), arc);
    }
    Arc::new(RwLock::new(local))
}

pub(super) fn register_tool_catalog(
    input: &StageBuildInput,
    session_tools: &SharedToolMap,
) -> Result<Arc<SessionToolCatalog>, StageBuildError> {
    let tool_catalog = Arc::new(SessionToolCatalog::try_new(
        session_tools.read().clone(),
        input.session_mcp_capability.clone(),
    )?);
    if let Some(deployment) = input.dynamic_mcp.as_ref() {
        deployment
            .register_catalog(&input.session_id, tool_catalog.dynamic_catalog_tools())
            .map_err(StageBuildError::DynamicMcp)?;
        // 启动闸门提交晚到的静态 MCP 工具后，碰撞目录必须随新 base 重验：注册
        // 回调缺省不设置（子 agent 沿用自身 capability，不改父 session 目录）。
        tool_catalog.set_startup_catalog_registration(catalog_registration(
            Arc::clone(deployment),
            input.session_id.clone(),
        ));
    }

    Ok(tool_catalog)
}

/// 启动提交的碰撞目录注册回调：捕获 deployment 与 session_id，把 registry 的
/// 拒绝翻译为目录发布错误。
///
/// registry 在重名/别名冲突时以冲突工具名作为 `safe_summary`
/// （见 `mcp/dynamic/registry.rs::candidate_catalog_conflict`），因此可以无损
/// 映射到 `StartupRegistrationRejected`；其余拒绝（会话/任务已关闭）不是工具
/// 冲突，不作为「某个工具名与目录冲突」上报。
fn catalog_registration(
    deployment: Arc<dyn DynamicMcpDeploymentPort>,
    session_id: String,
) -> StartupCatalogRegistration {
    Arc::new(move |tools: Vec<DynamicMcpCatalogTool>| {
        deployment
            .register_catalog(&session_id, tools)
            .map_err(|failure| match failure.code {
                DynamicMcpErrorCode::ToolNameConflict => {
                    CatalogRefreshError::StartupRegistrationRejected {
                        tool: failure.safe_summary,
                    }
                }
                _ => CatalogRefreshError::InconsistentCapability,
            })
    })
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
