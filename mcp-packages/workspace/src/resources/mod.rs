//! Workspace 资源 provider（W1：资源提供与 wire，不启用宿主投递）。
//!
//! 职责：把宿主输入的资源根（本地三根 / 插件根 / builtin 关闭位）与工作区
//! 绑定文档（项目指令、`.peri/meta` 段落覆盖）扫描为只读资源面，经
//! `resources/list|read|templates/list` 与 MCPP 的 `skills/list|get` custom
//! requests 暴露。
//!
//! 边界（与 plan §5.1/§8.1 W1 一致）：
//! - server 侧只做已授权文件/嵌入资产的解析、manifest 与只读提供；不持模型
//!   上下文、HITL、外部 server pool；不注册任何工具（技能工具保留宿主，J3）；
//! - 输入一次注入（构造期），provider 不读宿主配置、不做关闭策略判定；
//! - `WorkspaceResourcesInput` 缺省（未装配）由 handler 层表达为「该实例不支持
//!   skills/*」（-32601）与「未知资源」（-32602），**不得**把空输入当
//!   「技能不存在且已 ready」；
//! - 每次请求实时扫描（W1 无跨请求缓存）：`skills/list` 的 manifest 与
//!   `resources/read` 各自自洽；revision/TTL 缓存属 W2+；
//! - `.peri/meta` 段落覆盖（J6，W3）随 provider 装配在 `list_resources` / `read`
//!   内提供，不需要新的输入字段；「list 空」与「read 错误」可区分（目录缺失 /
//!   不可读 → 空批而不是错误；read 未知 stem → `NotFound`），扫描语义见
//!   `resources::meta`；
//! - 错误分类：URI 非法 → `InvalidUri`；合法 URI 但目标不存在/未公开 →
//!   `NotFound`；根外/越界 → `Denied`；超预算 → `Budget`；其他 IO → `Io`。
//!   handler 侧的 MCP 错误码映射见 `workspace.rs`（skills/get 的未知 URI
//!   一律 `-32602` 以符合 MCPP 约定；resources/read 的缺失用 `-32002`）。

use std::path::PathBuf;

use peri_acp_types::workspace_resources::{
    meta_uri, parse_agent_uri, parse_instruction_uri, parse_meta_uri, parse_skill_uri, skill_uri,
    ResourceScope, SkillGetResponse, SkillsListResponse, INSTRUCTION_INDEX_URI,
    INSTRUCTION_LOCAL_URI, INSTRUCTION_MAIN_URI, META_KEY_DIGEST, META_KEY_PLUGIN, META_KEY_SCOPE,
    MIME_JSON, MIME_MARKDOWN, SKILL_ENTRY_FILE,
};
use rmcp::model::{MetaObject, Resource, ResourceTemplate};
use thiserror::Error;

pub(crate) mod agents;
pub(crate) mod builtin;
pub(crate) mod frontmatter;
pub(crate) mod instructions;
pub(crate) mod meta;
pub(crate) mod path;
pub(crate) mod scan;
pub(crate) mod skills;

/// 资源根（宿主构造；顺序即扫描优先级，跨根同名先到先得）。
#[derive(Debug, Clone)]
pub struct ResourceRoot {
    /// 目录路径（宿主解析的根；可能是符号链接，provider 对其做 canonical 化）。
    pub path: PathBuf,
    /// 来源 scope（进 URI authority 与 `_meta`）。
    pub scope: ResourceScope,
    /// 插件来源名（仅 [`ResourceScope::Plugin`] 填；进 URI 的 plugin 段）。
    pub plugin_name: Option<String>,
}

impl ResourceRoot {
    pub fn new(path: impl Into<PathBuf>, scope: ResourceScope) -> Self {
        Self {
            path: path.into(),
            scope,
            plugin_name: None,
        }
    }

    /// 插件根（携带插件名标签）。
    pub fn plugin(path: impl Into<PathBuf>, plugin_name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            scope: ResourceScope::Plugin,
            plugin_name: Some(plugin_name.into()),
        }
    }
}

