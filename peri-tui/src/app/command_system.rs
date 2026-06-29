use std::collections::HashSet;

use peri_acp_types::skill::SkillMetadataDto;

use crate::command::CommandRegistry;

/// 命令系统：命令注册表、帮助列表、Skills 元数据、Agent 命令集合。
///
/// `agent_commands` 存储从 ACP `AvailableCommandsUpdate` 学习到的命令名集合。
/// 当本地 UICommand 未匹配时，检查该集合——命中则通过 `session/prompt` 发给 Agent 执行。
pub struct CommandSystem {
    pub command_registry: CommandRegistry,
    pub command_help_list: Vec<(String, String, Vec<String>)>,
    pub skills: Vec<SkillMetadataDto>,
    /// 从 ACP AvailableCommandsUpdate 学习到的 Agent 命令名集合（不含 `/` 前缀）。
    pub agent_commands: HashSet<String>,
}

impl CommandSystem {
    pub fn new(
        command_registry: CommandRegistry,
        skills: Vec<SkillMetadataDto>,
        lc: &crate::i18n::LcRegistry,
    ) -> Self {
        let command_help_list = command_registry.list(lc);
        Self {
            command_registry,
            command_help_list,
            skills,
            agent_commands: HashSet::new(),
        }
    }

    /// 从 ACP `AvailableCommandsUpdate` 更新 agent 命令列表。
    /// 过滤已存在于本地 `command_registry` 或 `skills` 列表的名字，避免 Hints 浮层重复显示。
    pub fn update_agent_commands(&mut self, names: Vec<String>) {
        let local_names: HashSet<&str> = self
            .command_help_list
            .iter()
            .flat_map(|(n, _, aliases)| {
                // 同时包含别名，防止别名也重复
                std::iter::once(n.as_str()).chain(aliases.iter().map(|s| s.as_str()))
            })
            .collect();
        let skill_names: HashSet<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
        self.agent_commands = names
            .into_iter()
            .filter(|n| !local_names.contains(n.as_str()) && !skill_names.contains(n.as_str()))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::skill::SkillSourceDto;

    fn make_metadata(name: &str, desc: &str) -> SkillMetadataDto {
        SkillMetadataDto {
            name: name.to_string(),
            description: desc.to_string(),
            path: format!("/fake/{name}/SKILL.md"),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        }
    }

    #[test]
    fn test_update_agent_commands_过滤重复的_skill_名和本地命令名() {
        // skills 列表已有 caveman
        let skills = vec![make_metadata("caveman", "desc")];
        let mut cs = CommandSystem::new(
            crate::command::default_registry(),
            skills,
            &crate::i18n::LcRegistry::default(),
        );
        // ACP 发来的命令列表包含 skill 名、本地已有命令名、和仅 agent 端的命令
        cs.update_agent_commands(vec!["compact".into(), "caveman".into(), "help".into()]);
        // caveman 应被过滤掉（已存在于 skills）
        assert!(
            !cs.agent_commands.contains("caveman"),
            "skill 名不应出现在 agent_commands 中"
        );
        // help 应被过滤掉（已存在于本地 command_registry）
        assert!(
            !cs.agent_commands.contains("help"),
            "本地已有命令不应出现在 agent_commands 中"
        );
        // compact 仅在 agent 端，应保留
        assert!(cs.agent_commands.contains("compact"));
    }

    #[test]
    fn test_update_agent_commands_无_skills_时不过滤() {
        let mut cs = CommandSystem::new(
            crate::command::default_registry(),
            vec![],
            &crate::i18n::LcRegistry::default(),
        );
        cs.update_agent_commands(vec!["compact".into(), "caveman".into()]);
        assert!(cs.agent_commands.contains("compact"));
        assert!(cs.agent_commands.contains("caveman"));
    }
}
