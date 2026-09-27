//! 会话准备输入：lease 之前的只读定格，装配与持久化消费同一对象。
//!
//! 与 `PreparedSession`（恢复准入结果：id/identity/read_only）不同——本结构是
//! **输入**定格：配置、插件聚合、运行环境、frozen 字节一次产出。准备阶段不启动
//! MCP/LSP/hook/cron，不创建 thread、不占 lease、不做 cache repair，也不写会话
//! 数据或本机登记；装配期不再重读配置/插件，也不再各取一份日期与环境探测。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use peri_acp_types::plugin::PluginLoadResult;
use peri_acp_types::skills::SkillRoot;

use crate::prompt::PromptRuntimeEnv;
use crate::provider::{ConfigSource, LlmProvider, PeriConfig};
use crate::session::executor::FrozenSessionData;
use crate::session::frozen_snapshot::{decode_frozen_snapshot, encode_frozen_snapshot};
use crate::transport::types::AcpError;

use super::workspace::workspace_error;
use super::AcpServerConfig;

/// legacy 接纳输入：新建与 fork 没有这一项。
///
/// 顶层字段（`config` / `plugin_data` / `frozen` / `frozen_encoded`）就是按
/// `saved_cwd` 构建的结果——不重复存第二份，避免同源双写。
#[derive(Debug, Clone)]
pub(crate) struct LegacyAdoptionInputs {
    /// 保存的绝对执行目录（不是调用方终端的 cwd）。
    pub(crate) saved_cwd: PathBuf,
}

/// 会话准备输入（一次准备、后续只读消费）。
pub(crate) struct PreparedSessionInputs {
    /// 规范化后的执行目录。
    pub(crate) cwd: String,
    /// 从同一 `ConfigSource` 读出并合并一次的配置视图。
    pub(crate) config: Arc<PeriConfig>,
    /// 路径决策事实源（后续持久化沿用，不再判定）。
    pub(crate) config_source: Arc<ConfigSource>,
    /// 由同一 config 解析（或环境变量）的 provider；失败即准备失败。
    pub(crate) provider: LlmProvider,
    /// 一次加载的插件聚合（roots/commands/hooks/lsp/mcp）。
    pub(crate) plugin_data: Option<PluginLoadResult>,
    pub(crate) skill_roots: Vec<SkillRoot>,
    pub(crate) agent_dirs: Vec<PathBuf>,
    pub(crate) frozen: FrozenSessionData,
    /// 版本化 snapshot 字节（数据端口只存不渲染）。
    pub(crate) frozen_encoded: String,
    /// 仅 legacy：取自保存的绝对 cwd。
    pub(crate) legacy: Option<LegacyAdoptionInputs>,
}

/// 插件发现结果：一次加载的聚合、技能根与 agent 目录。
type DiscoveredPlugins = (Option<PluginLoadResult>, Vec<SkillRoot>, Vec<PathBuf>);

/// frozen 的来源：新建/legacy 构建一次，fork 直接复用 source 的精确字节。
enum FrozenSource<'a> {
    Build,
    Reuse(&'a str),
}

impl PreparedSessionInputs {
    /// 新建会话准备：只读，不创建 thread、不占 lease、不启动执行资源。
    pub(crate) fn prepare_new(host: &AcpServerConfig, cwd: &str) -> Result<Self, AcpError> {
        Self::prepare_scope(host, cwd, FrozenSource::Build)
    }

