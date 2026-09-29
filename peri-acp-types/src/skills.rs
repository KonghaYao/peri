//! Skill 契约（来源标签 / 根目录 / 元数据）。
//!
//! 自 `peri-middlewares/src/skills/loader.rs` 迁入（3.0 批 2 波 1：协议类型
//! 归契约层；middlewares 保留 re-export 保兼容）。
//!
//! W4b（J5）：技能**内容**的扫描与读取整体归 MCP 侧（builtin `workspace`
//! 实例的资源 provider），宿主侧只剩根解析适配器（
//! `peri_middlewares::resolve_skill_roots`）与配置读取
//! （`peri_middlewares::settings`）；本模块因此只承载类型契约。

use std::path::PathBuf;

/// SEP-2640 Skills 扩展标识（`capabilities.extensions` 键；server 声明即支持
/// `skills/list` / `skills/get`）。
///
/// 单一事实源：provider 侧在 `ServerCapabilities.extensions` 写入该键
/// （`mcp-packages/workspace`），客户端侧据其判定 `skills_capable`（规范路径
/// 的唯一门闩，`peri-middlewares/src/mcp/client/service.rs`）；两侧各自硬编码
/// 会静默漂移成「声明了但客户端不认」。
pub const SKILLS_EXTENSION_ID: &str = "io.modelcontextprotocol/skills";

/// Skill 来源 scope，用于 metadata 标签与日志诊断
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// ~/.claude/skills
    User,
    /// ~/.peri/settings.json::skillsDir
    Global,
    /// {cwd}/.claude/skills
    Project,
    /// 插件 manifest 声明的 skill 目录
    Plugin,
    /// 随二进制分发的内置 skill（include_str! 编译期嵌入）
    Builtin,
    /// MCP 服务器经 `skill://` 资源发现的远端 skill
    Mcp,
}

/// Skill 来源定位信息（仅远端来源填写；本地 skill 为 None）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOrigin {
    /// 来自 MCP 服务器 `skill://` 资源（uri 为发现时的资源 URI）
    Mcp { server: String, uri: String },
}

/// 带 source 标签的 skill 根目录
#[derive(Debug, Clone)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub source: SkillSource,
    /// 仅 Plugin source 填，用于日志诊断
    pub plugin_name: Option<String>,
}

/// Skill 资源绑定（仅 Mcp source 填写；本地 skill 为空）。
///
/// 对应 SEP-2640 `skills/list` / `skills/get` 条目 `resources[]` 的完整清单
/// （非精选子集）：host 持有条目期间，读该 skill 的文件须 resolve 到所列
/// URI；`digest` 为对应文件内容的 sha256（格式 `sha256:{64 位小写 hex}`），
/// 读取面按它做内容绑定校验。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillResource {
    pub uri: String,
    pub digest: String,
}

/// Skill 元数据（来自 SKILL.md frontmatter）
#[derive(Debug, Clone)]
pub struct SkillMetadata {
    pub name: String,
    /// 规范名称的可选别名（frontmatter `aliases`），用于查找与命令路由。
    pub aliases: Vec<String>,
    pub description: String,
    pub path: PathBuf,
    /// skill 来源（由 scan_dir_recursive 注入，load_skill_metadata 内填占位）
    pub source: SkillSource,
    /// 仅 Plugin source 填，其他为 None
    pub plugin_name: Option<String>,
    /// 远端来源定位（仅 Mcp source 填，其余 None）
    pub origin: Option<SkillOrigin>,
    /// 已读入的 SKILL.md 全文（仅 Mcp source 填，本地为 None）
    pub content: Option<String>,
    /// 技能资源绑定（仅 Mcp source 填——entry.resources 完整清单；本地为空）
    pub resources: Vec<SkillResource>,
    /// 发现条目的 frontmatter 全文（verbatim JSON map；仅 Mcp 发现面填写）。
    ///
    /// W2：激活时以它与读到的正文 frontmatter **逐字段全量**比对（任何差异，
    /// 含附加字段，MUST NOT load），因此条目必须持有发现时的原始 map——不是
    /// 精选子集、不从 name/description 重建。本地来源为 None。
    pub frontmatter: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Default for SkillMetadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            aliases: Vec::new(),
            description: String::new(),
            path: PathBuf::new(),
            source: SkillSource::Project,
            plugin_name: None,
            origin: None,
            content: None,
            resources: Vec::new(),
            frontmatter: None,
        }
    }
}
