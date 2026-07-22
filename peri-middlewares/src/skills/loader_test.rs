//! Tests for loader_skills

use tempfile::tempdir;

use super::*;

fn write_skill(dir: &Path, name: &str, desc: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let content = format!(
        "---\nname: '{}'\ndescription: '{}'\n---\n\n# {}\n\nContent here.\n",
        name, desc, name
    );
    std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
}

/// 在指定 path 直接写一个 SKILL.md（path 含完整文件名）
fn write_skill_file(path: &Path, name: &str, desc: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let content = format!(
        "---\nname: '{}'\ndescription: '{}'\n---\n\n# {}\n\nContent here.\n",
        name, desc, name
    );
    std::fs::write(path, content).unwrap();
}

#[test]
fn test_scan_skill_roots_nested() {
    let root = tempdir().unwrap();
    // 构造 6 层嵌套：root/a/b/c/d/e/f/SKILL.md（depth=6 在范围内）
    let deep = root
        .path()
        .join("a")
        .join("b")
        .join("c")
        .join("d")
        .join("e")
        .join("f");
    write_skill_file(&deep.join("SKILL.md"), "deep-skill", "deep nested");

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "deep-skill");
    assert_eq!(skills[0].source, SkillSource::Project);
}

#[test]
fn test_scan_skill_roots_depth_limit() {
    let root = tempdir().unwrap();
    // 构造 7 层嵌套：root/a/b/c/d/e/f/g/SKILL.md（depth=7 超出限制）
    let too_deep = root
        .path()
        .join("a")
        .join("b")
        .join("c")
        .join("d")
        .join("e")
        .join("f")
        .join("g");
    write_skill_file(&too_deep.join("SKILL.md"), "too-deep", "ignored");

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert!(
        skills.is_empty(),
        "7 层深度的 SKILL.md 应被 MAX_SCAN_DEPTH=6 拒绝"
    );
}

#[test]
fn test_scan_skill_roots_leaf_semantics() {
    let root = tempdir().unwrap();
    // dir 含 SKILL.md，且 dir/sub 也含 SKILL.md
    // 叶子语义：dir/SKILL.md 加载，dir/sub/SKILL.md 不应被扫描
    let dir = root.path().join("my-skill");
    write_skill_file(&dir.join("SKILL.md"), "parent", "parent skill");
    write_skill_file(&dir.join("sub").join("SKILL.md"), "child", "child skill");

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert_eq!(
        skills.len(),
        1,
        "叶子语义应停止下钻，子目录 SKILL.md 不被扫描"
    );
    assert_eq!(skills[0].name, "parent");
}

#[test]
fn test_scan_skill_roots_dir_count_limit() {
    let root = tempdir().unwrap();
    // 构造 5 个子目录，每个含 SKILL.md
    for i in 0..5 {
        let dir = root.path().join(format!("skill-{i}"));
        write_skill_file(&dir.join("SKILL.md"), &format!("s{i}"), "x");
    }

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    // 注入 max_dirs=3（root 自身算 1，最多再扫 2 个子目录）
    let skills = scan_skill_roots_with_limits(&roots, 6, 3);
    assert!(
        skills.len() <= 2,
        "max_dirs=3 时 root 自身占 1，剩余配额 2，扫描结果应 ≤ 2，实际 {}",
        skills.len()
    );
}

#[test]
#[cfg(unix)] // symlink 在 Windows 需要管理员权限，仅在 unix 测试
fn test_scan_skill_roots_symlink_followed() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let real_target = tempdir().unwrap();
    // real_target/my-skill/SKILL.md
    write_skill_file(
        &real_target.path().join("my-skill").join("SKILL.md"),
        "linked",
        "via symlink",
    );
    // root/linked → real_target（symlink 应被跟随）
    symlink(real_target.path(), root.path().join("linked")).unwrap();

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::User,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "linked");
}

