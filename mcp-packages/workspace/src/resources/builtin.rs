//! Builtin 静态资产：技能（W1 迁入）与 Agent 定义（W5 迁入）。
//!
//! 迁移事实（F7 归位，plan §2.3 / §6.2）：
//! - **内容唯一副本在 provider 侧**（本目录 `builtin/skills/<name>/SKILL.md`
//!   与 `builtin/agents/<id>.md`）；上层不再保留嵌入副本或 fallback；
//! - builtin 技能恒为最低优先级（宿主 roots 顺序的末位），`disable_bundled`
//!   关闭位由输入控制（F12 配置读取仍归宿主）；
//! - builtin 技能无附件文件（清单只含 `SKILL.md`，digest 按嵌入字节计算）；
//! - builtin Agent 恒以 `agent://builtin/{id}/agent.md` 公开（W5）：**是否可用**
//!   由宿主按会话冻结开关（`built_in_subagents_enabled`）判定——provider 只提供
//!   静态资产，不做启用/关闭策略（§5.1「server 不持策略」）；
//! - workspace 与独立 web MCP 引用同一份 explorer 定义；按角色 ID 对外选择。

use peri_acp_types::workspace_resources;
use peri_acp_types::workspace_resources::ResourceScope;
use peri_acp_types::workspace_resources::SKILL_ENTRY_FILE;
use peri_mcp_common::agent_definition::frontmatter;

use super::skills::{SkillFileRecord, SkillRecord, SkillStore};
use super::ResourceBudget;

/// 单个 builtin 技能的编译期嵌入数据。
#[derive(Debug)]
pub struct BuiltinSkill {
    /// 注册表名（与 frontmatter `name` 一致；有测试锁定）。
    pub name: &'static str,
    /// SKILL.md 全文（含 frontmatter），编译期嵌入。
    pub content: &'static str,
}

/// 全部 builtin 技能（顺序不影响功能；与宿主侧注册表**同名单同顺序**）。
pub static BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "use-artifacts",
        content: include_str!("builtin/skills/use-artifacts/SKILL.md"),
    },
    BuiltinSkill {
        name: "goal",
        content: include_str!("builtin/skills/goal/SKILL.md"),
    },
    BuiltinSkill {
        name: "multitask",
        content: include_str!("builtin/skills/multitask/SKILL.md"),
    },
    BuiltinSkill {
        name: "ultra-adlc",
        content: include_str!("builtin/skills/ultra-adlc/SKILL.md"),
    },
    BuiltinSkill {
        name: "ultracode",
        content: include_str!("builtin/skills/ultracode/SKILL.md"),
    },
    BuiltinSkill {
        name: "codemode",
        content: include_str!("builtin/skills/codemode/SKILL.md"),
    },
    BuiltinSkill {
        name: "cron",
        content: include_str!("builtin/skills/cron/SKILL.md"),
    },
    BuiltinSkill {
        name: "monitor",
        content: include_str!("builtin/skills/monitor/SKILL.md"),
    },
    BuiltinSkill {
        name: "interview",
        content: include_str!("builtin/skills/interview/SKILL.md"),
    },
];

/// builtin 技能的原始字节（`SkillRecord::read_file` 的静态读取路径）。
pub(crate) fn builtin_skill_bytes(index: usize) -> Option<&'static [u8]> {
    BUILTIN_SKILLS
        .get(index)
        .map(|skill| skill.content.as_bytes())
}

/// 单个 builtin Agent 定义的编译期嵌入数据（W5 自
/// `peri-middlewares/src/subagent/built-in/*.md` 迁入）。
///
/// `id` = 文件 stem（宿主 `subagent_type` 参数值），定义文件与之一致。
#[derive(Debug)]
pub struct BuiltinAgent {
    /// Agent 标识（`agent://builtin/{id}/agent.md` 的 `{id}` 段）。
    pub id: &'static str,
    /// 定义全文（YAML frontmatter + markdown 正文），编译期嵌入。
    pub content: &'static str,
}

/// 全部 builtin Agent（顺序即 `resources/list` 的公开顺序）。
pub static BUILTIN_AGENTS: &[BuiltinAgent] = &[
    BuiltinAgent {
        id: "coder",
        content: include_str!("builtin/agents/coder.md"),
    },
    BuiltinAgent {
        id: "explorer",
        content: peri_mcp_common::EXPLORER_AGENT,
    },
    BuiltinAgent {
        id: "general-purpose",
        content: include_str!("builtin/agents/general-purpose.md"),
    },
    BuiltinAgent {
        id: "plan",
        content: include_str!("builtin/agents/plan.md"),
    },
    BuiltinAgent {
        id: "verification",
        content: include_str!("builtin/agents/verification.md"),
    },
];

/// builtin Agent 定义的原始字节（`agents::AgentStore::Builtin` 的静态读取路径）。
pub(crate) fn builtin_agent_bytes(index: usize) -> Option<&'static [u8]> {
    BUILTIN_AGENTS
        .get(index)
        .map(|agent| agent.content.as_bytes())
}

/// 构造 builtin 技能条目（frontmatter 解析失败 / 名称非法 / 超预算的条目跳过并记日志）。
pub(crate) fn builtin_skill_records(budget: &ResourceBudget) -> Vec<SkillRecord> {
    let mut records = Vec::new();
    {
        let indexed_entries = BUILTIN_SKILLS.iter().enumerate();
        for (index, skill) in indexed_entries {
            let bytes = skill.content.as_bytes();
            if bytes.len() as u64 > budget.max_file_bytes {
                tracing::warn!(skill = skill.name, "builtin 技能超过大小预算，跳过");
                continue;
            }
            let Some(frontmatter) = frontmatter::frontmatter_json(skill.content) else {
                tracing::warn!(
                    skill = skill.name,
                    "builtin 技能 frontmatter 解析失败，跳过"
                );
                continue;
            };
            let Some((name, description)) = frontmatter::frontmatter_summary(&frontmatter) else {
                tracing::warn!(skill = skill.name, "builtin 技能缺 name/description，跳过");
                continue;
            };
            if name != skill.name {
                tracing::warn!(
                    registry_name = skill.name,
                    frontmatter_name = %name,
                    "builtin 注册表名与 frontmatter name 不一致"
                );
            }
            if !workspace_resources::is_valid_uri_segment(&name) {
                tracing::warn!(
                    skill = skill.name,
                    "builtin 技能名不符合 URI 段约束，不公开"
                );
                continue;
            }
            records.push(SkillRecord {
                scope: ResourceScope::Builtin,
                plugin_name: None,
                name,
                description,
                store: SkillStore::Builtin { index },
                frontmatter,
                files: vec![SkillFileRecord {
                    relative_path: SKILL_ENTRY_FILE.to_string(),
                    digest: workspace_resources::digest_bytes(bytes),
                    size: bytes.len() as u64,
                    text: true,
                }],
            });
        }
    }
    records
}

#[cfg(test)]
#[path = "builtin_test.rs"]
mod tests;

/// 派发 Skill 的能力预检契约（表格 vs 定义、预检正负例）。
#[cfg(test)]
#[path = "builtin_skill_capability_test.rs"]
mod builtin_skill_capability_tests;
