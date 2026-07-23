use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use gray_matter::{engine::YAML, Matter};
use serde::Deserialize;

/// Skill 来源 scope，用于 metadata 标签与日志诊断
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// ~/.claude/skills
    User,
    /// ~/.peri/settings.json::skillsDir
    Global,
    /// {cwd}/.claude/skills
    Project,
    /// 插件 manifest 声明的 skill 目录
    Plugin,
    /// 随二进制分发的内置 skill（include_str! 编译期嵌入）
    Builtin,
}

/// 带 source 标签的 skill 根目录
#[derive(Debug, Clone)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub source: SkillSource,
    /// 仅 Plugin source 填，用于日志诊断
    pub plugin_name: Option<String>,
}

/// 递归深度上限（相对每个 skill root）
pub const MAX_SCAN_DEPTH: usize = 6;

/// 单 root 目录数上限
pub const MAX_SKILLS_DIRS_PER_ROOT: usize = 1000;

/// 永远不会包含 SKILL.md 的目录名（跳过以加速扫描，避免递归进入无关子目录）
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".tox",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "outputs",
    "old_skill",
];

fn should_skip_dir(dir_name: &str) -> bool {
    SKIP_DIR_NAMES.contains(&dir_name)
}

/// Skill 元数据（来自 SKILL.md frontmatter）
#[derive(Debug, Clone)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// skill 来源（由 scan_dir_recursive 注入，load_skill_metadata 内填占位）
    pub source: SkillSource,
    /// 仅 Plugin source 填，其他为 None
    pub plugin_name: Option<String>,
}

impl Default for SkillMetadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            path: PathBuf::new(),
            source: SkillSource::Project,
            plugin_name: None,
        }
    }
}

/// frontmatter 反序列化结构
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

/// 加载单个 SKILL.md，解析 frontmatter，返回元数据
///
/// **description trim**：YAML `>`（折叠标量）和 `|`（字面标量）会在末尾保留 `\n`，
/// 下游 `build_summary` 把 description 拼到 Markdown list item 末尾，尾随 `\n` 会
/// 让 list 渲染断裂成段落。这里 trim 尾随空白与 `parse_builtin_frontmatter` 保持一致。
pub fn load_skill_metadata(path: &Path) -> Option<SkillMetadata> {
    let content = std::fs::read_to_string(path).ok()?;
    let matter = Matter::<YAML>::new();
    let result: gray_matter::ParsedEntity = matter.parse(&content).ok()?;

    let data = result.data?;
    let fm: SkillFrontmatter = data.deserialize().ok()?;

    Some(SkillMetadata {
        name: fm.name,
        description: fm.description.trim().to_string(),
        path: path.to_path_buf(),
        // 占位值：实际 source/plugin_name 由 scan_dir_recursive 中的 insert_skill 覆盖
        source: SkillSource::Project,
        plugin_name: None,
    })
}

/// 统一的 skill 扫描入口。
///
/// 对每个 root 独立递归扫描（深度上限 `MAX_SCAN_DEPTH`、目录数上限
/// `MAX_SKILLS_DIRS_PER_ROOT`、symlink 跟随 + canonicalize 防环、叶子语义：
/// dir 含 SKILL.md 则加载并停止下钻）。跨 root 同名去重：roots 顺序决定优先级
/// （先到先得）。
///
/// **Builtin 特判**：`SkillSource::Builtin` 的 root 跳过磁盘扫描（path 字段为占位
/// `PathBuf::new()`），直接从编译期常量 `crate::skills::builtin::BUILTIN_SKILLS`
/// 加载，构造虚拟路径 `<builtin>/<name>`（不对应真实文件，加载全文需通过
/// `SkillPreloadMiddleware` 的 Builtin 特判路由）。
pub fn scan_skill_roots(roots: &[SkillRoot]) -> Vec<SkillMetadata> {
    scan_skill_roots_impl(roots, MAX_SCAN_DEPTH, MAX_SKILLS_DIRS_PER_ROOT)
}

