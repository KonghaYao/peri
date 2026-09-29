//! Workspace 资源面契约（2026-09-29 W1 冻结）。
//!
//! 本模块是 resources / skills / agents / instructions 三类**来源面**的跨层事实源：
//! URI scheme 与 authority、scope prefix 取值、`_meta` 私有字段名（`io.peri/`
//! 命名空间）、skills/list|get 的 wire 形状、digest 格式与 URI 语法校验。
//! provider 实现在 `peri-mcp-workspace`（`resources` 模块）；宿主消费面
//! （registry 投影 / activation / 投递策略）属 W2+，不在本模块冻结范围内。
//!
//! - `peri-meta://workspace/{section_id}`（J6，W3 冻结）：段落覆盖文档的资源
//!   形状在此冻结（构造 / 解析见 [`meta_uri`] / [`parse_meta_uri`]）；扫描语义
//!   （一级 `{cwd}/.peri/meta/*.md`、stem = section_id、读取失败跳过）与 provider
//!   在 `peri-mcp-workspace`（`resources::meta`）。宿主 `frozen` 消费面仍归后续
//!   消费波，不得据本模块推断其已完成。
//! - 受限 Peri profile（X5 裁决）：首期不声明 `directoryRead`、不消费
//!   `depends_on` / `tools` / `context_budget` 编排字段。provider 对 frontmatter
//!   **逐字透传**（不改写、不解释编排字段）；带依赖声明的技能是否可激活由宿主
//!   activation（W2）按保守策略拒绝，不在本层表达。
//! - X3 冻结口径：技能公开面**禁 symlink**（存量 symlink 技能不可发现、不可读，
//!   失效面由 provider 日志登记）；URI 中的技能标识是 frontmatter `name` 的
//!   **逐字**值，`: → -` 等别名由宿主从 metadata 派生，wire 不改写。
//!
//! URI 形状（构造与解析的唯一实现点）：
//!
//! - `skill://{scope}/{name}/{relative-path}`（`{scope}` = `user` / `global` /
//!   `project` / `builtin`）；
//! - `skill://plugin/{plugin-name}/{name}/{relative-path}`（插件 scope 的
//!   `plugin-name` 段来自宿主插件 manifest，不是技能 frontmatter）；
//! - `agent://{scope}/{name}/agent.md`（`{name}` = agent 定义标识，即文件
//!   stem / 目录名，不是 frontmatter display name；agents 不发明
//!   `agents/list|get`，只走标准 resources）；
//! - `peri-instruction://workspace/{main|local|index}`（Peri 私有 profile）；
//! - `peri-meta://workspace/{section_id}`（`{section_id}` = `.peri/meta/{id}.md` 的
//!   文件 stem **逐字**值；不要求 ∈ `SECTION_IDS`——是否消费由宿主按配置决定，
//!   provider 只列出扫描到的 stem）。
//!
//! 编码口径：本模块**不实现 percent-decoding**；合法公开 URI 不含 `%`、`?`、
//! `#`、`\`、NUL 与控制字符（构造与解析都拒绝），因此「编码穿越 / 二次解码」
//! 在语法层不成入口。

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ─── scheme / authority ───────────────────────────────────────────────────────

/// 技能资源 scheme（`skill://…`）。
pub const SKILL_RESOURCE_SCHEME: &str = "skill";
/// Agent 定义资源 scheme（`agent://…`）。
pub const AGENT_RESOURCE_SCHEME: &str = "agent";
/// 项目指令资源 scheme（`peri-instruction://…`，Peri 私有 profile）。
pub const INSTRUCTION_RESOURCE_SCHEME: &str = "peri-instruction";
/// MetaHarness 段落覆盖 scheme（J6：W3 实现，本模块只留位）。
pub const META_RESOURCE_SCHEME: &str = "peri-meta";
/// `peri-instruction://` 与 `peri-meta://` 的 authority 段。
pub const WORKSPACE_AUTHORITY: &str = "workspace";

// ─── 入口文件名 ───────────────────────────────────────────────────────────────

/// 技能入口文件（技能根相对名；同时是 `skills/list` 的 `resources[]` 首项与
/// 每项可读验收的锚点）。
pub const SKILL_ENTRY_FILE: &str = "SKILL.md";
/// Agent 定义入口文件。
pub const AGENT_ENTRY_FILE: &str = "agent.md";

// ─── instruction 固定 URI ────────────────────────────────────────────────────

/// 项目指令主文档（`{cwd}` 三候选首个匹配 + `@import` 展开）。
pub const INSTRUCTION_MAIN_URI: &str = "peri-instruction://workspace/main";
/// 项目指令本地叠加文档（`CLAUDE.local.md`）。
pub const INSTRUCTION_LOCAL_URI: &str = "peri-instruction://workspace/local";
/// 项目指令索引（JSON manifest）。
pub const INSTRUCTION_INDEX_URI: &str = "peri-instruction://workspace/index";

