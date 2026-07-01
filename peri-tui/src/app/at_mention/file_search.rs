use std::ffi::OsString;
use std::path::{Path, PathBuf};

use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

/// 文件系统条目
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: OsString,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// 查询当前目录下的条目（跳过隐藏文件，目录优先排序）
pub fn read_dir_entries(dir: &Path) -> Vec<Entry> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut entries: Vec<Entry> = rd
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            !name_str.starts_with('.')
        })
        .filter_map(|e| {
            let ft = e.file_type().ok()?;
            Some(Entry {
                name: e.file_name(),
                is_dir: ft.is_dir(),
                is_symlink: ft.is_symlink(),
            })
        })
        .collect();

    // 目录优先，其次按名字排序
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    entries
}

/// 对条目列表做模糊匹配，返回匹配到的条目引用（保持排序）
pub fn fuzzy_match_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    if query.is_empty() {
        return entries.iter().collect();
    }

    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(&Entry, i64)> = entries
        .iter()
        .filter_map(|e| {
            let name_str = e.name.to_string_lossy();
            let score = matcher.fuzzy_match(&name_str, query).unwrap_or(0);
            if score > 0 { Some((e, score)) } else { None }
        })
        .collect();

    // score 降序 → 目录优先 → name 长度升序
    scored.sort_by(|(a, sa), (b, sb)| {
        sb.cmp(sa)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.name.len().cmp(&b.name.len()))
    });

    scored.into_iter().map(|(e, _)| e).collect()
}

/// 解析 @ 后的 query，拆分为目录部分和搜索部分
/// 返回 (dir_part, query_part)。不处理回退逻辑。
pub fn parse_at_query(query: &str) -> Option<(String, String)> {
    if query.is_empty() {
        return None;
    }
    if let Some(slash_pos) = query.rfind('/') {
        let dir_part = query[..=slash_pos].to_string();
        let query_part = query[slash_pos + 1..].to_string();
        Some((dir_part, query_part))
    } else {
        Some((String::new(), query.to_string()))
    }
}

/// 解析目录路径，不存在时向上回退到最近存在的目录
/// 返回 (resolved_dir, fallback_query)。
/// fallback_query 是回退掉的不存在部分，需与 query_part 合并做 fuzzy。
pub fn resolve_dir(cwd: &Path, dir_part: &str) -> (PathBuf, String) {
    let mut curr = cwd.join(dir_part);
    let mut fallback = String::new();

    while !curr.is_dir() {
        if let Some(name) = curr.file_name() {
            let name_str = name.to_string_lossy();
            if fallback.is_empty() {
                fallback = name_str.to_string();
            } else {
                fallback = format!("{}/{}", name_str, fallback);
            }
        }
        if let Some(parent) = curr.parent() {
            curr = parent.to_path_buf();
        } else {
            let cwd_owned = cwd.to_path_buf();
            if !cwd_owned.is_dir() {
                return (cwd_owned, fallback);
            }
            curr = cwd_owned;
            break;
        }
    }

    (curr, fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_read_dir_entries_basic() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::write(base.join("main.rs"), "").unwrap();
        fs::write(base.join("lib.rs"), "").unwrap();
        fs::create_dir(base.join("src")).unwrap();

        let entries = read_dir_entries(base);
        let names: Vec<&str> = entries.iter().map(|e| e.name.to_str().unwrap()).collect();
        assert!(names.contains(&"src"));
        assert!(names.contains(&"main.rs"));
        assert!(names.contains(&"lib.rs"));
        // src 应在 main.rs 前面（is_dir 优先）
        let src_pos = entries.iter().position(|e| e.name == "src").unwrap();
        let main_pos = entries.iter().position(|e| e.name == "main.rs").unwrap();
        assert!(src_pos < main_pos, "目录应排在文件前面");
    }

    #[test]
    fn test_read_dir_skips_hidden() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "").unwrap();
        fs::write(dir.path().join("visible.rs"), "").unwrap();

        let entries = read_dir_entries(dir.path());
        let names: Vec<&str> = entries.iter().map(|e| e.name.to_str().unwrap()).collect();
        assert!(!names.iter().any(|n| n.starts_with('.')), "应跳过隐藏文件");
        assert!(names.contains(&"visible.rs"));
    }

    #[test]
    fn test_fuzzy_match_entries_with_query() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main_logic.rs"), "").unwrap();
        fs::write(dir.path().join("util_test.rs"), "").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let entries = read_dir_entries(dir.path());

        let results = fuzzy_match_entries(&entries, "main");
        assert!(!results.is_empty(), "应匹配到 main_logic.rs");
        assert!(results.iter().any(|e| e.name == "main_logic.rs"));
    }

    #[test]
    fn test_fuzzy_match_entries_empty_query() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("readme.md"), "").unwrap();
        let entries = read_dir_entries(dir.path());

        let results = fuzzy_match_entries(&entries, "");
        assert_eq!(results.len(), 2);
        assert!(results[0].is_dir, "第一个条目应为目录");
    }

    #[test]
    fn test_resolve_dir_exact_match() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::create_dir_all(base.join("side-projects/git-stats/src")).unwrap();

        let (resolved, fallback) = resolve_dir(base, "side-projects/git-stats/");
        assert!(resolved.ends_with("side-projects/git-stats"));
        assert_eq!(fallback, "");
    }

    #[test]
    fn test_resolve_dir_fallback() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        fs::create_dir_all(base.join("side-projects")).unwrap();

        let (resolved, fallback) = resolve_dir(base, "side-projects/nonex/sr");
        assert!(resolved.ends_with("side-projects"));
        assert_eq!(fallback, "nonex/sr");
    }

    #[test]
    fn test_resolve_dir_all_nonexistent() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let (resolved, fallback) = resolve_dir(base, "nonex/sub");
        assert_eq!(resolved, base);
        assert_eq!(fallback, "nonex/sub");
    }

    #[test]
    fn test_parse_at_query_simple() {
        let (dir_part, query_part) = parse_at_query("side-projects/git-stats/sr").unwrap();
        assert_eq!(dir_part, "side-projects/git-stats/");
        assert_eq!(query_part, "sr");
    }

    #[test]
    fn test_parse_at_query_no_slash() {
        let (dir_part, query_part) = parse_at_query("sr").unwrap();
        assert_eq!(dir_part, "");
        assert_eq!(query_part, "sr");
    }

    #[test]
    fn test_parse_at_query_trailing_slash() {
        let (dir_part, query_part) = parse_at_query("side-projects/").unwrap();
        assert_eq!(dir_part, "side-projects/");
        assert_eq!(query_part, "");
    }
}
