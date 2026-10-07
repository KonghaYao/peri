//! 会话准备输入：准入之前的只读定格，装配与持久化消费同一对象。
//!
//! 与 `PreparedSession`（恢复准入结果：id/identity）不同——本结构是
//! **输入**定格：配置、插件聚合、运行环境、frozen 字节一次产出。准备阶段不启动
//! MCP/hook/cron，不创建 thread、不做 cache repair，也不写会话
//! 数据或本机登记；装配期不再重读配置/插件，也不再各取一份日期与环境探测。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use peri_acp_types::plugin::PluginLoadResult;
use peri_acp_types::skills::SkillRoot;

use crate::provider::{ConfigSource, LlmProvider, PeriConfig};
use crate::session::executor::FrozenSessionData;
use crate::session::frozen_snapshot::{decode_frozen_snapshot, encode_frozen_snapshot};
use crate::transport::types::AcpError;

use super::assemble::{HostCapabilities, PreparedPlugins};
use super::workspace::workspace_error;
use super::AcpServerConfig;

/// legacy 接纳输入：新建与 fork 没有这一项。
///
/// 顶层字段（`configuration` / `plugin_data` / `frozen` / `frozen_encoded`）就是按
/// `saved_cwd` 构建的结果——不重复存第二份，避免同源双写；接纳竞争后 `frozen` 由
/// winner 的持久字节注入（[`PreparedSessionInputs::inject_frozen`]），其余字段不变。
#[derive(Debug, Clone)]
pub(crate) struct LegacyAdoptionInputs {
    /// 保存的绝对执行目录（不是调用方终端的 cwd）。
    pub(crate) saved_cwd: PathBuf,
}

/// 一次准备定格的配置事实：同一 cwd 复用 host 已装配视图，异目录只读一次
/// `ConfigSource::load_at`；provider 由该视图解析。
///
/// 装配（[`super::workspace::SessionEnvironment::assemble_with_frozen`]）显式消费它，
/// 因此装配期不会第二次读配置——「配置/插件/frozen 各一份」在类型上成立。
pub(crate) struct PreparedConfiguration {
    pub(crate) config_source: Arc<ConfigSource>,
    pub(crate) config: Arc<PeriConfig>,
    pub(crate) provider: LlmProvider,
}

/// 会话准备输入（一次准备、后续只读消费）。
pub(crate) struct PreparedSessionInputs {
    /// 规范化后的执行目录。
    pub(crate) cwd: String,
    /// Frozen from the deployment host before P4 switches to the session-local cfg.
    deployment_capabilities: HostCapabilities,
    /// 从同一 `ConfigSource` 读出并合并一次的配置视图。
    pub(crate) configuration: PreparedConfiguration,
    /// 一次加载的插件聚合（roots/commands/hooks/mcp）。
    pub(crate) plugin_data: Option<PluginLoadResult>,
    pub(crate) skill_roots: Vec<SkillRoot>,
    /// ACP session/new 扩展指令；只在新建时加入冻结 system prompt。
    pub(crate) agent_instructions: Option<String>,
    /// MCP servers declared by the ACP client for this session.
    pub(crate) session_mcp_servers:
        std::collections::HashMap<String, peri_acp_types::plugin::McpServerConfig>,
    /// frozen 事实源：new/legacy 是本次构建产物，恢复路径是持久 blob 的注入结果。
    pub(crate) frozen: Option<FrozenSessionData>,
    /// 版本化 snapshot 字节；与 `frozen` 始终同源。
    pub(crate) frozen_encoded: Option<String>,
    /// 仅 legacy：取自保存的绝对 cwd。
    pub(crate) legacy: Option<LegacyAdoptionInputs>,
}

/// 插件发现结果：一次加载的聚合、技能根与 agent 目录。
type DiscoveredPlugins = (Option<PluginLoadResult>, Vec<SkillRoot>);

/// frozen 的来源：新建/legacy 构建一次，fork 直接复用 source 的精确字节。
enum FrozenSource<'a> {
    Build,
    Reuse(&'a str),
}

impl PreparedSessionInputs {
    /// 新建会话准备（测试夹具）：无覆盖来源，冻结立即构建。
    ///
    /// 生产 new 路径必须走 [`Self::prepare_new_deferred`]——覆盖文档只能在 workspace
    /// activate 之后经 MCP 资源读取（J6/X8），这里不构成第二条静默重冻路径。
    #[cfg(test)]
    pub(crate) fn prepare_new(host: &AcpServerConfig, cwd: &str) -> Result<Self, AcpError> {
        let mut inputs = Self::prepare_scope(host, cwd, FrozenSource::Build)?;
        inputs.build_frozen_after_activation(host, HashMap::new(), &[], &Default::default())?;
        Ok(inputs)
    }

    pub(crate) fn prepare_new_deferred(
        host: &AcpServerConfig,
        cwd: &str,
    ) -> Result<Self, AcpError> {
        Self::prepare_scope(host, cwd, FrozenSource::Build)
    }