#[test]
#[cfg(unix)]
fn test_scan_skill_roots_symlink_loop() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    // 构造环：root/a/loop → root/a（自指）
    let a_dir = root.path().join("a");
    std::fs::create_dir_all(&a_dir).unwrap();
    symlink(&a_dir, a_dir.join("loop")).unwrap();
    write_skill_file(&a_dir.join("SKILL.md"), "real", "real skill");

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    // 不应无限递归（防环 canonicalize 命中 visited 后退出）
    let skills = scan_skill_roots(&roots);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "real");
}

#[test]
fn test_scan_skill_roots_dedup_across_roots() {
    let user_dir = tempdir().unwrap();
    let project_dir = tempdir().unwrap();
    // 两个 root 都有同名 skill "common"
    write_skill_file(
        &user_dir.path().join("common").join("SKILL.md"),
        "common",
        "from user",
    );
    write_skill_file(
        &project_dir.path().join("common").join("SKILL.md"),
        "common",
        "from project",
    );

    let roots = vec![
        SkillRoot {
            path: user_dir.path().to_path_buf(),
            source: SkillSource::User,
            plugin_name: None,
        },
        SkillRoot {
            path: project_dir.path().to_path_buf(),
            source: SkillSource::Project,
            plugin_name: None,
        },
    ];
    let skills = scan_skill_roots(&roots);
    assert_eq!(skills.len(), 1);
    assert_eq!(
        skills[0].description, "from user",
        "User 应先于 Project 胜出"
    );
    assert_eq!(skills[0].source, SkillSource::User);
}

#[test]
fn test_scan_skill_roots_dedup_within_root() {
    let root = tempdir().unwrap();
    // 同一 root 下两个不同子目录都有 "dup" skill
    write_skill_file(&root.path().join("a").join("SKILL.md"), "dup", "from a");
    write_skill_file(&root.path().join("b").join("SKILL.md"), "dup", "from b");

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert_eq!(skills.len(), 1);
    // subdirs.sort() 后 "a" 排在 "b" 前，应胜出
    assert_eq!(skills[0].description, "from a");
}

#[test]
fn test_scan_skill_roots_source_tag() {
    let root = tempdir().unwrap();
    write_skill_file(&root.path().join("p").join("SKILL.md"), "x", "y");

    let roots = vec![SkillRoot {
        path: root.path().to_path_buf(),
        source: SkillSource::Plugin,
        plugin_name: Some("my-plugin".to_string()),
    }];
    let skills = scan_skill_roots(&roots);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].source, SkillSource::Plugin);
    assert_eq!(skills[0].plugin_name.as_deref(), Some("my-plugin"));
}

#[test]
fn test_load_skill_metadata() {
    let dir = tempdir().unwrap();
    write_skill(dir.path(), "my-skill", "A test skill");
    let skill_file = dir.path().join("my-skill").join("SKILL.md");
    let meta = load_skill_metadata(&skill_file).unwrap();
    assert_eq!(meta.name, "my-skill");
    assert_eq!(meta.description, "A test skill");
}

#[test]
fn test_list_skills_dedup() {
    let dir1 = tempdir().unwrap();
    let dir2 = tempdir().unwrap();
    write_skill(dir1.path(), "skill-a", "from dir1");
    write_skill(dir1.path(), "skill-b", "from dir1");
    write_skill(dir2.path(), "skill-a", "from dir2"); // 重复，应被忽略
    write_skill(dir2.path(), "skill-c", "from dir2");

    let skills = list_skills(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()]);
    assert_eq!(skills.len(), 3);

    let skill_a = skills.iter().find(|s| s.name == "skill-a").unwrap();
    assert_eq!(skill_a.description, "from dir1"); // dir1 优先
}

#[test]
fn test_resolve_skill_roots_returns_standard_paths() {
    let cwd = "/tmp/test-project";
    let roots = resolve_skill_roots(cwd, vec![], false);
    assert!(
        roots
            .iter()
            .any(|r| r.path.ends_with(".claude/skills") && r.source == SkillSource::User),
        "应包含 ~/.claude/skills 作为 User source"
    );
    assert!(
        roots
            .iter()
            .any(|r| r.path == Path::new("/tmp/test-project/.claude/skills")
                && r.source == SkillSource::Project),
        "应包含项目 .claude/skills 作为 Project source"
    );
}

