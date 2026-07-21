use std::sync::Arc;

use peri_agent::middleware::{r#trait::Middleware, state::MiddlewareState};

use crate::plugin::loader::LoadedPlugin;

pub struct PluginMiddleware {
    plugins: Arc<Vec<LoadedPlugin>>,
}

impl PluginMiddleware {
    pub fn new(plugins: Vec<LoadedPlugin>) -> Self {
        Self {
            plugins: Arc::new(plugins),
        }
    }

    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }
}

#[async_trait::async_trait]
impl Middleware for PluginMiddleware {
    fn name(&self) -> &str {
        "PluginMiddleware"
    }

    async fn before_agent(
        &self,
        _state: &mut dyn MiddlewareState,
    ) -> peri_agent::error::AgentResult<()> {
        let plugins = &self.plugins;
        if plugins.is_empty() {
            tracing::debug!(target: "plugin", "no plugins loaded");
            return Ok(());
        }

        tracing::info!(
            target: "plugin",
            count = plugins.len(),
            "PluginMiddleware: validating {} loaded plugin(s)",
            plugins.len()
        );

        let mut ok_count = 0u32;
        let mut warn_count = 0u32;

        for p in plugins.iter() {
            let mut warnings: Vec<&str> = Vec::new();

            // 核心字段校验
            if p.name.is_empty() {
                warnings.push("empty name");
            }
            if p.version.is_empty() {
                warnings.push("empty version");
            }
            if p.manifest.name.is_empty() {
                warnings.push("manifest missing name");
            }

            // 统计各能力数量
            let cmd_count = p.commands.len();
            let skill_count = p.skills_roots.len();
            let agent_count = p.agents_dirs.len();
            let mcp_count = p.mcp_servers.len();
            let has_hooks = p.hooks_config.is_some();

            if warnings.is_empty() {
                ok_count += 1;
                tracing::info!(
                    target: "plugin",
                    name = %p.name,
                    version = %p.version,
                    marketplace = %p.marketplace,
                    commands = cmd_count,
                    skills_roots = skill_count,
                    agents = agent_count,
                    mcp_servers = mcp_count,
                    hooks = has_hooks,
                    "plugin validated OK"
                );
            } else {
                warn_count += 1;
                tracing::warn!(
                    target: "plugin",
                    name = %p.name,
                    version = %p.version,
                    marketplace = %p.marketplace,
                    warnings = %warnings.join(", "),
                    commands = cmd_count,
                    skills_roots = skill_count,
                    agents = agent_count,
                    mcp_servers = mcp_count,
                    hooks = has_hooks,
                    "plugin validation warnings"
                );
            }
        }

        tracing::info!(
            target: "plugin",
            total = plugins.len(),
            ok = ok_count,
            warnings = warn_count,
            "PluginMiddleware: validation complete"
        );

        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
