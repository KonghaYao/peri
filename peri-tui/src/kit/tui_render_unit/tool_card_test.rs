use super::*;

/// [回归测试] 无分配格式化哈希须与原公式一致，含 Unicode、presentation、fold 及秒级时长。
#[test]
fn test_tool_hash_writer_preserves_format_hash_contract() {
    for fold in [
        FoldState::Collapsed,
        FoldState::Preview,
        FoldState::Expanded,
    ] {
        for duration in [None, Some(0), Some(999), Some(1000), Some(12345)] {
            let mut card = TuiToolCard {
                tool_id: "tool|中文".into(),
                tool_name: "Skill".into(),
                input_summary: "输入😀|\n".repeat(128),
                output_summary: "结果".repeat(128),
                is_error: true,
                is_running: duration.is_some(),
                running_duration_ms: duration,
                completed_duration_ms: Some(1234),
                diff: None,
                presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
                    name: "技能😀".into(),
                }),
                fold,
                user_modified: true,
                content_hash: 0,
                tool_calls_count: 0,
            };
            let old_input = format!(
                "{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}",
                card.tool_id,
                card.tool_name,
                card.input_summary,
                card.output_summary,
                card.is_error,
                card.is_running,
                card.running_duration_ms.map(|ms| ms / 1000),
                card.completed_duration_ms.map(|ms| ms / 1000),
                card.presentation,
                card.fold,
                card.user_modified
            );
            card.recompute_hash();
            assert_eq!(
                card.content_hash,
                tui_hash_combine(tui_hash_str(&old_input), card.diff_code())
            );
        }
    }
}