/// 带参数化上限的扫描入口（仅供测试注入小值，prod 用 `scan_skill_roots`）
#[cfg(test)]
pub(crate) fn scan_skill_roots_with_limits(
    roots: &[SkillRoot],
    max_depth: usize,
    max_dirs: usize,
) -> Vec<SkillMetadata> {
    scan_skill_roots_impl(roots, max_depth, max_dirs)
}

fn scan_skill_roots_impl(
    roots: &[SkillRoot],
    max_depth: usize,
    max_dirs: usize,
) -> Vec<SkillMetadata> {
    let mut seen: HashMap<String, SkillMetadata> = HashMap::new();
    let mut ordered: Vec<String> = Vec::new();

    for root in roots {
        // Builtin 特判：跳过磁盘扫描，直接从编译期常量数组加载。
        // path 字段对 Builtin 是占位（PathBuf::new()），不走 is_dir() 检查。
        if matches!(root.source, SkillSource::Builtin) {
            for skill in crate::skills::builtin::BUILTIN_SKILLS {
                let parsed = crate::skills::builtin::parse_builtin_frontmatter(skill.content);
                let Some((name, description)) = parsed else {
                    tracing::warn!("builtin skill {} frontmatter 解析失败，跳过", skill.name);
                    continue;
                };
                let meta = SkillMetadata {
                    name,
                    description,
                    path: PathBuf::from(format!("<builtin>/{}", skill.name)),
                    source: SkillSource::Builtin,
                    plugin_name: None,
                };
                insert_skill(meta, root, &mut seen, &mut ordered);
            }
            continue;
        }

        if !root.path.is_dir() {
            continue;
        }
        // 每 root 独立 visited/dir_count，避免跨 root 配额污染与误判环
        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut dir_count: usize = 0;
        scan_dir_recursive(
            &root.path,
            0,
            max_depth,
            max_dirs,
            root,
            &mut visited,
            &mut dir_count,
            &mut seen,
            &mut ordered,
        );
    }

    ordered
        .into_iter()
        .filter_map(|n| seen.remove(&n))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn scan_dir_recursive(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    max_dirs: usize,
    root: &SkillRoot,
    visited: &mut HashSet<PathBuf>,
    dir_count: &mut usize,
    seen: &mut HashMap<String, SkillMetadata>,
    ordered: &mut Vec<String>,
) {
    if depth > max_depth {
        return;
    }
    if *dir_count >= max_dirs {
        return;
    }

    // 跳过不可能包含 SKILL.md 的子目录（.git, node_modules, target, dist, outputs 等）。
    // 仅 depth > 0 时生效：根目录（depth=0）不跳过，避免 TempDir 的 `.tmp...` 被误判。
    if depth > 0 {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            if should_skip_dir(name) {
                return;
            }
        }
    }

    // 防环：canonicalize 后入 visited（失败时回退到原 path）
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canon) {
        return;
    }
    *dir_count += 1;

    // 叶子语义：dir 自己含 SKILL.md 则加载，不再下钻
    let skill_file = dir.join("SKILL.md");
    if skill_file.is_file() {
        if let Some(meta) = load_skill_metadata(&skill_file) {
            insert_skill(meta, root, seen, ordered);
        }
        return;
    }

    // 容器：递归扫描子目录（is_dir 自动跟随 symlink）
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut subdirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    for sub in subdirs {
        scan_dir_recursive(
            &sub,
            depth + 1,
            max_depth,
            max_dirs,
            root,
            visited,
            dir_count,
            seen,
            ordered,
        );
    }
}

fn insert_skill(
    mut meta: SkillMetadata,
    root: &SkillRoot,
    seen: &mut HashMap<String, SkillMetadata>,
    ordered: &mut Vec<String>,
) {
    meta.source = root.source;
    meta.plugin_name = root.plugin_name.clone();
    if seen.contains_key(&meta.name) {
        return;
    }
    ordered.push(meta.name.clone());
    seen.insert(meta.name.clone(), meta);
}