// ─── MIME ─────────────────────────────────────────────────────────────────────

/// `text/markdown`（SKILL.md / agent.md / 主指令文档）。
pub const MIME_MARKDOWN: &str = "text/markdown";
/// `text/plain`（UTF-8 文本附件）。
pub const MIME_TEXT: &str = "text/plain";
/// `application/json`（指令索引等结构化文档）。
pub const MIME_JSON: &str = "application/json";
/// `application/octet-stream`（非 UTF-8 附件）。
pub const MIME_OCTET_STREAM: &str = "application/octet-stream";

// ─── `_meta` 私有字段名（`io.peri/` 命名空间）───────────────────────────────
//
// 只占用 `io.peri/` 前缀；不得挤占 `io.mcpp/` 未登记项。值都是公开的
// 来源/完整性标识，不含主机绝对路径。
//
// `peri-instruction://` 与 `peri-meta://` 的 authority 段是 `workspace`（不是
// scope）：这两类 workspace 绑定资源的 `io.peri/scope` 取 `project`（provider
// 侧 [`crate::workspace_resources::ResourceScope::Project`] 的投影口径）。

/// 资源来源 scope（值为 [`ResourceScope`] 的 `as_str()`）。
pub const META_KEY_SCOPE: &str = "io.peri/scope";
/// 插件来源名（仅 Plugin scope 的资源携带）。
pub const META_KEY_PLUGIN: &str = "io.peri/plugin";
/// 内容 digest（`sha256:{64 位小写 hex}`，对**原始字节**计算）。
pub const META_KEY_DIGEST: &str = "io.peri/digest";

// ─── scope ────────────────────────────────────────────────────────────────────

/// 资源来源 scope（URI authority 段的值域与 `_meta` `io.peri/scope` 的值域）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceScope {
    /// `~/.claude/skills` / `~/.claude/agents` 等用户级来源。
    User,
    /// `~/.peri/settings.json` 配置的目录（`skillsDir`）。
    Global,
    /// 工作区项目级来源（`{cwd}/.claude/…`）。
    Project,
    /// 插件 manifest 声明的来源（携带 `plugin_name`）。
    Plugin,
    /// 随二进制分发的静态资产（技能；Agent 静态资产属 W5）。
    Builtin,
}

impl ResourceScope {
    /// 全部 scope（URI 模板按此顺序声明；不代表任何优先级）。
    pub const ALL: [Self; 5] = [
        Self::User,
        Self::Global,
        Self::Project,
        Self::Plugin,
        Self::Builtin,
    ];

    /// URI authority 段与 `_meta` 值的规范写法。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Global => "global",
            Self::Project => "project",
            Self::Plugin => "plugin",
            Self::Builtin => "builtin",
        }
    }

    /// 解析 authority 段；未知值返回 `None`（不做大小写宽松）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "global" => Some(Self::Global),
            "project" => Some(Self::Project),
            "plugin" => Some(Self::Plugin),
            "builtin" => Some(Self::Builtin),
            _ => None,
        }
    }
}

// ─── digest ───────────────────────────────────────────────────────────────────

/// 原始字节的 sha256 digest（`sha256:{64 位小写 hex}`，与既有
/// `SkillResource.digest` 格式一致）。
pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ─── URI 段校验 ───────────────────────────────────────────────────────────────

/// 单个 URI path 段的合法性：非空、非 `.` / `..`、不含 `/` `\` `%` `?` `#`
/// NUL 与控制字符、长度 ≤ 255 字节。
///
/// `:` 是合法字符（X3：wire 不改写 frontmatter `name`，含 `:` 的名称原样进
/// URI；CLI 别名派生归宿主）。
pub fn is_valid_uri_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment.len() <= 255
        && !segment.contains(['/', '\\', '%', '?', '#', '\0'])
        && !segment.chars().any(char::is_control)
}

/// 相对路径（`/` 分隔的段序列）的合法性：至少一段，每段合法。
pub fn is_valid_uri_relative_path(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(is_valid_uri_segment)
}

// ─── 解析结果 ─────────────────────────────────────────────────────────────────

/// 解析后的 skill URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillUri {
    /// 来源 scope（authority 段）。
    pub scope: ResourceScope,
    /// 插件来源名（仅 [`ResourceScope::Plugin`] 为 `Some`）。
    pub plugin_name: Option<String>,
    /// 技能标识（= frontmatter `name` 逐字值）。
    pub name: String,
    /// 技能根相对路径（`SKILL.md` 或附件路径，无前导 `/`）。
    pub relative_path: String,
}

/// 解析后的 agent URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentUri {
    /// 来源 scope（authority 段）。
    pub scope: ResourceScope,
    /// 插件来源名（仅 [`ResourceScope::Plugin`] 为 `Some`）。
    pub plugin_name: Option<String>,
    /// Agent 定义标识（文件 stem / 目录名）。
    pub name: String,
}