#[test]
fn test_resolve_skill_roots_includes_plugin_roots() {
    let extra = tempfile::tempdir().unwrap();
    let plugin_root = SkillRoot {
        path: extra.path().to_path_buf(),
        source: SkillSource::Plugin,
        plugin_name: Some("test-plugin".to_string()),
    };
    let roots = resolve_skill_roots("/tmp", vec![plugin_root], false);
    assert!(
        roots.iter().any(|r| r.path == extra.path().to_path_buf()
            && r.source == SkillSource::Plugin
            && r.plugin_name.as_deref() == Some("test-plugin")),
        "应包含传入的 plugin root"
    );
}

#[test]
fn test_resolve_skill_roots_skips_nonexistent_plugin_roots() {
    let nonexistent = SkillRoot {
        path: PathBuf::from("/nonexistent/path"),
        source: SkillSource::Plugin,
        plugin_name: None,
    };
    let roots = resolve_skill_roots("/tmp", vec![nonexistent], false);
    assert!(
        !roots
            .iter()
            .any(|r| r.path.to_str() == Some("/nonexistent/path")),
        "不存在的 plugin root 应被跳过"
    );
}

#[test]
fn test_resolve_skill_roots_includes_builtin_when_enabled() {
    let roots = resolve_skill_roots("/tmp", vec![], false);
    assert!(
        roots.iter().any(|r| r.source == SkillSource::Builtin),
        "disable_bundled=false 时应含 Builtin root"
    );
}

#[test]
fn test_resolve_skill_roots_excludes_builtin_when_disabled() {
    let roots = resolve_skill_roots("/tmp", vec![], true);
    assert!(
        !roots.iter().any(|r| r.source == SkillSource::Builtin),
        "disable_bundled=true 时不应含 Builtin root"
    );
}

#[test]
fn test_resolve_skill_roots_builtin_is_lowest_priority() {
    // Builtin root 应在末尾（最后被扫描，最低优先级）
    let roots = resolve_skill_roots("/tmp", vec![], false);
    let builtin_idx = roots
        .iter()
        .position(|r| r.source == SkillSource::Builtin)
        .expect("应含 Builtin root");
    assert_eq!(
        builtin_idx,
        roots.len() - 1,
        "Builtin root 应在列表末尾（最低优先级）"
    );
}

#[test]
fn test_scan_skill_roots_builtin_returns_metadata() {
    // 仅含 Builtin root 时，应返回 BUILTIN_SKILLS 的 metadata
    let roots = vec![SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Builtin,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert!(
        skills.iter().any(|s| s.name == "use-artifacts"
            && s.source == SkillSource::Builtin
            && s.path == Path::new("<builtin>/use-artifacts")),
        "应含 use-artifacts 的 Builtin metadata，path=<builtin>/use-artifacts，实际: {:?}",
        skills
    );
}

#[test]
fn test_scan_skill_roots_builtin_empty_path() {
    // 即使 path 字段是空（占位），Builtin source 也应正常扫描
    // （特判分支跳过 is_dir() 检查）
    let roots = vec![SkillRoot {
        path: PathBuf::new(), // 空路径占位
        source: SkillSource::Builtin,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    assert!(!skills.is_empty(), "Builtin source 不应因 path 为空被跳过");
}

#[test]
fn test_scan_skill_roots_user_overrides_builtin() {
    // User root 有同名 use-artifacts，应胜出（User 描述 + source=User）
    let user_dir = tempdir().unwrap();
    write_skill_file(
        &user_dir.path().join("use-artifacts").join("SKILL.md"),
        "use-artifacts",
        "from user override",
    );

    let roots = vec![
        SkillRoot {
            path: user_dir.path().to_path_buf(),
            source: SkillSource::User,
            plugin_name: None,
        },
        SkillRoot {
            path: PathBuf::new(),
            source: SkillSource::Builtin,
            plugin_name: None,
        },
    ];
    let skills = scan_skill_roots(&roots);
    let ua = skills.iter().find(|s| s.name == "use-artifacts").unwrap();
    assert_eq!(ua.source, SkillSource::User, "User 应覆盖 Builtin");
    assert_eq!(ua.description, "from user override");
}
