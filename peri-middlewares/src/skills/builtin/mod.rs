//! Builtin skills —— 随二进制分发的 SKILL.md，编译期嵌入。
//!
//! 复用 `built_in_agents.rs` 的 `include_str!` + `&'static str` 模式：
//! 零运行时 I/O，最低优先级（被 User/Global/Project/Plugin 同名覆盖）。
//!
//! **资产归属（W1 迁移，J5/F7）**：SKILL.md 文件的唯一副本已迁至
//! `mcp-packages/workspace/src/resources/builtin/skills/`（provider 侧经
//! `resources` 面公开）；本注册表以仓库相对路径 `include_str!` 引用同一批
//! 文件（不复制内容、不引入 re-export）。本模块与引用它的宿主流均在 W4
//! 技能源切换完成时删除，届时 provider 成为唯一读取方。
//!
//! 新增 Builtin skill 步骤：
//! 1. 把 SKILL.md 放到 provider 侧
//!    `mcp-packages/workspace/src/resources/builtin/skills/<name>/SKILL.md`
//! 2. 在 `mcp-packages/workspace/src/resources/builtin.rs` 的 `BUILTIN_SKILLS`
//!    与本文件数组同时追加 entry（两份注册表在 W4 前必须同名单）
//! 3. `builtin_test.rs::test_builtin_skills_frontmatter_valid` 自动覆盖

use gray_matter::{engine::YAML, Matter};

/// 单个 builtin skill 的编译期嵌入数据
pub struct BuiltinSkill {
    pub name: &'static str,
    /// SKILL.md 全文（含 frontmatter），通过 `include_str!` 编译期嵌入
    pub content: &'static str,
}

/// 所有 builtin skills 的注册表（编译期常量数组）
///
/// 顺序不影响功能（scan_skill_roots_impl 按 name 去重），但建议按字母排序便于维护。
///
/// **路径说明（W1）**：资产文件位于 `mcp-packages/workspace/src/resources/builtin/skills/`
/// （provider 侧唯一副本）；本表以 4 级 `..` 指向仓库根再进入该目录。provider 侧
/// 注册表 `peri_mcp_workspace::resources::builtin::BUILTIN_SKILLS` 与下表同名单，
/// 本表随 W4 删除。
pub static BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "use-artifacts",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/use-artifacts/SKILL.md"
        ),
    },
    BuiltinSkill {
        name: "goal",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/goal/SKILL.md"
        ),
    },
    BuiltinSkill {
        name: "multitask",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/multitask/SKILL.md"
        ),
    },
    BuiltinSkill {
        name: "programmatic-tool-calling",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/programmatic-tool-calling/SKILL.md"
        ),
    },
    BuiltinSkill {
        name: "ultra-adlc",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/ultra-adlc/SKILL.md"
        ),
    },
    BuiltinSkill {
        name: "ultracode",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/ultracode/SKILL.md"
        ),
    },
    BuiltinSkill {
        name: "cron",
        content: include_str!(
            "../../../../mcp-packages/workspace/src/resources/builtin/skills/cron/SKILL.md"
        ),
    },
];

/// 从 SKILL.md 全文解析 frontmatter，返回 `(name, aliases, description)`。
///
/// 复用 `loader::load_skill_metadata` 的 `gray_matter::Matter::<YAML>` 解析模式。
/// frontmatter 格式错误或缺字段时返回 `None`，由调用方决定是否跳过。
///
/// **description trim**：YAML `>`（折叠标量）和 `|`（字面标量）会在末尾保留 `\n`，
/// 下游 `build_summary` 把 description 拼到 Markdown list item 末尾，尾随 `\n` 会
/// 让 list 渲染断裂成段落。这里 trim 尾随空白避免该问题。
pub fn parse_builtin_frontmatter(content: &str) -> Option<(String, Vec<String>, String)> {
    let matter = Matter::<YAML>::new();
    // 显式类型注释：ParsedEntity 默认 D=Pod，但类型推断在 .data 访问时会失败
    // 参考 loader.rs:72 的同一模式
    let result: gray_matter::ParsedEntity = matter.parse(content).ok()?;
    let data = result.data?;

    #[derive(serde::Deserialize)]
    struct Fm {
        name: String,
        #[serde(default)]
        aliases: Vec<String>,
        description: String,
    }
    let fm: Fm = data.deserialize().ok()?;
    Some((fm.name, fm.aliases, fm.description.trim().to_string()))
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