/// 资源面预算（宿主可调；默认值即 W1 冻结的初始值）。
#[derive(Debug, Clone, Copy)]
pub struct ResourceBudget {
    /// 技能目录发现的最大递归深度（相对每个根）。
    pub max_depth: usize,
    /// 每个根的最大目录访问数。
    pub max_dirs_per_root: usize,
    /// 公开批的技能总数上限（超出截断并记日志）。
    pub max_skills: usize,
    /// 单技能附件枚举上限（含 `SKILL.md`）。
    pub max_files_per_skill: usize,
    /// 单文件大小上限（字节）；超限文件不列入 manifest、读取按预算拒绝。
    pub max_file_bytes: u64,
    /// 项目指令 `@import` 的数量上限。
    pub max_imports: usize,
    /// 项目指令 `@import` 展开树的**累计读取字节**上限（含根正文与每一层被导入
    /// 文件，递归共享同一预算；M9）。达到上限后不再读取后续 import，保留有界
    /// 占位符并记 warn。
    pub max_instruction_total_bytes: u64,
    /// 项目指令最终文本的字节上限（main 展开结果与 local 正文各自适用；M9）。
    /// 超限时保留有界前缀 + 显式截断说明，不先生成巨串再截。
    pub max_instruction_text_bytes: usize,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_dirs_per_root: 1000,
            max_skills: 512,
            max_files_per_skill: 256,
            max_file_bytes: 1024 * 1024,
            max_imports: 64,
            max_instruction_total_bytes: 4 * 1024 * 1024,
            max_instruction_text_bytes: 1024 * 1024,
        }
    }
}

/// 资源面输入（**独立于 `WorkspaceInstanceInput` 的 Bash 输入**；plan §5.1）。
///
/// 根列表由宿主装配提供（F11 插件 manifest / F12 配置读取仍归宿主）；provider
/// 不自行推导配置目录。`disable_bundled` 是 builtin 技能关闭位（F12）。
#[derive(Debug, Clone, Default)]
pub struct WorkspaceResourcesInput {
    pub skill_roots: Vec<ResourceRoot>,
    pub agent_roots: Vec<ResourceRoot>,
    pub disable_bundled: bool,
    /// 项目指令 main 候选的排除 glob（W5 自宿主设置迁入；语义 = 与候选的绝对
    /// 路径字符串做 `glob::Pattern` 匹配，命中则该候选不参与选择——迁移前
    /// `AgentsMdMiddleware::find_file` 的同一口径）。
    pub instruction_excludes: Vec<String>,
    pub budget: ResourceBudget,
}

impl WorkspaceResourcesInput {
    /// 空输入（无根、builtin 启用、默认预算）。
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_skill_root(mut self, root: ResourceRoot) -> Self {
        self.skill_roots.push(root);
        self
    }

    pub fn with_agent_root(mut self, root: ResourceRoot) -> Self {
        self.agent_roots.push(root);
        self
    }

    pub fn with_instruction_excludes(mut self, excludes: Vec<String>) -> Self {
        self.instruction_excludes = excludes;
        self
    }

    pub fn with_disable_bundled(mut self, disable_bundled: bool) -> Self {
        self.disable_bundled = disable_bundled;
        self
    }

    pub fn with_budget(mut self, budget: ResourceBudget) -> Self {
        self.budget = budget;
        self
    }
}

/// 资源面错误（映射到 MCP 错误码的职责在 handler；文案不含绝对路径）。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResourceError {
    /// URI 语法非法 / scheme 未知 / 段非法（编码穿越、NUL、绝对路径等）。
    #[error("invalid resource uri")]
    InvalidUri,
    /// 合法 URI 但目标不存在或未公开（symlink、隐藏项、被预算排除等）。
    #[error("resource not found")]
    NotFound,
    /// 越界访问（canonical 前缀校验失败等）。
    #[error("resource access denied")]
    Denied,
    /// 超过预算（大小 / 数量）。
    #[error("resource exceeds budget")]
    Budget,
    /// 其他 IO 失败。
    #[error("resource io failure")]
    Io,
}

