use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::skills::{
    find_skill_in_list, scan_skill_roots, SkillRoot, SkillSource, SkillsMiddleware,
};

use super::{parse_builtin_frontmatter, BUILTIN_SKILLS};

#[test]
fn test_builtin_skills_non_empty() {
    // 至少含 use-artifacts 验证用例
    assert!(BUILTIN_SKILLS.iter().any(|s| s.name == "use-artifacts"),
        "BUILTIN_SKILLS 应含 use-artifacts");
}

#[test]
fn test_builtin_skills_unique_names() {
    let mut names: Vec<&str> = BUILTIN_SKILLS.iter().map(|s| s.name).collect();
    names.sort();
    let original_len = names.len();
    names.dedup();
    assert_eq!(names.len(), original_len, "BUILTIN_SKILLS 名称不应重复");
}

#[test]
fn test_builtin_skills_frontmatter_valid() {
    // 每个 BUILTIN_SKILLS 的 frontmatter 都应能解析出 name + description
    for skill in BUILTIN_SKILLS {
        let parsed = parse_builtin_frontmatter(skill.content);
        assert!(parsed.is_some(),
            "builtin skill {} frontmatter 解析失败", skill.name);
        let (name, aliases, desc) = parsed.unwrap();
        assert_eq!(name, skill.name,
            "builtin skill {} frontmatter name 字段不匹配", skill.name);
        if skill.name == "programmatic-tool-calling" {
            assert_eq!(aliases, vec!["ptc"]);
        }
        assert!(!desc.is_empty(),
            "builtin skill {} description 为空", skill.name);
    }
}

#[test]
fn test_ultra_adlc_skill_registered_and_discriminating() {
    let skill = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc");
    let (name, aliases, description) =
        parse_builtin_frontmatter(skill.content).expect("ultra-adlc frontmatter 应有效");

    assert_eq!(name, "ultra-adlc");
    assert!(aliases.is_empty());
    let description = description.to_ascii_lowercase();
    assert!(description.contains("very large"));
    assert!(description.contains("end-to-end"));
    assert!(description.contains("do not use for ordinary"));
    assert!(skill.content.contains("userInvocable: true"));
    assert!(skill.content.contains("argumentHint:"));
}

#[test]
fn test_ultra_adlc_skill_is_discoverable_in_builtin_summary() {
    let skills = scan_skill_roots(&[SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Builtin,
        plugin_name: None,
    }]);
    let summary = SkillsMiddleware::build_summary(&skills);

    assert!(
        summary.contains("**ultra-adlc** [builtin]"),
        "builtin 摘要应暴露 ultra-adlc，实际: {summary}"
    );
}

#[test]
fn test_ultra_adlc_skill_encodes_peri_workflow_contract() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content;

    for marker in [
        "./.peri/adlc/",
        "intent.md",
        "execution.md",
        "evidence.md",
        "exactly two **logical** workflows",
        "discovery-design",
        "delivery-convergence",
        "AskUserQuestion",
        "SearchExtraTools(\"workflow\")",
        "ExecuteExtraTool(\"Workflow\"",
        ".claude/workflow-runs/<run-id>/state.json",
        "() => agent(",
        "Date.now()",
        "new Date()",
        "Math.random()",
        "phase(name) only marks a stage",
        "delivery_status",
        "acceptance_status: unknown",
        "delivery is `unknown`, not blocked",
        "pre-existing unrelated",
        "The Git postcondition compares before/after porcelain records",
        "infer the culprit from the final dirty set",
        "path_allowlist",
        "git status --porcelain",
        "at most 4",
    ] {
        assert!(content.contains(marker), "ultra-adlc 应锁定 {marker}");
    }

    assert!(
        !content.contains("./peri/adlc/"),
        "ultra-adlc 不得再锁定已删除路径 ./peri/adlc/"
    );
    assert!(
        !content.contains("at most three"),
        "ultra-adlc 提问上限应与 AskUserQuestion 对齐为 at most 4"
    );

    for profile in ["`fable`", "`opus`", "`sonnet`", "`haiku`"] {
        assert!(
            content.contains(profile),
            "ultra-adlc 应覆盖 profile {profile}"
        );
    }
}

