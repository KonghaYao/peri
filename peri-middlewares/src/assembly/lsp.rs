//! LSP 配置合并与 host 级 pool 构造、生产槽位挂载。
//!
//! 单 pool 原则（A11/A22）：host 级 pool 由 [`create_host_lsp_pool`] 单次构造，
//! 同时承载 `lsp` builtin 实例的工具面与链上同步中间件；[`add_lsp`] 只消费
//! `AssemblyContext::lsp_pool` 投影进来的端口，不建第二份 pool，也不提供
//! session 级构造入口（H-04 已删除过渡 helper `create_session_lsp_pool`）。
use super::AssemblyContext;
use crate::LspSyncMiddleware;
use peri_agent::middleware::chain::MiddlewareChain;
use peri_resources::lsp::{config::LspConfigFile, pool::LspServerPool};
use std::{collections::HashMap, path::Path, sync::Arc};

/// 加载全局 LSP 配置（settings.json 的 `config.lspServers`）并与插件 LSP
/// 服务器合并，返回装配用服务器列表。
///
/// 合并优先级对齐 MCP 三层合并（`crate::mcp::config::load_merged_config_full`）：
/// global < plugin——同名 key 插件覆盖全局（插件名带 `plugin:{name}:{server}`
/// 前缀，实际冲突面小，覆盖方向仍与 MCP 一致）。source 标记与 `${VAR}`
/// 展开由加载/构造侧完成（`load_global_lsp_config` / `lsp_config_from_plugin`），
/// 此处只做合并。无任何配置时返回空 Vec——装配处
/// `lsp_servers.is_empty()` 条件注册语义不变。
///
/// H5：宿主装配（TUI/print 经 `assemble_server_config`、stdio 经
/// `init_stdio_context`）经此函数接入全局配置；此前宿主只取插件
/// lsp_servers，无插件时 LSP 整条产品线静默不可用。
pub fn load_merged_lsp_servers(
    settings_json_path: &Path,
    plugin_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
) -> Vec<peri_acp_types::lsp::LspServerConfig> {
    let global = peri_resources::lsp::config::load_global_lsp_config(settings_json_path);
    let mut merged: HashMap<String, peri_acp_types::lsp::LspServerConfig> = global.lsp_servers;
    for server in plugin_servers {
        merged.insert(server.name.clone(), server);
    }
    merged.into_values().collect()
}

/// 构造 **host 级唯一** LSP 服务器池（A11/A22 单 pool 原则，H-03）。
///
/// 宿主装配（`peri-acp`）经 `peri_middlewares::assembly` 的再导出调用本函数
/// 构造一份 pool，同时以同一 `Arc` 喂给 builtin `lsp` 实例的
/// `LspInstanceInput`（`BuiltinInstanceContext`）与 `AssemblyContext::lsp_pool`
/// 投影——A33：宿主不得 import `crate::mcp::builtin`，故工厂放在本模块。
///
/// 三条冻结事实：
/// - **单次构造、root = host cwd**：`LspServerPool::new` 的 `root_uri` 取自
///   传入的 host cwd；多 cwd / 多 session 共享同一 host root 是**已裁决的
///   功能退化**（A11/A22），wave 3 经 `ToolContext` 恢复 per-session cwd 后再按
///   session 区分。
/// - **惰性**：`LspServerPool::new` 只登记配置表（构造 `LspClient` 不拉起
///   language server 进程），任何进程都由首个按需请求触发。
/// - **无条件返回**：空配置也返回 pool（`has_servers()` 为假 ⇒ `lsp` 实例
///   工具面为空表但仍 ready，A6/A21）。不得用「返回 None / 不构造」表达
///   「无配置」——那会让实例退化成「上下文缺失」而不是「可见但空」。
pub fn create_host_lsp_pool(
    cwd: &str,
    configs: &[peri_acp_types::lsp::LspServerConfig],
) -> Arc<LspServerPool> {
    let lsp_config = LspConfigFile {
        lsp_servers: configs
            .iter()
            .map(|s| (s.name.clone(), s.clone()))
            .collect(),
    };
    Arc::new(LspServerPool::new(cwd, lsp_config))
}

pub(super) fn add_lsp(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext {
        lsp_servers,
        lsp_pool,
        ..
    } = ctx;
    if lsp_servers.is_empty() {
        return;
    }
    // 单 pool 原则（A11/A22）：pool 由宿主装配经 [`create_host_lsp_pool`] 单次构造，
    // 经 `AssemblyContext::lsp_pool` 投影进链。本函数只把该端口交给
    // `LspSyncMiddleware`——不建第二份 pool、不 downcast 还原具体类型
    // （端口即消费面，A23/A30）。
    let Some(port) = lsp_pool else {
        tracing::debug!(
            target: "lsp",
            servers = lsp_servers.len(),
            "无 host pool ⇒ 不装同步中间件"
        );
        return;
    };
    chain.add(Box::new(LspSyncMiddleware::new(Arc::clone(port))));
}
