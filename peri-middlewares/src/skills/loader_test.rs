use super::{resolve_skill_roots, SkillRoot, SkillSource};

#[test]
#[serial_test::serial]
fn roots_forward_missing_plugin_path_and_preserve_priority_and_bundled_switch() {
    let temporary = tempfile::tempdir().unwrap();
    let plugin_path = temporary.path().join("missing-plugin-skills");
    let plugin_root = SkillRoot {
        path: plugin_path,
        source: SkillSource::Plugin,
        plugin_name: Some("retained-plugin".to_owned()),
    };
    let cwd = temporary.path().to_str().unwrap();
    for disable_bundled in [false, true] {
        let roots = resolve_skill_roots(cwd, vec![plugin_root.clone()], disable_bundled);
        let mut expected = vec![SkillSource::User, SkillSource::Project, SkillSource::Plugin];
        if !disable_bundled {
            expected.push(SkillSource::Builtin);
        }
        assert_eq!(
            roots.iter().map(|root| root.source).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(roots[0].path, crate::plugin::claude_home().join("skills"));
        assert_eq!(roots[1].path, temporary.path().join(".claude/skills"));
        assert_eq!(roots[2].path, plugin_root.path);
        assert_eq!(roots[2].plugin_name, plugin_root.plugin_name);
    }
}