#[test]
fn test_ultra_adlc_skill_defaults_to_independent_arbitration_and_semantic_progress() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content;

    for marker in [
        "Decision Arbiter",
        "fresh, independent `opus`",
        "ADLC/W1/Arbitrate",
        "peri.adlc/decision-packet-v1",
        "peri.adlc/arbitration-handoff-v1",
        "peri.adlc/decision-record-v1",
        "decision_source: arbiter | user",
        "`arbitration_result` is exactly one of `decided |",
        "enum: ['decided', 'needs_evidence', 'needs_user', 'invalid']",
        "attempt_number: <1-or-2-for-opus-or-3-for-fable>",
        "correction_of_attempt_id: <prior-attempt-id-or-none>",
        "expectedArbiterProfile = args.arbitrationAttemptNumber <= 2 ? 'opus' : 'fable'",
        "priorAttemptsAreFreshFailedOpus",
        "attempt.packet_fingerprint === args.packetFingerprint",
        "attempt.recoveredFrom: null",
        "manifest.decision.attempts",
        "existing regular, non-symlinked candidate",
        "workflowMode === 'prepare_packet'",
        "status: 'packet_ready'",
        "workflowMode !== 'arbitrate'",
        "packetCandidatePathAlreadyExists === true",
        "expectedCandidatePath =",
        "candidate-${args.prepareAttemptId}.md",
        "exclusive-create/no-overwrite",
        "O_CREAT | O_EXCL",
        "decisionPacketContent",
        "const packetEnvelope = JSON.stringify({",
        "const boundArbitratePrompt =",
        "do not reopen packet_path",
        "packetVerifiedByMainAgent === true",
        "handoffPathAlreadyExists === false",
        "optionMatchesPacket(",
        "allowedEvidenceReferences.has(reference)",
        "args.expectedArbitrationHandoffPath === expectedHandoffPath",
        "args.packetPath === expectedPacketPath",
        "^sha256:[0-9a-f]{64}$",
        "packet_path: arbitration.packet_path",
        "packet_fingerprint: arbitration.packet_fingerprint",
        "Arbitration retries intentionally skip ADLC/W1/Discover, Design, and Synthesize.",
        "schema: arbitrationResultSchema",
        "if (!metadataMatches || !resultFieldsAreLegal)",
        "status: arbitration.arbitration_result",
        "two fresh `opus` attempts fail",
        "`fable` is escalation, never the default",
        "Do not turn model uncertainty into a user preference question",
        "peri.adlc/progress-snapshot-v1",
        "denominator_revision",
        "denominator_fingerprint: <sha256-of-canonical-dimension-id-semantic-content-completion-condition-required-flag-and-linked-revisions>",
        "deterministic canonical",
        "overall_progress_percent = min(",
        "gap_closure_percent",
        "accepted Verification Plan checks",
        "empty set is defined as 100%",
        "For example, 75% requirements, 80% Work Packages, 60% acceptance",
        "90% gap closure produces `overall_progress_percent: 60%`",
        "Do not average dimensions",
        "ID | Semantic content",
        "bare ids such as",
    ] {
        assert!(content.contains(marker), "ultra-adlc 应锁定 {marker}");
    }

    assert!(
        !content.contains("status: 'complete',\n    workPackage: 'W1-SYNTH'"),
        "Workflow 1 不得把任意裁决结果无条件压成 complete"
    );
    assert_eq!(
        content
            .matches("\n    phase('ADLC/W1/Discover')")
            .count(),
        1,
        "canonical prepare_packet 分支应恰好执行一次 Discover，arbitrate 分支不得重跑"
    );
    assert_eq!(
        content
            .matches("\n    phase('ADLC/W1/Synthesize')")
            .count(),
        1,
        "canonical prepare_packet 分支应恰好执行一次 Synthesize，arbitrate 分支不得重跑"
    );
    assert!(
        !content.contains("packet_fingerprint: sha256:<lowercase-hex>\nintent_revision:"),
        "packet fingerprint 是 Main Agent 计算的外部元数据，不得写入被 hash 的 packet"
    );
    assert!(
        !content.contains("await agent(arbitratePrompt, {"),
        "arbiter 必须消费 Main Agent 绑定的 packet bytes，不得只消费可变路径或未绑定 prompt"
    );
    assert!(
        !content.contains("handoffs/workflow-1/arbitration-D-001-r1.md"),
        "裁决 Handoff 路径必须按 attempt 唯一，不能让重试覆盖前序审计记录"
    );
    assert!(
        !content.contains("If it is absent, stop as `blocked`; Workflow Agents cannot replace it."),
        "AskUserQuestion 缺失不得阻塞默认自动裁决路径"
    );
    assert!(
        !content.contains("Call `AskUserQuestion` with at most 4 questions per round."),
        "Ultra-ADLC 不得恢复默认用户裁决"
    );
}