/// 资源正文（文本或二进制）。
#[derive(Debug)]
pub(crate) enum ResourceBody {
    Text(String),
    Blob(Vec<u8>),
}

/// 读取产物：`digest` / `body` 同源（同一次读取的字节）；`meta` 是 `_meta` 投影。
#[derive(Debug)]
pub(crate) struct ResourcePayload {
    pub uri: String,
    pub mime: String,
    pub digest: String,
    pub meta: MetaObject,
    pub body: ResourceBody,
}

/// 资源提供器（构造期冻结 cwd 与输入；只读）。
pub(crate) struct WorkspaceResourceProvider {
    cwd: PathBuf,
    input: WorkspaceResourcesInput,
}

impl WorkspaceResourceProvider {
    pub fn new(cwd: impl Into<PathBuf>, input: WorkspaceResourcesInput) -> Self {
        Self {
            cwd: cwd.into(),
            input,
        }
    }

    fn budget(&self) -> ResourceBudget {
        self.input.budget
    }

    /// 全部公开资源的发现批（技能文件 + agent 定义 + 指令文档）。
    pub fn list_resources(&self) -> Vec<Resource> {
        let budget = self.budget();
        let mut resources = Vec::new();

        for record in skills::scan_catalog(&self.input, &budget) {
            if record.uri().is_none() {
                continue;
            }
            for file in &record.files {
                let Some(uri) = skill_uri(
                    record.scope,
                    record.plugin_name.as_deref(),
                    &record.name,
                    &file.relative_path,
                ) else {
                    continue;
                };
                let mime = skills::mime_for(&file.relative_path, file.text);
                let mut resource = Resource::new(uri, file.relative_path.clone())
                    .with_mime_type(mime)
                    .with_size(file.size)
                    .with_meta(resource_meta(
                        record.scope,
                        record.plugin_name.as_deref(),
                        Some(&file.digest),
                    ));
                if file.relative_path == SKILL_ENTRY_FILE {
                    resource = resource.with_description(record.description.clone());
                }
                resources.push(resource);
            }
        }

        for record in agents::scan_catalog(&self.input, &budget) {
            let Some(uri) = record.uri() else {
                continue;
            };
            let mut meta = resource_meta(
                record.scope,
                record.plugin_name.as_deref(),
                Some(&record.digest),
            );
            // W5：frontmatter JSON 逐字投影（宿主目录渲染的 capability 推断输入；
            // 正文不在这里公开）。
            for (key, value) in record.meta() {
                meta.0.insert(key, value);
            }
            let mut resource = Resource::new(uri, record.agent_id.clone())
                .with_mime_type(MIME_MARKDOWN)
                .with_meta(meta);
            if record.display_name != record.agent_id {
                resource = resource.with_title(record.display_name.clone());
            }
            if !record.description.is_empty() {
                resource = resource.with_description(record.description.clone());
            }
            resources.push(resource);
        }

        let bundle = instructions::scan(&self.cwd, &budget, &self.input.instruction_excludes);
        if let Some(main) = &bundle.main {
            resources.push(
                Resource::new(INSTRUCTION_MAIN_URI, "main")
                    .with_description(main.source_label.clone())
                    .with_mime_type(MIME_MARKDOWN)
                    .with_meta(resource_meta(
                        ResourceScope::Project,
                        None,
                        Some(&main.digest),
                    )),
            );
        }
        if let Some(local) = &bundle.local {
            resources.push(
                Resource::new(INSTRUCTION_LOCAL_URI, "local")
                    .with_description(local.source_label.clone())
                    .with_meta(resource_meta(
                        ResourceScope::Project,
                        None,
                        Some(&local.digest),
                    )),
            );
        }
        resources.push(Resource::new(INSTRUCTION_INDEX_URI, "index").with_mime_type(MIME_JSON));

        // 段落覆盖文档（J6）：list 只列 `.peri/meta/*.md` 实际存在的 stem（含不在
        // SECTION_IDS 的项）；正文不在此预取，宿主仅对启用 section 走 read。
        for section in meta::scan_sections(&self.cwd) {
            let Some(uri) = meta_uri(&section.section_id) else {
                // 扫描期已按 URI 段约束过滤；此处保持诚实映射。
                continue;
            };
            resources.push(
                Resource::new(uri, section.section_id.clone())
                    .with_description(format!(
                        "{}/{}.md",
                        meta::META_DIR_RELATIVE,
                        section.section_id
                    ))
                    .with_mime_type(MIME_MARKDOWN)
                    .with_meta(resource_meta(
                        ResourceScope::Project,
                        None,
                        Some(&section.digest),
                    )),
            );
        }

        resources
    }

