use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;

use super::config::PeriConfig;

/// 进程级全局配置路径重定向（None 表示未设置，使用默认路径）。
///
/// 仅由部署装配点（CLI 入口 `--config-file`）在启动早期调用一次，之后
/// 全部读取经 [`config_path`] 跟随。相对路径按启动时 cwd 解析为绝对路径。
static CONFIG_PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 全局配置文件路径。
///
/// 已通过 [`set_global_config_path`] 设置重定向时返回重定向路径，否则返回默认
/// `~/.peri/settings.json`。
pub fn config_path() -> PathBuf {
    CONFIG_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(default_config_path)
}

/// 默认配置文件路径：~/.peri/settings.json
fn default_config_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json")
}

/// 进程级重定向全局配置文件路径；`None` 复位为默认路径。
///
/// 由部署装配点（CLI 入口）在启动早期调用一次，之后 [`config_path()`] 跟随
/// 该路径。相对路径按启动时 cwd 解析为绝对路径。
pub fn set_global_config_path(path: Option<PathBuf>) {
    let resolved = path.map(|p| {
        if p.is_relative() {
            std::env::current_dir()
                .ok()
                .map(|c| c.join(&p))
                .unwrap_or(p)
        } else {
            p
        }
    });
    *CONFIG_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = resolved;
}

/// 工作区配置路径探测：`{cwd}/.peri/settings.json` 存在时返回，否则 None。
fn workspace_config_path_at(cwd: &Path) -> Option<PathBuf> {
    let path = cwd.join(".peri").join("settings.json");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// 基于进程当前目录的工作区配置路径探测（只读场景：main.rs 启动期 env 注入）。
///
/// 保存场景请勿调用本函数——保存必须经 [`ConfigSource`]（加载时已确定布局，
/// 保证读写路径决策一致，不会漂移）。
pub fn workspace_config_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    workspace_config_path_at(&cwd)
}

/// 配置源——加载时一次性确定的「全局 + 工作区」布局与分层基准。
///
/// 不可变值对象：进程启动早期构建一次（TUI/ACP 共享同一 `Arc`），此后所有
/// 保存操作都从该实例出发，与加载共享同一个路径决策——读写对称，不可能出现
/// "加载用工作区配置、保存写全局文件"的漂移。
pub struct ConfigSource {
    global_path: PathBuf,
    workspace_path: Option<PathBuf>,
    /// 原始全局配置（分层回写的差异基准，不含工作区覆盖）
    global: PeriConfig,
    /// 原始工作区配置（仅用于保留文件级元数据如 `$schema`）
    workspace: Option<PeriConfig>,
    /// 加载时刻的合并配置（全局 + 工作区覆盖）
    merged: PeriConfig,
}

impl ConfigSource {
    /// 确定性构造：显式 cwd + 全局路径。测试直接调用，无需切换进程 cwd
    /// 或依赖进程级重定向。
    pub fn load_at(cwd: &Path, global_path: PathBuf) -> Result<Self> {
        let workspace_path = workspace_config_path_at(cwd);
        let global = load_from(&global_path)?;
        let workspace = workspace_path.as_deref().map(load_from).transpose()?;
        let mut merged = global.clone();
        if let Some(ws) = &workspace {
            merged.config.merge_overrides(ws.config.clone());
        }
        Ok(Self {
            global_path,
            workspace_path,
            global,
            workspace,
            merged,
        })
    }

    /// 生产入口：cwd = 进程当前目录；全局路径跟随 [`config_path`]
    /// （含 `--config-file` 重定向）。
    pub fn load() -> Result<Self> {
        let cwd =
            std::env::current_dir().map_err(|e| anyhow::anyhow!("无法获取当前工作目录: {e}"))?;
        Self::load_at(&cwd, config_path())
    }

