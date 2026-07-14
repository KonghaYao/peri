use super::*;

#[test]
fn test_session_id_unique_and_ordered() {
    let id1 = SessionId::new();
    let id2 = SessionId::new();
    assert_ne!(id1, id2, "SessionId 必须唯一");
    // UUID v7 时间有序
    assert!(id2.as_uuid() >= id1.as_uuid(), "SessionId 应时间有序");
}

#[test]
fn test_session_id_default() {
    let id = SessionId::default();
    assert_ne!(id.as_uuid(), uuid::Uuid::nil(), "默认值应生成有效 UUID");
}

#[test]
fn test_session_id_display() {
    let id = SessionId::new();
    let s = format!("{}", id);
    assert_eq!(s, id.as_uuid().to_string(), "Display 应输出 UUID 字符串");
}

#[test]
fn test_frozen_context_builder_defaults() {
    let ctx = FrozenContext::builder().build();
    assert!(ctx.system_prompt.is_empty(), "默认 system_prompt 应为空");
    assert!(ctx.claude_md.is_empty(), "默认 claude_md 应为空");
    assert!(ctx.skill_summary.is_empty(), "默认 skill_summary 应为空");
    assert!(ctx.date.is_empty(), "默认 date 应为空");
    assert!(ctx.language.is_none(), "默认 language 应为 None");
}

#[test]
fn test_frozen_context_builder_full() {
    let ctx = FrozenContext::builder()
        .system_prompt("You are a helpful assistant.")
        .claude_md("# Project Rules\n- Use Rust")
        .skill_summary("commit, review")
        .date("2026-06-24")
        .language(Some("zh-CN"))
        .build();

    assert_eq!(&*ctx.system_prompt, "You are a helpful assistant.");
    assert_eq!(&*ctx.claude_md, "# Project Rules\n- Use Rust");
    assert_eq!(&*ctx.skill_summary, "commit, review");
    assert_eq!(&*ctx.date, "2026-06-24");
    assert_eq!(ctx.language.as_deref(), Some("zh-CN"));
}

#[test]
fn test_frozen_context_builder_language_none() {
    // language(None) 显式清除 → 最终为 None
    let ctx = FrozenContext::builder().language(None::<&str>).build();
    assert!(ctx.language.is_none());
}

#[test]
fn test_frozen_context_clone_shares_arcs() {
    let ctx = FrozenContext::builder().system_prompt("prompt").build();
    let cloned = ctx.clone();
    // Arc::ptr_eq 检查是否共享同一内存
    assert!(
        Arc::ptr_eq(&ctx.system_prompt, &cloned.system_prompt),
        "clone 应共享 Arc"
    );
}

#[test]
fn test_session_store_construction() {
    let cwd: Arc<str> = Arc::from("/tmp/project");
    let frozen = FrozenContext::builder()
        .system_prompt("test prompt")
        .claude_md("test claude_md")
        .build();
    let store = SessionStore::new(cwd.clone(), frozen, Some("thread-42".into()));

    assert_ne!(store.session_id.as_uuid(), uuid::Uuid::nil());
    assert_eq!(&*store.cwd, "/tmp/project");
    assert_eq!(&*store.frozen.system_prompt, "test prompt");
    assert_eq!(&*store.frozen.claude_md, "test claude_md");
    assert_eq!(store.thread_id.as_deref(), Some("thread-42"));
    assert!(!store.is_git_repo(), "默认 is_git_repo 应为 false");
}

#[test]
fn test_session_store_set_is_git_repo() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let store = SessionStore::new(cwd, frozen, None);

    assert!(!store.is_git_repo());
    store.set_is_git_repo(true);
    assert!(store.is_git_repo());
    store.set_is_git_repo(false);
    assert!(!store.is_git_repo());
}