    /// frozen 按本次解析出的执行目录 `workspace_cwd` 构建（现有兼容语义）。
    /// legacy 恢复准备：`saved_cwd` 是登记事实（保存的绝对 cwd），配置/插件/
    /// frozen 按本次解析出的执行目录 `workspace_cwd` 构建（既有兼容语义）。
    ///
    /// 该构建发生在接纳事务之前，彼时没有执行环境与 MCP 资源面，覆盖文档不可得
    /// （X8：保持内置并 warn，不回落磁盘）。
    pub(crate) fn prepare_legacy(
        host: &AcpServerConfig,
        saved_cwd: &str,
        workspace_cwd: &str,
    ) -> Result<Self, AcpError> {
        let mut inputs = Self::prepare_scope(host, workspace_cwd, FrozenSource::Build)?;
        inputs.legacy = Some(LegacyAdoptionInputs {
            saved_cwd: PathBuf::from(saved_cwd),
        });
        // legacy 首次接纳发生在内容准入之前：没有执行环境 ⇒ 没有 workspace 资源面
        // ⇒ 技能摘要与项目指令都不可得（J2 §3.1；X4/J5：零磁盘兜底）。这里显式
        // warn，避免「永久为空且无任何信号」。
        tracing::warn!(
            cwd = %workspace_cwd,
            "legacy 首次接纳无执行环境：项目指令与技能摘要不可得（不回落磁盘）"
        );
        inputs.build_frozen_after_activation(host, HashMap::new(), &[], &Default::default())?;
        Ok(inputs)
    }

    /// 普通 fork 准备：复用 source 已持久化的精确 frozen 字节，不按当前日期/
    /// 目录重冻；配置与插件按 fork 目录定格一次。
    pub(crate) fn prepare_fork(
        host: &AcpServerConfig,
        cwd: &str,
        source_snapshot: &str,
    ) -> Result<Self, AcpError> {
        Self::prepare_scope(host, cwd, FrozenSource::Reuse(source_snapshot))
    }

    /// 恢复路径的准备（冷 load/resume/legacy 竞争后）：frozen **只接受持久 blob**。
    ///
    /// 与 [`Self::prepare_new`] 的唯一区别是事实源：这里解码已保存的字节（winner），
    /// 绝不按当前配置/日期重建——重建会把已发布的冻结输入换成当前目录状态
    /// （ARC-FROZEN-001）。配置与插件仍按同一条只读规则定格一次。
    pub(crate) fn prepare_restore(
        host: &AcpServerConfig,
        cwd: &str,
        persisted_snapshot: &str,
    ) -> Result<Self, AcpError> {
        Self::prepare_scope(host, cwd, FrozenSource::Reuse(persisted_snapshot))
    }

    /// 把 winner 的持久字节注入为唯一 frozen 事实源（legacy 接纳竞争后重读的判据）。
    ///
    /// 本方法与 `frozen_encoded` 一起替换，保证「解码视图」与「字节」不出现两个真相；
    /// 只用于恢复路径（装配是消费者，不再构建第二份候选）。
    pub(crate) fn inject_frozen(
        &mut self,
        frozen: FrozenSessionData,
        encoded: String,
    ) -> Result<(), AcpError> {
        // 注入的视图与字节必须互相解码一致：两者只能有一个真相，不一致是内部错误。
        let decoded_date = decode_frozen_snapshot(&encoded)
            .map(|decoded| decoded.date().to_owned())
            .unwrap_or_default();
        debug_assert_eq!(
            decoded_date,
            frozen.date(),
            "injected frozen view and bytes must decode consistently"
        );
        self.frozen = Some(frozen);
        self.frozen_encoded = Some(encoded);
        Ok(())
    }