    /// 资源模板（只描述可读路径范围；read 仍逐 URI 校验）。
    pub fn list_resource_templates(&self) -> Vec<ResourceTemplate> {
        let mut templates = Vec::new();
        for scope in ResourceScope::ALL {
            let (entry_template, attachment_template) = match scope {
                ResourceScope::Plugin => (
                    "skill://plugin/{plugin_name}/{name}/SKILL.md".to_string(),
                    "skill://plugin/{plugin_name}/{name}/{+path}".to_string(),
                ),
                other => (
                    format!("skill://{}/{{name}}/SKILL.md", other.as_str()),
                    format!("skill://{}/{{name}}/{{+path}}", other.as_str()),
                ),
            };
            templates.push(
                ResourceTemplate::new(entry_template, format!("skill-entry-{}", scope.as_str()))
                    .with_description("Skill entry document (SKILL.md)")
                    .with_mime_type(MIME_MARKDOWN),
            );
            templates.push(
                ResourceTemplate::new(
                    attachment_template,
                    format!("skill-attachment-{}", scope.as_str()),
                )
                .with_description("Skill attachment file (root-relative path)"),
            );
        }
        for scope in [
            ResourceScope::User,
            ResourceScope::Global,
            ResourceScope::Project,
            // W5：builtin 静态 Agent 定义（`agent://builtin/{name}/agent.md`）。
            ResourceScope::Builtin,
        ] {
            templates.push(
                ResourceTemplate::new(
                    format!("agent://{}/{{name}}/agent.md", scope.as_str()),
                    format!("agent-definition-{}", scope.as_str()),
                )
                .with_description("Agent definition document")
                .with_mime_type(MIME_MARKDOWN),
            );
        }
        templates.push(
            ResourceTemplate::new(
                "agent://plugin/{plugin_name}/{name}/agent.md",
                "agent-definition-plugin",
            )
            .with_description("Plugin agent definition document")
            .with_mime_type(MIME_MARKDOWN),
        );
        templates
    }

    /// 读取任意资源 URI（按 scheme 分派；未知 scheme / 非法形态 → `InvalidUri`）。
    pub fn read(&self, uri: &str) -> Result<ResourcePayload, ResourceError> {
        if let Some(parsed) = parse_skill_uri(uri) {
            return self.read_skill(uri, &parsed);
        }
        if let Some(parsed) = parse_agent_uri(uri) {
            return self.read_agent(uri, &parsed);
        }
        if let Some(document) = parse_instruction_uri(uri) {
            let read = instructions::read(
                &self.cwd,
                &self.budget(),
                document,
                &self.input.instruction_excludes,
            )?;
            return Ok(ResourcePayload {
                uri: uri.to_string(),
                mime: read.mime.to_string(),
                digest: read.digest.clone(),
                meta: resource_meta(ResourceScope::Project, None, Some(&read.digest)),
                body: ResourceBody::Text(read.text),
            });
        }
        if let Some(section) = parse_meta_uri(uri) {
            // 只服务扫描得到的 stem（不做任意路径 join）；未知 / 不可得 → NotFound。
            let read = meta::read(&self.cwd, &section.section_id)?;
            return Ok(ResourcePayload {
                uri: uri.to_string(),
                mime: MIME_MARKDOWN.to_string(),
                digest: read.digest.clone(),
                meta: resource_meta(ResourceScope::Project, None, Some(&read.digest)),
                body: ResourceBody::Text(read.text),
            });
        }
        Err(ResourceError::InvalidUri)
    }