/// MetaHarness 段落覆盖文档选择器（`peri-meta://workspace/{section_id}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaUri {
    /// 段落 ID（= 文档文件 stem 的逐字值；无需 ∈ `SECTION_IDS`）。
    pub section_id: String,
}

/// 项目指令文档选择器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionDocument {
    /// `peri-instruction://workspace/main`。
    Main,
    /// `peri-instruction://workspace/local`。
    Local,
    /// `peri-instruction://workspace/index`。
    Index,
}

impl InstructionDocument {
    /// 该文档的规范 URI。
    pub fn uri(self) -> &'static str {
        match self {
            Self::Main => INSTRUCTION_MAIN_URI,
            Self::Local => INSTRUCTION_LOCAL_URI,
            Self::Index => INSTRUCTION_INDEX_URI,
        }
    }
}

// ─── URI 构造 ─────────────────────────────────────────────────────────────────

/// 构造 skill URI；任一段非法或 plugin 组合不成立时返回 `None`。
///
/// - `scope == Plugin` 必须给 `plugin_name`，其余 scope 必须不给；
/// - `relative_path` 是技能根相对路径（`SKILL.md` 或附件）。
pub fn skill_uri(
    scope: ResourceScope,
    plugin_name: Option<&str>,
    name: &str,
    relative_path: &str,
) -> Option<String> {
    if !is_valid_uri_segment(name) || !is_valid_uri_relative_path(relative_path) {
        return None;
    }
    let mut uri = format!("{SKILL_RESOURCE_SCHEME}://{}", scope.as_str());
    match (scope, plugin_name) {
        (ResourceScope::Plugin, Some(plugin)) => {
            if !is_valid_uri_segment(plugin) {
                return None;
            }
            uri.push('/');
            uri.push_str(plugin);
        }
        (ResourceScope::Plugin, None) => return None,
        (_, None) => {}
        (_, Some(_)) => return None,
    }
    uri.push('/');
    uri.push_str(name);
    uri.push('/');
    uri.push_str(relative_path);
    Some(uri)
}

/// 构造 agent URI；`{name}` 是 agent 定义标识（文件 stem / 目录名）。
pub fn agent_uri(scope: ResourceScope, plugin_name: Option<&str>, name: &str) -> Option<String> {
    if !is_valid_uri_segment(name) {
        return None;
    }
    let mut uri = format!("{AGENT_RESOURCE_SCHEME}://{}", scope.as_str());
    match (scope, plugin_name) {
        (ResourceScope::Plugin, Some(plugin)) => {
            if !is_valid_uri_segment(plugin) {
                return None;
            }
            uri.push('/');
            uri.push_str(plugin);
        }
        (ResourceScope::Plugin, None) => return None,
        (_, None) => {}
        (_, Some(_)) => return None,
    }
    uri.push('/');
    uri.push_str(name);
    uri.push('/');
    uri.push_str(AGENT_ENTRY_FILE);
    Some(uri)
}

/// 构造段落覆盖文档 URI（`peri-meta://workspace/{section_id}`）；`section_id`
/// 不满足 URI 段约束时返回 `None`（provider 侧即「该 stem 不公开」）。
pub fn meta_uri(section_id: &str) -> Option<String> {
    if !is_valid_uri_segment(section_id) {
        return None;
    }
    Some(format!(
        "{META_RESOURCE_SCHEME}://{WORKSPACE_AUTHORITY}/{section_id}"
    ))
}

// ─── URI 解析 ─────────────────────────────────────────────────────────────────

/// 剥离 scheme（大小写不敏感，RFC 3986）并返回 `:` 之后的部分。
fn strip_scheme<'a>(uri: &'a str, scheme: &str) -> Option<&'a str> {
    let (head, rest) = uri.split_once(':')?;
    if head.eq_ignore_ascii_case(scheme) {
        Some(rest)
    } else {
        None
    }
}

/// 解析 `{scheme}://{authority}/{…}` 的 authority 与 path。
fn split_authority_path<'a>(uri: &'a str, scheme: &str) -> Option<(&'a str, &'a str)> {
    let rest = strip_scheme(uri, scheme)?;
    let rest = rest.strip_prefix("//")?;
    let (authority, path) = rest.split_once('/')?;
    if path.is_empty() {
        return None;
    }
    Some((authority, path))
}