#[test]
fn test_ultra_adlc_canonical_w1_separates_packet_preparation_from_arbitration() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content
        .replace("\r\n", "\n");
    let script = content
        .split_once("Canonical W1 script shape:\n\n```javascript\n")
        .and_then(|(_, rest)| rest.split_once("\n```"))
        .map(|(script, _)| script)
        .expect("Ultra-ADLC 应包含 canonical W1 JavaScript");
    let (_, arbitrate_branch) = script
        .split_once("if (args.workflowMode !== 'arbitrate')")
        .expect("canonical W1 应显式拒绝未知 mode");

    assert!(script.contains("if (args.workflowMode === 'prepare_packet')"));
    assert!(script.contains("status: 'packet_ready'"));
    assert!(!arbitrate_branch.contains("phase('ADLC/W1/Discover')"));
    assert!(!arbitrate_branch.contains("phase('ADLC/W1/Design')"));
    assert!(!arbitrate_branch.contains("phase('ADLC/W1/Synthesize')"));
    assert!(arbitrate_branch.contains("phase('ADLC/W1/Arbitrate')"));
    assert!(arbitrate_branch.contains("agent(boundArbitratePrompt"));
    assert!(arbitrate_branch.contains("optionMatchesPacket("));
    assert!(arbitrate_branch.contains("allowedEvidenceReferences.has(reference)"));
    assert!(arbitrate_branch.contains("status: arbitration.arbitration_result"));
}

#[test]
fn test_ultra_adlc_skill_guards_complete_delivery_and_audit() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content;

    for marker in [
        "There is no",
        "`partially_complete`",
        "Completion Assessor",
        "exactly one new",
        "100% coverage",
        "gap-round-N.md",
        "learning/agent-performance.md",
        "Only when the assessor verdict is `complete`",
        "must not receive a success performance record",
        "Never commit, push, publish, deploy",
        "Never put a secret, token, password",
    ] {
        assert!(content.contains(marker), "ultra-adlc 应锁定 {marker}");
    }
}

#[test]
fn test_ultra_adlc_skill_does_not_require_repository_snapshot() {
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "ultra-adlc")
        .expect("BUILTIN_SKILLS 应含 ultra-adlc")
        .content;

    for marker in [
        "peri workflow boundary",
        "filesystem write-boundary snapshot",
        "Bounded filesystem evidence",
        "expectedBaselineFingerprint",
        "filesystem boundary check fails",
    ] {
        assert!(!content.contains(marker), "不应恢复仓库快照门禁：{marker}");
    }
    assert!(content.contains("git status --porcelain"));
    assert!(content.contains("writeIntent.path_allowlist"));
    assert!(content.contains("Review task-scoped"));
}

#[test]
fn test_ultra_task_skill_not_registered_or_discoverable() {
    assert!(
        !BUILTIN_SKILLS.iter().any(|skill| skill.name == "ultra-task"),
        "BUILTIN_SKILLS 不应含 ultra-task"
    );

    let skills = scan_skill_roots(&[SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Builtin,
        plugin_name: None,
    }]);
    assert!(
        !skills.iter().any(|skill| skill.name == "ultra-task"),
        "builtin 扫描结果不应含 ultra-task"
    );
    let summary = SkillsMiddleware::build_summary(&skills);
    assert!(
        !summary.contains("ultra-task"),
        "builtin 摘要不应暴露 ultra-task，实际: {summary}"
    );
}

#[test]
fn test_multitask_skill_is_registered_as_builtin() {
    let skill = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "multitask")
        .expect("BUILTIN_SKILLS 应含 multitask");
    let (name, aliases, description) =
        parse_builtin_frontmatter(skill.content).expect("multitask frontmatter 应有效");

    assert_eq!(name, "multitask");
    assert!(aliases.is_empty());
    assert!(description.contains("coordinator"));
    assert!(description.contains("exactly one owner"));
}