    /// legacy 恢复准备：`saved_cwd` 是登记事实（保存的绝对 cwd），配置/插件/
    /// frozen 按本次解析出的执行目录 `workspace_cwd` 构建（现有兼容语义）。
    pub(crate) fn prepare_legacy(
        host: &AcpServerConfig,
        saved_cwd: &str,
        workspace_cwd: &str,
    ) -> Result<Self, AcpError> {
        let mut inputs = Self::prepare_scope(host, workspace_cwd, FrozenSource::Build)?;
        inputs.legacy = Some(LegacyAdoptionInputs {
            saved_cwd: PathBuf::from(saved_cwd),
        });
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

    fn prepare_scope(
        host: &AcpServerConfig,
        cwd: &str,
        frozen_source: FrozenSource<'_>,
    ) -> Result<Self, AcpError> {
        let (config_source, config, provider) = Self::resolve_configuration(host, cwd)?;
        let (plugin_data, skill_roots, agent_dirs) = Self::discover_plugins(host, cwd)?;
        // 运行环境（平台 / OS / Git）在准备阶段探测一次，随冻结渲染定格；日期由
        // `frozen.date` 固化。装配期不得重新 `detect`/`with_frozen_date` 各取一份
        // ——需要这些事实的下一批消费者应从这里提升字段，而不是各自探测。
        let runtime_env = PromptRuntimeEnv::detect(cwd);
        let (frozen, frozen_encoded) = match frozen_source {
            FrozenSource::Build => {
                let frozen = host
                    .session_manager
                    .build_frozen_data_with_config_and_runtime(
                        &config,
                        cwd,
                        &skill_roots,
                        &agent_dirs,
                        &runtime_env,
                    );
                let encoded = encode_frozen_snapshot(&frozen).map_err(|error| {
                    AcpError::new(-32603, format!("Frozen snapshot encode failed: {error}"))
                })?;
                (frozen, encoded)
            }
            FrozenSource::Reuse(snapshot) => {
                let frozen = decode_frozen_snapshot(snapshot).map_err(workspace_error)?;
                (frozen, snapshot.to_owned())
            }
        };
        Ok(Self {
            cwd: cwd.to_owned(),
            config,
            config_source,
            provider,
            plugin_data,
            skill_roots,
            agent_dirs,
            frozen,
            frozen_encoded,
            legacy: None,
        })
    }

    /// 配置一次读出：同一 cwd 复用 host 已装配视图，不同 cwd 只读一次
    /// `ConfigSource::load_at`；provider 由该视图解析，失败即准备失败。
    fn resolve_configuration(
        host: &AcpServerConfig,
        cwd: &str,
    ) -> Result<(Arc<ConfigSource>, Arc<PeriConfig>, LlmProvider), AcpError> {
        let same_directory = host.workspace_assembly.as_ref().is_none_or(|source| {
            std::fs::canonicalize(&source.startup_cwd).ok().as_deref() == Some(Path::new(cwd))
        });
        if same_directory {
            return Ok((
                host.config_source.clone(),
                Arc::new(host.peri_config.read().clone()),
                host.provider.read().clone(),
            ));
        }
        let source = Arc::new(
            ConfigSource::load_at(Path::new(cwd), host.config_source.global_path().to_owned())
                .map_err(workspace_error)?,
        );
        let config = source.loaded_merged();
        let provider = LlmProvider::from_config(&config)
            .or_else(LlmProvider::from_env)
            .ok_or_else(|| AcpError::new(-32603, "No provider configured for session workspace"))?;
        Ok((source, Arc::new(config), provider))
    }

    /// 插件发现：session 级装配经**严格只读**入口一次加载，失败即准备失败
    /// （缺失/非法清单定位到具体插件，不生成合成清单、不写插件缓存）；
    /// host 级与 bare 沿用既有形状（无插件聚合）。
    fn discover_plugins(host: &AcpServerConfig, cwd: &str) -> Result<DiscoveredPlugins, AcpError> {
        match host.workspace_assembly.as_ref() {
            None => Ok((
                None,
                host.plugin_skill_roots.clone(),
                host.plugin_agent_dirs.clone(),
            )),
            Some(source) if source.bare => Ok((None, Vec::new(), Vec::new())),
            Some(_) => {
                // 严格只读发现：用户级 `.claude` 由装配面解析（HOME 优先的唯一
                // 权威在 `plugin::claude_home`，见 `assemble` 函数 doc），
                // 准备面只提供执行目录。
                let data =
                    super::assemble::discover_enabled_plugins_readonly(cwd).map_err(|error| {
                        AcpError::new(-32603, format!("Plugin discovery failed: {error}"))
                    })?;
                let skill_roots = data.all_skill_roots.clone();
                let agent_dirs = data.all_agent_dirs.clone();
                Ok((Some(data), skill_roots, agent_dirs))
            }
        }
    }
}
