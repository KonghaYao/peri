//! Skill **根解析适配器**（F11/F12：只产出「根路径 + scope/标签」，不读技能内容）。
//!
//! J5（2026-09-29 裁决）：技能内容的读取整体归 MCP 侧（builtin `workspace`
//! 实例的资源 provider）。本模块因此只剩两件事：
//!
//! 1. 契约类型的 re-export（`SkillMetadata` / `SkillRoot` / `SkillSource`，定义在
//!    `peri_acp_types::skills`）；
//! 2. [`resolve_skill_roots`]——按优先级把「用户级 / 全局配置 / 项目级 / 插件
//!    manifest」解析为一组带 scope 标签的根，交给 provider 当输入
//!    （`WorkspaceResourcesInput::skill_roots`）。
//!
//! 已删除（W4b，plan §2.3 的 F2–F5/F7–F10）：目录扫描（`scan_skill_roots` /
//! `scan_dir_recursive`）、单文件读取（`load_skill_metadata`）、按名查找
//! （`find_skill_content` / `find_skill_in_list`）与 `builtin` 编译期嵌入注册表。
//! 宿主不再有任何技能文件系统读取点；这些语义（深度/目录数预算、symlink 口径、
//! 叶子语义、同名先到先得）由 provider 在 MCP 侧承担
//! （`mcp-packages/workspace/src/resources/skills.rs`）。

use std::path::PathBuf;

// 3.0 批 2 波 1：协议类型归契约层（本模块保留 re-export 保兼容）。
pub use peri_acp_types::skills::{SkillMetadata, SkillRoot, SkillSource};

/// 统一解析 skill 根列表，按优先级返回 `SkillRoot`。
///
/// 顺序即去重优先级：User → Global → Project → Plugin → Builtin（先到先得）。
/// 这是 skill **根解析**的 single source of truth（[`crate::skills::SkillsMiddleware`]
/// 与 provider 输入装配共用）；扫描语义不在宿主侧实现。
///
/// `disable_bundled=true` 时跳过 Builtin root（用户通过 settings.json
/// `config.disableBundledSkills: true` 关闭内置技能；该位同样作为 provider 的
/// `disable_bundled` 输入传递，见 `WorkspaceResourcesInput::with_disable_bundled`）。
pub fn resolve_skill_roots(
    cwd: &str,
    plugin_roots: Vec<SkillRoot>,
    disable_bundled: bool,
) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    // 1. User（主目录经 plugin::claude_home 解析，HOME 优先的唯一权威）
    roots.push(SkillRoot {
        path: crate::plugin::claude_home().join("skills"),
        source: SkillSource::User,
        plugin_name: None,
    });

    // 2. Global（~/.peri/settings.json::skillsDir）
    if let Some(dir) = crate::skills::load_global_skills_dir() {
        roots.push(SkillRoot {
            path: dir,
            source: SkillSource::Global,
            plugin_name: None,
        });
    }

    // 3. Project
    roots.push(SkillRoot {
        path: PathBuf::from(cwd).join(".claude").join("skills"),
        source: SkillSource::Project,
        plugin_name: None,
    });

    // 4. Plugin（来自参数，已带 source/plugin_name）
    for r in plugin_roots {
        if r.path.is_dir() {
            roots.push(r);
        }
    }

    // 5. Builtin（最低优先级；path 为占位——静态资产由 provider 内置提供，
    //    本 root 只表达「builtin 面是否启用」这一位，映射时不产生资源根）
    if !disable_bundled {
        roots.push(SkillRoot {
            path: PathBuf::new(),
            source: SkillSource::Builtin,
            plugin_name: None,
        });
    }

    roots
}