/// 从 path 段序列解析 `(plugin_name, name, tail)`。
fn parse_scope_segments<'a>(
    scope: ResourceScope,
    mut segments: impl Iterator<Item = &'a str>,
) -> Option<(Option<String>, String, Vec<&'a str>)> {
    let (plugin_name, name) = if scope == ResourceScope::Plugin {
        let plugin = segments.next()?;
        if !is_valid_uri_segment(plugin) {
            return None;
        }
        let name = segments.next()?;
        if !is_valid_uri_segment(name) {
            return None;
        }
        (Some(plugin.to_string()), name.to_string())
    } else {
        let name = segments.next()?;
        if !is_valid_uri_segment(name) {
            return None;
        }
        (None, name.to_string())
    };
    let tail: Vec<&str> = segments.collect();
    if tail.iter().any(|segment| !is_valid_uri_segment(segment)) {
        return None;
    }
    Some((plugin_name, name, tail))
}

/// 解析 skill URI；语法非法（scheme/authority/段）返回 `None`。
///
/// 返回的 URI **不保证资源存在**：存在性由 provider 查询决定。
pub fn parse_skill_uri(uri: &str) -> Option<SkillUri> {
    let (authority, path) = split_authority_path(uri, SKILL_RESOURCE_SCHEME)?;
    let scope = ResourceScope::parse(authority)?;
    let (plugin_name, name, tail) = parse_scope_segments(scope, path.split('/'))?;
    if tail.is_empty() {
        return None;
    }
    Some(SkillUri {
        scope,
        plugin_name,
        name,
        relative_path: tail.join("/"),
    })
}

/// 解析 agent URI；`relative` 必须是唯一一段且等于 [`AGENT_ENTRY_FILE`]。
pub fn parse_agent_uri(uri: &str) -> Option<AgentUri> {
    let (authority, path) = split_authority_path(uri, AGENT_RESOURCE_SCHEME)?;
    let scope = ResourceScope::parse(authority)?;
    let (plugin_name, name, tail) = parse_scope_segments(scope, path.split('/'))?;
    if tail != [AGENT_ENTRY_FILE] {
        return None;
    }
    Some(AgentUri {
        scope,
        plugin_name,
        name,
    })
}

/// 解析项目指令 URI；authority 必须是 [`WORKSPACE_AUTHORITY`]。
pub fn parse_instruction_uri(uri: &str) -> Option<InstructionDocument> {
    let (authority, path) = split_authority_path(uri, INSTRUCTION_RESOURCE_SCHEME)?;
    if authority != WORKSPACE_AUTHORITY || path.contains('/') {
        return None;
    }
    match path {
        "main" => Some(InstructionDocument::Main),
        "local" => Some(InstructionDocument::Local),
        "index" => Some(InstructionDocument::Index),
        _ => None,
    }
}

/// 解析段落覆盖文档 URI；authority 必须是 [`WORKSPACE_AUTHORITY`]，path 恰为
/// 一段合法段落 ID；语法非法返回 `None`。
///
/// 返回的 URI **不保证文档存在**，也不要求 `section_id ∈ SECTION_IDS`：列出与
/// 读取的授权面由 provider 的扫描结果决定（provider 只服务扫描得到的 stem）。
pub fn parse_meta_uri(uri: &str) -> Option<MetaUri> {
    let (authority, path) = split_authority_path(uri, META_RESOURCE_SCHEME)?;
    if authority != WORKSPACE_AUTHORITY || path.contains('/') || !is_valid_uri_segment(path) {
        return None;
    }
    Some(MetaUri {
        section_id: path.to_string(),
    })
}

// ─── wire 形状（skills/list、skills/get）─────────────────────────────────────

/// `skills/list` / `skills/get` 条目里的资源引用（`resources[]` 项）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillResourceRef {
    /// 资源 URI（同一 skill URI 形状；含 SKILL.md 自身）。
    pub uri: String,
    /// 该 URI 内容的 sha256 digest（[`digest_bytes`] 格式）。
    pub digest: String,
}

/// 技能条目（`skills/list` 的 `skills[]` 项与 `skills/get` 的 `skill` 字段，
/// 同构、同规则）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    /// 技能入口 URI（`skill://…/{name}/SKILL.md`）。
    pub uri: String,
    /// SKILL.md YAML frontmatter 的 verbatim JSON 渲染（非精选子集；不做
    /// `:` → `-` 等改写）。
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
    /// 完整资源清单；provider 必须给 `Some`（含 SKILL.md 自身）。`None` 保留
    /// 给动态生成技能的规范 MAY 省略语义，当前 provider 不产出。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<SkillResourceRef>>,
}

/// `skills/list` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListResponse {
    /// 条目列表。
    pub skills: Vec<SkillEntry>,
    /// 分页游标；首期 provider 不分页（缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// 结果新鲜期（毫秒）；首期 provider 不设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// 缓存范围；首期 provider 不设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_scope: Option<String>,
}

/// `skills/get` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillGetResponse {
    /// 该技能当前条目快照（与 `skills/list` 条目同构）。
    pub skill: SkillEntry,
}

#[cfg(test)]
#[path = "workspace_resources_test.rs"]
mod tests;