#[test]
fn test_multitask_skill_discoverable_without_project_copy() {
    // 项目根为空目录（无本地 multitask 副本）时，仍须以 Builtin 来源被发现
    let project = tempdir().unwrap();
    let skills = scan_skill_roots(&[
        SkillRoot {
            path: project.path().to_path_buf(),
            source: SkillSource::Project,
            plugin_name: None,
        },
        SkillRoot {
            path: PathBuf::new(),
            source: SkillSource::Builtin,
            plugin_name: None,
        },
    ]);

    let multitask = skills
        .iter()
        .find(|skill| skill.name == "multitask")
        .expect("项目无本地副本时 multitask 仍应可发现");
    assert_eq!(multitask.source, SkillSource::Builtin);
    assert_eq!(multitask.path, PathBuf::from("<builtin>/multitask"));
    assert!(
        SkillsMiddleware::build_summary(&skills).contains("- **multitask** [builtin]"),
        "摘要应以 [builtin] 来源标签列出 multitask"
    );

    // SkillTool / 预加载走 find_skill_in_list → content::load 的 Builtin 分支
    let (loaded, content) = find_skill_in_list(&skills, "multitask").expect("应能按名加载正文");
    assert_eq!(loaded.source, SkillSource::Builtin);
    assert!(content.contains("# Multitask Mode"));
    assert!(content.contains("Independent siblings run concurrently"));
}

#[test]
fn test_multitask_skill_not_shadowed_by_project_directory() {
    // 仓库 .claude/skills 不得再保留 multitask 副本：项目来源会遮蔽 builtin 版本
    let project_skills = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.claude/skills");
    assert!(
        !project_skills.join("multitask").exists(),
        ".claude/skills/multitask 应已移除，避免项目级副本遮蔽 builtin"
    );

    let skills = scan_skill_roots(&[
        SkillRoot {
            path: project_skills,
            source: SkillSource::Project,
            plugin_name: None,
        },
        SkillRoot {
            path: PathBuf::new(),
            source: SkillSource::Builtin,
            plugin_name: None,
        },
    ]);
    let multitask = skills
        .iter()
        .find(|skill| skill.name == "multitask")
        .expect("multitask 应可发现");
    assert_eq!(
        multitask.source,
        SkillSource::Builtin,
        "multitask 应统一来自 builtin，实际: {:?}",
        multitask.source
    );
}

#[test]
fn test_multitask_skill_has_no_concurrency_limit_wording() {
    // 后台 subagent 并发上限已移除，正文不得再引用上限或据其排队
    let content = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == "multitask")
        .expect("BUILTIN_SKILLS 应含 multitask")
        .content;

    assert!(
        !content.contains("concurrency limit"),
        "multitask 不得再引用并发上限"
    );
    assert!(
        !content.contains("launching around it"),
        "上限消失后不再有需要绕开的排队语义"
    );
    assert!(
        content.contains("Independent siblings run concurrently"),
        "multitask 应显式允许 siblings 同时运行"
    );
}

#[test]
fn test_parse_builtin_frontmatter_invalid_returns_none() {
    // 格式错误的 frontmatter 应返回 None
    let bad = "no frontmatter here";
    assert!(parse_builtin_frontmatter(bad).is_none());

    let bad2 = "---\nname: only_name\n---\nbody";
    assert!(parse_builtin_frontmatter(bad2).is_none(),
        "缺 description 字段应返回 None");
}

#[test]
fn test_parse_builtin_frontmatter_valid() {
    let content = "---\nname: test-skill\ndescription: 测试 skill\n---\n\n# Body\n";
    let parsed = parse_builtin_frontmatter(content).unwrap();
    assert_eq!(parsed.0, "test-skill");
    assert!(parsed.1.is_empty());
    assert_eq!(parsed.2, "测试 skill");
}

#[test]
fn test_parse_builtin_frontmatter_trims_trailing_newline() {
    // YAML `>`（折叠标量）和 `|`（字面标量）会在末尾保留 `\n`，
    // 下游拼到 Markdown list item 末尾会让 list 渲染断裂，需要 trim
    let content = "---\nname: folded\ndescription: >\n  Multi line description.\n---\n\n# Body\n";
    let parsed = parse_builtin_frontmatter(content).unwrap();
    assert_eq!(parsed.0, "folded");
    assert!(
        !parsed.2.ends_with('\n') && !parsed.2.ends_with('\r'),
        "description 不应含尾随换行，实际: {:?}", parsed.2
    );
}
