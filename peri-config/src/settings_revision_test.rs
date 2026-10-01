use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use serde_json::Value;

use crate::{
    settings::{ConfigSource, SettingsError},
    ConfigurationError,
};

fn make_source() -> (tempfile::TempDir, ConfigSource, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project");
    let workspace_path = cwd.join(".peri/settings.json");
    let global_path = temp.path().join("global/settings.json");
    std::fs::create_dir_all(workspace_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(global_path.parent().unwrap()).unwrap();
    std::fs::write(
        &global_path,
        r#"{"config":{"providers":[{"id":"global","type":"openai","apiKey":"global-secret"}]},"langfuse":{"trace_sampling":0.5}}"#,
    )
    .unwrap();
    std::fs::write(&workspace_path, r#"{"config":{"language":"en"}}"#).unwrap();
    let source = ConfigSource::load_at(&cwd, global_path.clone()).unwrap();
    (temp, source, global_path, workspace_path)
}

#[test]
fn old_revision_cannot_save_old_draft_after_external_workspace_change_and_reload() {
    let (_temp, source, global_path, workspace_path) = make_source();
    let global_before = std::fs::read(&global_path).unwrap();
    let old_snapshot = source.snapshot().unwrap();
    let mut old_draft = old_snapshot.settings().clone();
    old_draft.config.language = Some("stale-draft".into());
    let external = br#"{"config":{"language":"ja"},"external":"preserve"}"#;
    std::fs::write(&workspace_path, external).unwrap();
    let mut new_draft = source.reload_merged().unwrap();
    let new_snapshot = source.snapshot().unwrap();
    assert_ne!(old_snapshot.revision(), new_snapshot.revision());
    assert_eq!(new_draft.config.language.as_deref(), Some("ja"));

    let error = source
        .save(old_snapshot.revision(), &old_draft)
        .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::Resolution(ConfigurationError::Conflict)
    ));
    assert_eq!(std::fs::read(&workspace_path).unwrap(), external.to_vec());
    assert!(Arc::ptr_eq(&new_snapshot, &source.snapshot().unwrap()));
    assert_eq!(
        old_snapshot.settings().config.language.as_deref(),
        Some("en")
    );

    new_draft.config.language = Some("fresh-draft".into());
    source.save(new_snapshot.revision(), &new_draft).unwrap();
    let content = std::fs::read_to_string(&workspace_path).unwrap();
    let document: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(document["config"]["language"], "fresh-draft");
    assert_eq!(document["external"], "preserve");
    assert!(!content.contains("global-secret"));
    assert_eq!(std::fs::read(&global_path).unwrap(), global_before);
    assert_ne!(
        source.snapshot().unwrap().revision(),
        new_snapshot.revision()
    );
}

#[test]
fn concurrent_saves_on_same_source_accept_only_one_draft_with_shared_revision() {
    let (_temp, source, global_path, workspace_path) = make_source();
    let global_before = std::fs::read(&global_path).unwrap();
    let source = Arc::new(source);
    let snapshot = source.snapshot().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = ["draft-a", "draft-b"]
        .into_iter()
        .map(|language| {
            let source = source.clone();
            let barrier = barrier.clone();
            let mut draft = snapshot.settings().clone();
            draft.config.language = Some(language.into());
            let revision = snapshot.revision();
            thread::spawn(move || {
                barrier.wait();
                (language, source.save(revision, &draft))
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|(_, result)| matches!(
                result,
                Err(SettingsError::Resolution(ConfigurationError::Conflict))
            ))
            .count(),
        1
    );
    let winner = outcomes
        .iter()
        .find(|(_, result)| result.is_ok())
        .unwrap()
        .0;
    let content = std::fs::read(&workspace_path).unwrap();
    let document: Value = serde_json::from_slice(&content).unwrap();
    assert_eq!(document["config"]["language"], winner);
    assert_eq!(
        source
            .snapshot()
            .unwrap()
            .settings()
            .config
            .language
            .as_deref(),
        Some(winner)
    );
    assert_eq!(snapshot.settings().config.language.as_deref(), Some("en"));
    assert_ne!(source.snapshot().unwrap().revision(), snapshot.revision());

    let error = source
        .save(snapshot.revision(), snapshot.settings())
        .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::Resolution(ConfigurationError::Conflict)
    ));
    assert_eq!(std::fs::read(&workspace_path).unwrap(), content);
    assert_eq!(std::fs::read(&global_path).unwrap(), global_before);
}