    /// 容错构造：解析失败时按空配置继续（warn 不 fail）。
    ///
    /// 生产启动路径使用——配置文件损坏时保持与迁移前 `load().ok()` 一致的
    /// fallback 行为（回退环境变量），同时保留路径决策供保存使用。
    pub fn load_lenient() -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        Self::load_at_lenient(&cwd, config_path())
    }

    /// 单文件来源构造：指定文件整体生效（`--settings` 语义——不探测工作区、
    /// 不合并全局配置），写回仍写该文件。读写对称。
    pub fn load_standalone(path: PathBuf) -> Result<Self> {
        let cfg = load_from(&path)?;
        Ok(Self {
            global_path: path,
            workspace_path: None,
            global: cfg.clone(),
            workspace: None,
            merged: cfg,
        })
    }

    /// 容错构造的确定性版本（显式 cwd + 全局路径），测试友好。
    pub fn load_at_lenient(cwd: &Path, global_path: PathBuf) -> Self {
        let workspace_path = workspace_config_path_at(cwd);
        let global = load_from(&global_path).unwrap_or_else(|e| {
            tracing::warn!(path = %global_path.display(), error = %e, "全局配置解析失败，按空配置继续");
            PeriConfig::default()
        });
        let workspace = workspace_path.as_deref().map(|p| {
            load_from(p).unwrap_or_else(|e| {
                tracing::warn!(path = %p.display(), error = %e, "工作区配置解析失败，按空配置继续");
                PeriConfig::default()
            })
        });
        let mut merged = global.clone();
        if let Some(ws) = &workspace {
            merged.config.merge_overrides(ws.config.clone());
        }
        Self {
            global_path,
            workspace_path,
            global,
            workspace,
            merged,
        }
    }

    /// 全局配置文件路径
    pub fn global_path(&self) -> &Path {
        &self.global_path
    }

    /// 工作区配置文件路径（无项目级配置时为 None）
    pub fn workspace_path(&self) -> Option<&Path> {
        self.workspace_path.as_deref()
    }

    /// 当前是否为工作区生效模式（存在项目级 `.peri/settings.json`）
    pub fn is_workspace(&self) -> bool {
        self.workspace_path.is_some()
    }

    /// 原始全局配置（未合并，分层回写基准）
    pub fn global_config(&self) -> &PeriConfig {
        &self.global
    }

    /// 加载时刻的合并配置（全局 + 工作区覆盖；调用方修改后经 [`Self::save`]
    /// 写回）。
    pub fn loaded_merged(&self) -> PeriConfig {
        self.merged.clone()
    }

    /// 写回当前生效层。
    ///
    /// 分层契约（与 `load` 对称，P0 修复语义）：
    /// - 工作区配置存在 → 只写「合并配置相对全局的差异字段」
    ///   （[`extract_overrides`](super::config::AppConfig::extract_overrides)）
    ///   到工作区文件，全局文件不动——工作区文件保持"项目覆盖"性质，
    ///   全局凭据不会拷贝进项目文件；
    /// - 无工作区配置 → 写全局文件（唯一事实源）。
    pub fn save(&self, merged: &PeriConfig) -> Result<()> {
        match &self.workspace_path {
            Some(ws_path) => {
                let overrides = merged.config.extract_overrides(&self.global.config);
                let schema = self
                    .workspace
                    .as_ref()
                    .and_then(|w| w.schema.clone())
                    .or_else(|| merged.schema.clone());
                save_to(
                    &PeriConfig {
                        schema,
                        config: overrides,
                    },
                    ws_path,
                )
            }
            None => save_to(merged, &self.global_path),
        }
    }
}

/// 加载合并配置（全局 + 工作区覆盖），文件不存在时返回默认空配置。
///
/// 便捷只读入口（cli_print / setup_wizard 等不持有 [`ConfigSource`] 的场景）。
/// 需要写回时请经 [`ConfigSource::save`]——保证读写路径决策一致。
pub fn load() -> Result<PeriConfig> {
    Ok(ConfigSource::load()?.loaded_merged())
}

/// 从指定路径加载配置
///
/// 解析成功后对 `config.meta_harness` 做解析期校验（未知 key warn + 忽略，
/// 见 [`super::config::AppConfig::validate_meta_harness`]）——warn 不 fail，
/// 不改变既有 `Result` 语义。
pub fn load_from(path: &Path) -> Result<PeriConfig> {
    if !path.exists() {
        return Ok(PeriConfig::default());
    }
    let content = std::fs::read_to_string(path)?;
    let mut cfg: PeriConfig = serde_json::from_str(&content)?;
    cfg.config.validate_meta_harness();
    Ok(cfg)
}

/// 将配置写入指定路径（原子写：先写临时文件，再 rename，避免写入中断
/// 导致文件损坏）。
///
/// 仅限显式路径场景（测试注入 / ACP 装配快照）；业务保存请经
/// [`ConfigSource::save`]。
pub fn save_to(cfg: &PeriConfig, path: &Path) -> Result<()> {
    // 确保目录存在
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(cfg)?;

    // atomic write
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_tests;
