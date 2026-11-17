//! Workspace 资源 provider（W1：资源提供与 wire，不启用宿主投递）。
//!
//! 职责：把宿主输入的资源根（本地三根 / 插件根 / builtin 关闭位）与工作区
//! 绑定文档（项目指令、`.peri/meta` 段落覆盖）扫描为只读资源面，经
//! `resources/list|read|templates/list` 与 SEP-2640 的 `skills/list|get` 暴露。
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
    ResourceScope, INSTRUCTION_INDEX_URI, INSTRUCTION_LOCAL_URI, INSTRUCTION_MAIN_URI,
    META_KEY_DIGEST, META_KEY_PLUGIN, META_KEY_SCOPE, MIME_JSON, MIME_MARKDOWN, SKILL_ENTRY_FILE,
};
use rmcp::model::{MetaObject, Resource, ResourceTemplate};
use thiserror::Error;

pub(crate) mod agents;
pub(crate) mod builtin;
pub(crate) mod instructions;
pub(crate) mod meta;
pub(crate) mod path;
pub(crate) mod scan;
pub(crate) mod skills;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "wire_test.rs"]
mod wire_tests;

mod workspace_resources;
#[cfg(test)]
use workspace_resources::typed_skill_from_record;
pub(crate) use workspace_resources::ResourceBody;
pub use workspace_resources::ResourceBudget;
pub use workspace_resources::ResourceError;
pub(crate) use workspace_resources::ResourcePayload;
pub use workspace_resources::ResourceRoot;
pub(crate) use workspace_resources::WorkspaceResourceProvider;
pub use workspace_resources::WorkspaceResourcesInput;