    /// 内容准入的冻结构建：运行环境在本函数内按**有效 Workspace 来源**定格
    /// （H3/D1，`workspace::frozen_runtime_env`）——调用方不再各自传入探测值，
    /// 避免远端 Workspace 会话冻结宿主环境冒充远端执行环境。
    ///
    /// `skill_catalog` = P4 内容准入期从 system 来源（builtin `workspace` 实例）
    /// 取到的技能元数据快照（W4b/F3）：空快照 = 技能面为空/不适用，不是错误。
    /// legacy 首次接纳等无执行环境的构造点传空快照（J5：不回落磁盘）。
    pub(crate) fn build_frozen_after_activation(
        &mut self,
        host: &AcpServerConfig,
        docs: HashMap<String, String>,
        skill_catalog: &[peri_acp_types::skills::SkillMetadata],
        instructions: &crate::session::executor::FrozenInstructions,
    ) -> Result<(), AcpError> {
        if self.frozen.is_some() {
            return Ok(());
        }
        let runtime_env =
            super::workspace::frozen_runtime_env(host, &self.session_mcp_servers, &self.cwd);
        let mut deployment_closed = std::collections::HashSet::new();
        let capabilities = self.deployment_capabilities;
        for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
            if !capabilities.builtin_mcp || (instance.name == "cron" && !capabilities.cron) {
                deployment_closed.insert(instance.policy_key.to_owned());
            }
        }
        let mut frozen = host
            .session_manager
            .build_frozen_data_with_deployment_closure(
                &self.configuration.config,
                &self.cwd,
                runtime_env.as_ref(),
                docs,
                skill_catalog,
                instructions,
                &deployment_closed,
            );
        if let Some(agent_instructions) = self.agent_instructions.as_deref() {
            let mut context = frozen.v2_frozen().clone();
            context.system_prompt = Arc::from(format!(
                "{}\n\n<agent_instructions>\n{}\n</agent_instructions>",
                context.system_prompt, agent_instructions
            ));
            frozen = FrozenSessionData::from_frozen_parts(
                context,
                frozen.claude_local_md().map(Arc::from),
            );
        }
        let encoded = encode_frozen_snapshot(&frozen).map_err(|error| {
            AcpError::new(-32603, format!("Frozen snapshot encode failed: {error}"))
        })?;
        self.frozen = Some(frozen);
        self.frozen_encoded = Some(encoded);
        Ok(())
    }

    /// 装配消费的插件事实（与准备输入同源，不第二次发现）。
    pub(crate) fn plugins(&self) -> PreparedPlugins {
        PreparedPlugins {
            data: self.plugin_data.clone(),
            skill_roots: self.skill_roots.clone(),
        }
    }

    fn prepare_scope(
        host: &AcpServerConfig,
        cwd: &str,
        frozen_source: FrozenSource<'_>,
    ) -> Result<Self, AcpError> {
        let (configuration, (plugin_data, skill_roots)) =
            Self::prepare_configuration_and_plugins(host, cwd)?;
        let (frozen, frozen_encoded) = match frozen_source {
            FrozenSource::Build => (None, None),
            FrozenSource::Reuse(snapshot) => {
                let frozen = decode_frozen_snapshot(snapshot).map_err(workspace_error)?;
                (Some(frozen), Some(snapshot.to_owned()))
            }
        };
        Ok(Self {
            cwd: cwd.to_owned(),
            deployment_capabilities: host
                .workspace_assembly
                .as_ref()
                .map(|source| source.capabilities)
                .unwrap_or_default(),
            configuration,
            plugin_data,
            skill_roots,
            agent_instructions: None,
            session_mcp_servers: HashMap::new(),
            frozen,
            frozen_encoded,
            legacy: None,
        })
    }

    /// 一次读出配置与插件（准备面唯一的只读入口）：同一 cwd 复用 host 已装配视图，
    /// 不同 cwd 只读一次 `ConfigSource::load_at`；provider 由该视图解析，失败即准备失败。
    fn prepare_configuration_and_plugins(
        host: &AcpServerConfig,
        cwd: &str,
    ) -> Result<(PreparedConfiguration, DiscoveredPlugins), AcpError> {
        let configuration = Self::resolve_configuration(host, cwd)?;
        let (plugin_data, skill_roots) = Self::discover_plugins(host, cwd)?;
        Ok((configuration, (plugin_data, skill_roots)))
    }

    fn resolve_configuration(
        host: &AcpServerConfig,
        cwd: &str,
    ) -> Result<PreparedConfiguration, AcpError> {
        let same_directory = host.workspace_assembly.as_ref().is_none_or(|source| {
            std::fs::canonicalize(&source.startup_cwd).ok().as_deref() == Some(Path::new(cwd))
        });
        if same_directory {
            return Ok(PreparedConfiguration {
                config_source: host.config_source.clone(),
                config: Arc::new(host.peri_config.read().clone()),
                provider: host.provider.read().clone(),
            });
        }
        let source = Arc::new(
            ConfigSource::load_at(Path::new(cwd), host.config_source.global_path().to_owned())
                .map_err(workspace_error)?,
        );
        let config = source.loaded_merged();
        let provider = LlmProvider::from_source(&source)
            .ok_or_else(|| AcpError::new(-32603, "No provider configured for session workspace"))?;
        Ok(PreparedConfiguration {
            config_source: source,
            config: Arc::new(config),
            provider,
        })
    }

    /// 插件发现：session 级装配经**严格只读**入口一次加载，失败即准备失败
    /// （缺失/非法清单定位到具体插件，不生成合成清单、不写插件缓存）；
    /// host 级与 bare 沿用既有形状（无插件聚合）。
    fn discover_plugins(host: &AcpServerConfig, cwd: &str) -> Result<DiscoveredPlugins, AcpError> {
        match host.workspace_assembly.as_ref() {
            None => Ok((None, host.plugin_skill_roots.clone())),
            Some(source) if source.bare || !source.capabilities.plugins => Ok((None, Vec::new())),
            Some(_) => {
                // 严格只读发现：用户级 `.claude` 由装配面解析（HOME 优先的唯一
                // 权威在 `plugin::claude_home`，见 `assemble` 函数 doc），
                // 准备面只提供执行目录。
                let data =
                    super::assemble::discover_enabled_plugins_readonly(cwd).map_err(|error| {
                        AcpError::new(-32603, format!("Plugin discovery failed: {error}"))
                    })?;
                let skill_roots = data.all_skill_roots.clone();
                Ok((Some(data), skill_roots))
            }
        }
    }
}
