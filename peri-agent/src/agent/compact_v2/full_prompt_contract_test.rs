// Responsibility-specific cases share the original test namespace and fixtures.

#[test]
fn summary_instruction_lists_recovery_resources_before_current_state() {
    let prompt = crate::agent::compact_v2::full::summary_instruction(1024);

    assert!(prompt.contains("Target at most 1024 output tokens"));
    assert!(prompt.contains("2. **Recovery resources**"));
    assert!(prompt.contains("3. **Current state and next action**"));
    let recovery = prompt
        .find("Recovery resources")
        .expect("the find fixture result is required for this assertion");
    let current = prompt
        .find("Current state and next action")
        .expect("the find fixture result is required for this assertion");
    assert!(
        recovery < current,
        "Recovery resources 必须是优先级 2，位于当前状态之前"
    );
    assert!(prompt.contains("SkillTool(\"<exact canonical name>\")"));
    assert!(prompt.contains("body archived by this compaction"));
    assert!(prompt.contains("when retention is not explicitly shown"));
    assert!(prompt.contains("preloaded; do not reload"));
    assert!(prompt.contains("work which depends on their results waits"));
    assert!(prompt.contains("Contract: X. Recent actual: Y (deviation)."));
    assert!(prompt.contains("Recovery targets the contract"));
    assert!(prompt.contains("report it as unrecoverable"));
    // 重复 compact：恢复项是 Earlier summaries 合并规则的显式例外。
    assert!(prompt.contains("except Recovery resources"));
    assert!(prompt.contains("list every skill still active on every compaction"));
    assert!(prompt.contains("carry forward recovery items the previous summary left unfinished"));
    assert!(prompt.contains("never merge or omit a recovery item"));
    assert!(prompt.contains("record the user's latest choice"));
    assert!(prompt.contains("completing one work item does not end a session-wide mode"));
    assert!(prompt.contains("Distinguish skill activation from body visibility"));
    assert!(prompt.contains("Preserve an explicit user exit or override"));
}

#[test]
fn recovery_prompts_name_recovery_resources() {
    for prompt in [SUMMARY_RECOVERY_PROMPT, SUMMARY_FINAL_PROMPT] {
        assert!(prompt.contains("recovery resources"));
        assert!(prompt.contains("skills whose bodies were archived"));
        assert!(prompt.contains("contract-vs-actual deviation"));
        assert!(prompt.contains("Do not call tools"));
    }
    assert!(SUMMARY_RECOVERY_PROMPT.contains("cut off"));
    assert!(SUMMARY_RECOVERY_PROMPT.contains("Continue exactly from the end"));
}

#[test]
fn continuation_hint_is_single_line_and_gates_the_first_action() {
    let hint = crate::agent::compact_v2::CONTINUATION_HINT;

    assert!(!hint.contains('\n'), "续接标记必须保持单行");
    for fragment in [
        "The summary below",
        "before any other tool call",
        "SkillTool",
        "If a reload fails",
        "DiscoverSkillsTool",
        "cannot be recovered",
        "authoritative",
        "user instruction takes precedence",
    ] {
        assert!(hint.contains(fragment), "续接标记缺少 {fragment}: {hint}");
    }

    let message = crate::agent::compact_v2::full::build_summary_message(
        "<summary>body</summary>",
        crate::messages::MessageId::new(),
    );
    let content = message.content();
    assert_eq!(content.lines().next(), Some(hint), "首行必须是续接标记");
    assert!(
        content.starts_with(&format!("{hint}\n\n<summary>body</summary>")),
        "摘要正文必须位于续接标记之后"
    );
}
