//! LSP 配置合并、会话池构造与生产槽位挂载。
use super::AssemblyContext;
use crate::LspMiddleware;
use peri_acp_types::ports::LspPoolPort;
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

/// 构造会话级 LSP 服务器池并 upcast 端口（装配面宿主 session/new /
/// load / resume / fork 调用；返回类型已锚定端口 trait，调用方无需引用
/// peri-lsp 类型路径）。
///
/// 无服务器配置时返回 None（不注册 LSP 中间件，与装配面
/// `lsp_servers.is_empty()` 条件注册语义一致）。H1：会话级实例跨 turn
/// 复用（服务器进程 / initialized / 诊断状态不丢），宿主退出时经端口
/// `shutdown` 优雅关闭。
pub fn create_session_lsp_pool(
    cwd: &str,
    configs: &[peri_acp_types::lsp::LspServerConfig],
) -> Option<Arc<dyn LspPoolPort>> {
    if configs.is_empty() {
        return None;
    }
    let lsp_config = LspConfigFile {
        lsp_servers: configs
            .iter()
            .map(|s| (s.name.clone(), s.clone()))
            .collect(),
    };
    Some(Arc::new(LspServerPool::new(cwd, lsp_config)) as Arc<dyn LspPoolPort>)
}

pub(super) fn add_lsp(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext {
        lsp_servers,
        lsp_pool,
        cwd,
        ..
    } = ctx;
    if !lsp_servers.is_empty() {
        // 会话级 pool 复用（workflow_middleware 同构模式，H1）：
        // Some → 复用跨 turn 存活的 pool（服务器进程/initialized/
        // 诊断状态不丢）；None → 临时实例（print 模式等无 session 路径）。
        let lsp_mw = if let Some(pool) = lsp_pool
            .as_ref()
            .and_then(|p| Arc::clone(p).downcast_arc::<LspServerPool>().ok())
        {
            LspMiddleware::from_pool(pool)
        } else {
            let lsp_config = LspConfigFile {
                lsp_servers: lsp_servers
                    .iter()
                    .map(|s| (s.name.clone(), s.clone()))
                    .collect(),
            };
            tracing::info!(
                target: "lsp",
                servers = lsp_config.lsp_servers.len(),
                "LSP 中间件已注册（临时 pool）"
            );
            LspMiddleware::new(cwd.clone(), lsp_config)
        };
        chain.add(Box::new(lsp_mw));
    }
}
