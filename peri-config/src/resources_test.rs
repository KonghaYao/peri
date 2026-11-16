use serde_json::json;

#[test]
fn disable_bundled_skills_defaults_to_false() {
    assert!(!super::resolve(&json!({})).disable_bundled_skills);
}

#[test]
fn resolves_global_top_level_value() {
    assert!(
        super::resolve(&json!({
            "disableBundledSkills": true
        }))
        .disable_bundled_skills
    );
}

#[test]
fn nested_global_value_takes_precedence_over_top_level() {
    assert!(
        !super::resolve(&json!({
            "config": { "disableBundledSkills": false },
            "disableBundledSkills": true
        }))
        .disable_bundled_skills
    );
}

#[test]
fn invalid_nested_value_keeps_nested_precedence() {
    assert!(
        !super::resolve(&json!({
            "config": { "disableBundledSkills": "invalid" },
            "disableBundledSkills": true
        }))
        .disable_bundled_skills
    );
}
