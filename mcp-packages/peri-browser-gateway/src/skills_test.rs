use rmcp::model::SkillResources;

use super::*;

#[test]
fn the_catalog_holds_the_two_shipped_skills() {
    let skills = catalog().expect("bundled skills parse");
    let names: Vec<&str> = skills
        .iter()
        .map(|skill| skill.frontmatter.name.as_str())
        .collect();
    assert_eq!(names, vec!["browser-automation", "debugging"]);
    for skill in &skills {
        assert!(!skill.frontmatter.description.trim().is_empty());
        assert_eq!(skill.uri, skill_uri(&skill.frontmatter.name));
        assert!(
            skill.validate().is_ok(),
            "{:?}",
            skill.validate().unwrap_err()
        );
    }
}

#[test]
fn uris_have_the_sep_2640_shape() {
    assert_eq!(
        skill_uri("debugging"),
        "skill://peri-browser/debugging/SKILL.md"
    );
    assert_eq!(SKILL_ROOT, "skill://peri-browser");
}

#[test]
fn the_digest_is_sha256_of_the_exact_bytes() {
    let hash = digest(b"hello");
    assert_eq!(hash.len(), 7 + 64);
    assert!(hash.starts_with("sha256:"));
    assert!(hash[7..]
        .chars()
        .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()));
    assert_eq!(
        hash,
        "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_ne!(digest(b"hello"), digest(b"hello\n"));
}

#[test]
fn resources_match_the_bytes_served() {
    let resources = resources().expect("bundled skills parse");
    assert_eq!(resources.len(), 2);
    for resource in &resources {
        let raw = read(&resource.uri).expect("resource URI is readable");
        assert_eq!(resource.mime_type.as_deref(), Some(SKILL_MIME));
        assert_eq!(resource.size, Some(raw.bytes.len() as u64));
        assert_eq!(resource.name, raw.uri.rsplit('/').nth(1).unwrap());
    }
}

#[test]
fn reading_a_skill_returns_its_exact_bytes() {
    let uri = skill_uri("browser-automation");
    let raw = read(&uri).expect("known skill");
    assert_eq!(raw.uri, uri);
    let contents = raw.content().expect("bundled bytes are UTF-8");
    let ResourceContents::TextResourceContents {
        text, mime_type, ..
    } = &contents
    else {
        panic!("bundled skills are text");
    };
    assert_eq!(text.as_bytes(), raw.bytes);
    assert!(text.starts_with("---\nname: browser-automation\n"));
    assert!(text.contains("take_snapshot"));
    assert_eq!(mime_type.as_deref(), Some(SKILL_MIME));
}

#[test]
fn the_manifest_lists_its_own_skill_md() {
    let skill = skill(&skill_uri("debugging")).expect("known skill");
    let SkillResources::Files(files) = &skill.resources else {
        panic!("bundled skills have fixed files");
    };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].uri, skill.uri);
    assert_eq!(files[0].digest, digest(read(&skill.uri).unwrap().bytes));
}

#[test]
fn unknown_uris_are_reported_as_missing_elements() {
    let error = read("skill://peri-browser/nope/SKILL.md").unwrap_err();
    assert!(matches!(error, BrowserError::ElementNotFound(_)));
    assert_eq!(error.code(), "element_not_found");

    let error = skill("skill://peri-browser/nope/SKILL.md").unwrap_err();
    assert!(matches!(error, BrowserError::ElementNotFound(_)));
}

#[test]
fn frontmatter_parsing_requires_name_and_description() {
    let parsed = parse_frontmatter("---\nname: demo\ndescription: \"A demo\"\n---\nbody")
        .expect("valid frontmatter");
    assert_eq!(parsed.name, "demo");
    assert_eq!(parsed.description, "A demo");
    assert!(parse_frontmatter("no frontmatter").is_none());
    assert!(parse_frontmatter("---\nname: demo\n---\n").is_none());
    assert!(parse_frontmatter("---\ndescription: only\n---\n").is_none());
}
