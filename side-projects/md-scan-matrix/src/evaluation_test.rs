use super::*;

#[test]
fn test_defined_collision_is_accepted_but_missing_or_extra_tokens_fail() {
    let md = "复制了 \0IMG0\0 然后 ![a](u)";
    let case = Case {
        name: "collision",
        md,
        expect_hits: 1,
        note: "同编号冲突",
        collide: Some(TokenKind::Nul),
    };
    let expected = evaluate(&case);
    assert!(expected.pass(), "精确两次同编号 token 是声明的负面实验");
    assert!(expected.expected_collision);
    let mut experiment = run();
    experiment.matrix = vec![expected];
    assert_eq!(experiment.exit_code(), 0, "预期碰撞不应被当作 scanner 失败");
    assert!(
        !tokens_match_expectation("token disappeared", 1, TokenKind::Nul, Some(TokenKind::Nul)),
        "token 丢失不能通过反转保留断言被误算为预期碰撞"
    );
    let extra = Case {
        md: "\0IMG0\0 \0IMG0\0 ![a](u)",
        ..case
    };
    experiment.matrix = vec![evaluate(&extra)];
    assert!(
        !experiment.matrix[0].nul_retained,
        "第三次重复不属于预期负例"
    );
    assert_eq!(experiment.exit_code(), 1, "未预期重复必须使进程失败");
}

#[test]
fn test_literal_fallback_and_unexpected_missing_image_have_distinct_verdicts() {
    let fallback = Case {
        name: "invalid destination",
        md: "![a](my file.png)",
        expect_hits: 0,
        note: "CommonMark 非法 destination 保留文本",
        collide: None,
    };
    let outcome = evaluate(&fallback);
    assert_eq!(outcome.actual_hits, 0);
    assert!(
        outcome.leftover_ok && outcome.pass(),
        "无图片命中的字面语法残留符合预期"
    );
    let unexpected = Case {
        expect_hits: 1,
        ..fallback
    };
    let mut experiment = run();
    experiment.matrix = vec![evaluate(&unexpected)];
    assert_eq!(experiment.exit_code(), 1, "预期图片却未命中必须影响退出码");
}

#[test]
fn test_shortened_streaming_prefix_fails_without_panicking() {
    let previous = parse("first\n\nsecond\n\n![a](")
        .iter()
        .map(crate::scanner::shape)
        .collect::<Vec<_>>();
    let current = parse("first")
        .iter()
        .map(crate::scanner::shape)
        .collect::<Vec<_>>();
    let check = prefix_check("prefix shrink", &previous, &current);
    assert!(!check.passed, "当前块少于已闭合前缀必须判失败而不是越界");
    let mut experiment = run();
    experiment.streaming = vec![check];
    assert_eq!(experiment.exit_code(), 1, "F 组前缀失败不能只打印");
}

#[test]
fn test_completed_streaming_suffix_preserves_closed_prefix() {
    let previous = parse("intro\n\n![a](u")
        .iter()
        .map(crate::scanner::shape)
        .collect::<Vec<_>>();
    let md = "intro\n\n![a](u)";
    let current = parse(&replace_images(md, &scan_images(md), TokenKind::Nul))
        .iter()
        .map(crate::scanner::shape)
        .collect::<Vec<_>>();
    assert!(
        closed_prefix_stable(&previous, &current),
        "闭合末段图片不能改变前面完整段落"
    );
}

#[test]
fn test_collision_resolution_requires_unique_tokens_and_preserved_user_text() {
    let source = "用户 \0IMG0\0 然后 ![a](u) ![b](v)";
    let hits = scan_images(source);
    let (fixed, tokens) = replace_collision_free(source, &hits, TokenKind::Nul);
    assert_eq!(tokens.len(), 2);
    assert_ne!(tokens[0], tokens[1], "两张图片必须映射到不同 token");
    assert!(collision_resolved(
        source,
        &fixed,
        &tokens,
        hits.len(),
        "\0IMG0\0"
    ));
    let corrupted = fixed.replace("\0IMG0\0", "");
    let check = resolved_collision_check(
        "lost user text",
        source,
        &corrupted,
        &tokens,
        hits.len(),
        "\0IMG0\0",
    );
    assert!(!check.passed, "重编号不能吞掉用户原有 token");
    let mut experiment = run();
    experiment.collisions = vec![check];
    assert_eq!(experiment.exit_code(), 1, "碰撞解决失败必须影响退出码");
    assert!(
        !collision_resolved(
            source,
            &fixed,
            &[tokens[0].clone(), tokens[0].clone()],
            hits.len(),
            "\0IMG0\0"
        ),
        "两个图片共用 token 不算解决碰撞"
    );
}

#[test]
fn test_missing_experiment_group_cannot_report_success() {
    let mut experiment = run();
    assert!(
        experiment.references.iter().all(|c| c.passed),
        "定义/未定义 reference 均应符合预期"
    );
    experiment.references.clear();
    assert_eq!(
        experiment.exit_code(),
        1,
        "漏执行整组不能被空集合的 all 判作通过"
    );
}
