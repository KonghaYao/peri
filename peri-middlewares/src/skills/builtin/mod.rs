//! Builtin skills —— 随二进制分发的 SKILL.md，编译期嵌入。
//!
//! 复用 `built_in_agents.rs` 的 `include_str!` + `&'static str` 模式：
//! 零运行时 I/O，最低优先级（被 User/Global/Project/Plugin 同名覆盖）。
//!
//! 新增 Builtin skill 步骤：
//! 1. 把 SKILL.md 放到 `skills/<name>/SKILL.md`（相对本文件）
//! 2. 在 `BUILTIN_SKILLS` 数组追加 entry
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
pub static BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "use-artifacts",
        content: include_str!("skills/use-artifacts/SKILL.md"),
    },
    BuiltinSkill {
        name: "goal",
        content: include_str!("skills/goal/SKILL.md"),
    },
];

/// 从 SKILL.md 全文解析 frontmatter，返回 `(name, description)`。
///
/// 复用 `loader::load_skill_metadata` 的 `gray_matter::Matter::<YAML>` 解析模式。
/// frontmatter 格式错误或缺字段时返回 `None`，由调用方决定是否跳过。
///
/// **description trim**：YAML `>`（折叠标量）和 `|`（字面标量）会在末尾保留 `\n`，
/// 下游 `build_summary` 把 description 拼到 Markdown list item 末尾，尾随 `\n` 会
/// 让 list 渲染断裂成段落。这里 trim 尾随空白避免该问题。
pub fn parse_builtin_frontmatter(content: &str) -> Option<(String, String)> {
    let matter = Matter::<YAML>::new();
    // 显式类型注释：ParsedEntity 默认 D=Pod，但类型推断在 .data 访问时会失败
    // 参考 loader.rs:72 的同一模式
    let result: gray_matter::ParsedEntity = matter.parse(content).ok()?;
    let data = result.data?;

    #[derive(serde::Deserialize)]
    struct Fm {
        name: String,
        description: String,
    }
    let fm: Fm = data.deserialize().ok()?;
    Some((fm.name, fm.description.trim().to_string()))
}

#[cfg(test)]
mod tests {
    include!("../builtin_test.rs");
}