    fn read_skill(
        &self,
        uri: &str,
        parsed: &peri_acp_types::workspace_resources::SkillUri,
    ) -> Result<ResourcePayload, ResourceError> {
        let budget = self.budget();
        let record = skills::locate(
            &self.input,
            &budget,
            parsed.scope,
            parsed.plugin_name.as_deref(),
            &parsed.name,
        )?;
        let (bytes, file) = record.read_file(&parsed.relative_path, budget.max_file_bytes)?;
        let mime = skills::mime_for(&parsed.relative_path, file.text).to_string();
        let digest = peri_acp_types::workspace_resources::digest_bytes(&bytes);
        let body = if file.text {
            match String::from_utf8(bytes) {
                Ok(text) => ResourceBody::Text(text),
                Err(_) => return Err(ResourceError::Io),
            }
        } else {
            ResourceBody::Blob(bytes)
        };
        Ok(ResourcePayload {
            uri: uri.to_string(),
            mime,
            digest: digest.clone(),
            meta: resource_meta(parsed.scope, parsed.plugin_name.as_deref(), Some(&digest)),
            body,
        })
    }

    fn read_agent(
        &self,
        uri: &str,
        parsed: &peri_acp_types::workspace_resources::AgentUri,
    ) -> Result<ResourcePayload, ResourceError> {
        let budget = self.budget();
        let record = agents::locate(
            &self.input,
            &budget,
            parsed.scope,
            parsed.plugin_name.as_deref(),
            &parsed.name,
        )?;
        let bytes =
            agents::read_record_definition(&record, &budget).ok_or(ResourceError::NotFound)?;
        let digest = peri_acp_types::workspace_resources::digest_bytes(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| ResourceError::Io)?;
        let mut meta = resource_meta(parsed.scope, parsed.plugin_name.as_deref(), Some(&digest));
        // 读取面同样投影 frontmatter（与 list 面同键同值，来源是本次读取的字节
        // 所携带的定义文本；scope/plugin 以 URI 解析结果为准）。
        for (key, value) in record.meta() {
            meta.0.insert(key, value);
        }
        Ok(ResourcePayload {
            uri: uri.to_string(),
            mime: MIME_MARKDOWN.to_string(),
            digest: digest.clone(),
            meta,
            body: ResourceBody::Text(text),
        })
    }

    /// `skills/list`（非空 cursor 按非法参数拒绝——首期不分页）。
    pub fn skills_list(&self, cursor: Option<String>) -> Result<SkillsListResponse, ResourceError> {
        skills::skills_list_response(&self.input, &self.budget(), cursor)
    }

    /// `skills/get`（按 URI 直读当前条目快照；URI 非法/未知 → `InvalidUri`）。
    pub fn skills_get(&self, uri: &str) -> Result<SkillGetResponse, ResourceError> {
        skills::skills_get_response(&self.input, &self.budget(), uri)
    }
}

/// `_meta` 私有 metadata（`io.peri/` 命名空间；不含主机绝对路径）。
fn resource_meta(
    scope: ResourceScope,
    plugin_name: Option<&str>,
    digest: Option<&str>,
) -> MetaObject {
    let mut meta = serde_json::Map::new();
    meta.insert(
        META_KEY_SCOPE.to_string(),
        serde_json::Value::String(scope.as_str().to_string()),
    );
    if let Some(plugin) = plugin_name {
        meta.insert(
            META_KEY_PLUGIN.to_string(),
            serde_json::Value::String(plugin.to_string()),
        );
    }
    if let Some(digest) = digest {
        meta.insert(
            META_KEY_DIGEST.to_string(),
            serde_json::Value::String(digest.to_string()),
        );
    }
    MetaObject(meta)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "wire_test.rs"]
mod wire_tests;
