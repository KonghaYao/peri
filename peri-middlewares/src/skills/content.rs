use std::path::PathBuf;

use super::{SkillMetadata, SkillSource};

#[derive(Debug, thiserror::Error)]
pub(crate) enum SkillContentError {
    #[error("Builtin skill '{0}' not found in BUILTIN_SKILLS")]
    MissingBuiltin(String),
    #[error("Skill '{0}' not found. Use DiscoverSkillsTool to see available skills.")]
    MissingMcpContent(String),
    #[error("Skill '{name}' is in the session catalog but its file cannot be read ({path}). It may have been moved or deleted mid-session — run DiscoverSkillsTool to see the current set.")]
    UnreadableFile { name: String, path: PathBuf },
}

// 所有消费方在这里读取已选定的 skill；MCP 仅消费发现时缓存的内容，
// 不能因缓存缺失改读磁盘。返回内容已含来源标注，调用方不得重复包装。
// 本地来源包含阻塞 I/O，async 调用方必须在 blocking 线程中执行。
pub(crate) fn load(skill: &SkillMetadata) -> Result<String, SkillContentError> {
    match skill.source {
        SkillSource::Builtin => super::builtin::BUILTIN_SKILLS
            .iter()
            .find(|builtin| {
                super::normalize_skill_name(builtin.name) == skill.name
                    || skill.path == std::path::Path::new(&format!("<builtin>/{}", builtin.name))
            })
            .map(|builtin| builtin.content.to_string())
            .ok_or_else(|| SkillContentError::MissingBuiltin(skill.name.clone())),
        SkillSource::Mcp => skill
            .content
            .as_deref()
            .map(|content| super::annotate_mcp_content(skill, content))
            .ok_or_else(|| SkillContentError::MissingMcpContent(skill.name.clone())),
        SkillSource::User | SkillSource::Global | SkillSource::Project | SkillSource::Plugin => {
            std::fs::read_to_string(&skill.path).map_err(|_| SkillContentError::UnreadableFile {
                name: skill.name.clone(),
                path: skill.path.clone(),
            })
        }
    }
}
