//! Builtin 技能静态资产（W1 自 `peri-middlewares/src/skills/builtin/skills/` 迁入）。
//!
//! 迁移事实（F7 归位，plan §2.3）：
//! - **内容唯一副本在 provider 侧**（本目录 `builtin/skills/<name>/SKILL.md`）；
//!   宿主侧注册表（`peri-middlewares/src/skills/builtin/mod.rs`，W4 删除前仍在位）
//!   以仓库相对路径 `include_str!` 引用同一批文件，不产生第二份内容；
//! - builtin 技能恒为最低优先级（宿主 roots 顺序的末位），`disable_bundled`
//!   关闭位由输入控制（F12 配置读取仍归宿主）；
//! - builtin 无附件文件（清单只含 `SKILL.md`，digest 按嵌入字节计算）。

use peri_acp_types::workspace_resources::{
    digest_bytes, is_valid_uri_segment, ResourceScope, SKILL_ENTRY_FILE,
};

use super::frontmatter::{frontmatter_json, frontmatter_summary};
use super::skills::{SkillFileRecord, SkillRecord, SkillStore};
use super::ResourceBudget;

/// 单个 builtin 技能的编译期嵌入数据。
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
        name: "programmatic-tool-calling",
        content: include_str!("builtin/skills/programmatic-tool-calling/SKILL.md"),
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
        name: "cron",
        content: include_str!("builtin/skills/cron/SKILL.md"),
    },
];

/// builtin 技能的原始字节（`SkillRecord::read_file` 的静态读取路径）。
pub(crate) fn builtin_skill_bytes(index: usize) -> Option<&'static [u8]> {
    BUILTIN_SKILLS
        .get(index)
        .map(|skill| skill.content.as_bytes())
}

/// 构造 builtin 技能条目（frontmatter 解析失败 / 名称非法 / 超预算的条目跳过并记日志）。
pub(crate) fn builtin_skill_records(budget: &ResourceBudget) -> Vec<SkillRecord> {
    let mut records = Vec::new();
    for (index, skill) in BUILTIN_SKILLS.iter().enumerate() {
        let bytes = skill.content.as_bytes();
        if bytes.len() as u64 > budget.max_file_bytes {
            tracing::warn!(skill = skill.name, "builtin 技能超过大小预算，跳过");
            continue;
        }
        let Some(frontmatter) = frontmatter_json(skill.content) else {
            tracing::warn!(
                skill = skill.name,
                "builtin 技能 frontmatter 解析失败，跳过"
            );
            continue;
        };
        let Some((name, description)) = frontmatter_summary(&frontmatter) else {
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
        if !is_valid_uri_segment(&name) {
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
                digest: digest_bytes(bytes),
                size: bytes.len() as u64,
                text: true,
            }],
        });
    }
    records
}

#[cfg(test)]
#[path = "builtin_test.rs"]
mod tests;