/// 扫描多个目录，返回所有可用 skill 元数据
///
/// 同名 skill 以先出现的为准（dirs 中靠前的目录优先）。
/// 已废弃：仅向后兼容旧测试与少量旧调用点。
/// 建议改用 `scan_skill_roots` + `resolve_skill_roots`。
///
/// 把传入的 PathBuf 视为 Project source（无来源标签信息）。
pub fn list_skills(dirs: &[PathBuf]) -> Vec<SkillMetadata> {
    let roots: Vec<SkillRoot> = dirs
        .iter()
        .map(|d| SkillRoot {
            path: d.clone(),
            source: SkillSource::Project,
            plugin_name: None,
        })
        .collect();
    scan_skill_roots(&roots)
}

/// 统一解析 skill 根列表，按优先级返回 `SkillRoot`。
///
/// 顺序即去重优先级：User → Global → Project → Plugin → Builtin（先到先得）。
/// 这是 skill 目录解析的 single source of truth，`SkillsMiddleware` 和
/// `SkillPreloadMiddleware` 都应委托此函数。
///
/// `disable_bundled=true` 时跳过 Builtin root（用户通过 settings.json
/// `config.disableBundledSkills: true` 关闭内置 skill）。
pub fn resolve_skill_roots(
    cwd: &str,
    plugin_roots: Vec<SkillRoot>,
    disable_bundled: bool,
) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    // 1. User
    if let Some(h) = dirs_next::home_dir() {
        roots.push(SkillRoot {
            path: h.join(".claude").join("skills"),
            source: SkillSource::User,
            plugin_name: None,
        });
    }

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

    // 5. Builtin（最低优先级，path 字段占位，scan 阶段特判跳过 is_dir()）
    if !disable_bundled {
        roots.push(SkillRoot {
            path: PathBuf::new(),
            source: SkillSource::Builtin,
            plugin_name: None,
        });
    }

    roots
}

/// 公共 skill 内容查找函数 —— 统一入口，供 SkillTool 和 SkillPreloadMiddleware 复用。
///
/// 按 skill 名称查找（大小写无关精确匹配），返回 `(SkillMetadata, 文件内容)`。
/// 查找范围由 `resolve_skill_roots` 决定：User → Global → Project → Plugin → Builtin。
///
/// # 返回值
/// - `Some((metadata, content))` — 找到 skill，metadata.path 为 SKILL.md 绝对路径
/// - `None` — 未找到匹配的 skill
pub fn find_skill_content(
    cwd: &str,
    plugin_roots: Vec<SkillRoot>,
    disable_bundled: bool,
    skill_name: &str,
) -> Option<(SkillMetadata, String)> {
    let roots = resolve_skill_roots(cwd, plugin_roots, disable_bundled);
    let skills = scan_skill_roots(&roots);

    let name_lower = skill_name.to_lowercase();
    let found = skills
        .iter()
        .find(|s| s.name.to_lowercase() == name_lower)?;

    let content = if matches!(found.source, SkillSource::Builtin) {
        crate::skills::builtin::BUILTIN_SKILLS
            .iter()
            .find(|bs| bs.name == found.name)
            .map(|bs| bs.content.to_string())?
    } else {
        std::fs::read_to_string(&found.path).ok()?
    };

    Some((found.clone(), content))
}

/// 在预扫描的 skills 列表中查找并加载 skill 内容（避免重复磁盘扫描）。
///
/// 与 [`find_skill_content`] 功能相同，但接受已扫描的 `Vec<SkillMetadata>`
/// 而非自行调用 `scan_skill_roots`。调用方需自行维护缓存生命周期。
pub fn find_skill_in_list(
    skills: &[SkillMetadata],
    skill_name: &str,
) -> Option<(SkillMetadata, String)> {
    let name_lower = skill_name.to_lowercase();
    let found = skills
        .iter()
        .find(|s| s.name.to_lowercase() == name_lower)?;

    let content = if matches!(found.source, SkillSource::Builtin) {
        crate::skills::builtin::BUILTIN_SKILLS
            .iter()
            .find(|bs| bs.name == found.name)
            .map(|bs| bs.content.to_string())?
    } else {
        std::fs::read_to_string(&found.path).ok()?
    };

    Some((found.clone(), content))
}

#[cfg(test)]
#[path = "loader_test.rs"]
mod tests;
