use super::*;

#[test]
fn meta_harness_override_replaces_section_full_text() {
    let state = override_state("01_intro", "# Custom Intro\n\n完全替换的角色定义。");
    let result = render_with_state(&state);
    assert!(result.contains("# Custom Intro"), "覆盖内容应出现在输出中");
    // 内置 01_intro 不再出现：以持有者段落全文为锚点校验
    let builtin = holder_section_content("01_intro");
    assert!(
        !result.contains(&builtin),
        "内置 01_intro 全文不应再出现在输出中"
    );
}

#[test]
fn meta_harness_override_05_using_tools() {
    let state = override_state("05_using_tools", "### Tools Discipline\n\n自定义工具纪律。");
    let result = render_with_state(&state);
    assert!(result.contains("自定义工具纪律"), "覆盖内容应出现");
    let builtin = holder_section_content("05_using_tools");
    assert!(!result.contains(&builtin), "内置 05_using_tools 不应再出现");
}

#[test]
fn meta_harness_unoverridden_section_unchanged() {
    let state = override_state("01_intro", "custom");
    let result = render_with_state(&state);
    let builtin_02 = holder_section_content("02_system");
    assert!(result.contains(&builtin_02), "未覆盖的 02_system 字节不变");
}

#[test]
fn meta_harness_multiple_overrides_apply_together() {
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert("01_intro".to_string(), Arc::from("intro-ovr"));
    state
        .section_overrides
        .insert("05_using_tools".to_string(), Arc::from("tools-ovr"));
    let result = render_with_state(&state);
    assert!(result.contains("intro-ovr"));
    assert!(result.contains("tools-ovr"));
    // 段落顺序不变：01 在 05 之前
    assert!(
        result.find("intro-ovr").unwrap() < result.find("tools-ovr").unwrap(),
        "段落渲染顺序不变（01_intro 在 05_using_tools 之前）"
    );
}

#[test]
fn meta_harness_override_not_trimmed() {
    // override 内容保留原样（不 trim）：前后空白原样进入输出
    let state = override_state("01_intro", "\n  # Padded  \n\nbody  \n");
    let result = render_with_state(&state);
    assert!(
        result.contains("\n  # Padded  \n\nbody  \n"),
        "覆盖内容不被 trim"
    );
}

#[test]
fn meta_harness_override_placeholders_still_substituted() {
    // override 内容中的占位符参与渲染期替换（与内置段落同一通道）
    let state = override_state("01_intro", "cwd={{cwd}} platform={{platform}}");
    let result = render_with_state(&state);
    assert!(result.contains("cwd=/tmp"), "{{cwd}} 被替换");
    assert!(result.contains("platform="), "{{platform}} 被替换");
}

#[test]
fn meta_harness_gated_enabled_shows_override() {
    // 10_hitl 由 PermissionMiddleware 持有（收集即装配）：覆盖 10_hitl
    // 应生效（覆盖 = 替换持有者对应段落贡献，设计 §2.4 / §3.5.1 步骤 5）。
    let state = override_state("10_hitl", "HITL-OVERRIDE");

    let result = render_with_state(&state);
    assert!(result.contains("HITL-OVERRIDE"), "持有者装配时显示覆盖内容");
}

/// 11_subagent 覆盖语义（设计 §3.5.1 步骤 5 / C3 D2 第 6 步）：覆盖 =
/// 替换 SubAgentMiddleware 持有段落贡献，机制与持有者无关——覆盖全文
/// 出现、内置全文消失、段落渲染位置不变（仍按段内序号在 10_hitl 与
/// 13_skills 之间）。
#[test]
fn meta_harness_override_11_subagent_replaces_holder_section() {
    let state = override_state("11_subagent", "SUBAGENT-OVERRIDE");
    let result = render_with_state(&state);
    assert!(
        result.contains("SUBAGENT-OVERRIDE"),
        "覆盖全文应替换持有者段落"
    );
    // 内置 11_subagent（含占位符的 Builtin 文本）全文不再出现
    let builtin = SubAgentMiddleware::sections()[0]
        .content
        .as_str()
        .to_string();
    assert!(
        !result.contains(&builtin),
        "内置 11_subagent 全文不应再出现在输出中"
    );
    // 段落顺序不变：11_subagent 位置仍在 10_hitl（order=3）与
    // 13_skills（order=5）之间
    let pos_hitl = result
        .find("## Which tools are sensitive")
        .expect("10_hitl 机制说明应在");
    let pos_override = result
        .find("SUBAGENT-OVERRIDE")
        .expect("11_subagent 覆盖段应在");
    let pos_skills = result.find("# Skills").expect("13_skills 机制说明应在");
    assert!(
        pos_hitl < pos_override && pos_override < pos_skills,
        "段落顺序不变（10_hitl < 11_subagent < 13_skills）：{pos_hitl} < {pos_override} < {pos_skills}"
    );
}

/// C3（gate 原子迁移）：10_hitl gate = PermissionMiddleware 是否在链上，
/// 不再依赖 permission_mode——`detect()` 无 gate 差异，覆盖恒渲染；关闭
/// 持有者（disabled_middlewares）才隐藏段落（决策记录 C3 D5；2026-08-15
/// 职责拆分：10_hitl 持有者由新 HumanInTheLoopMiddleware 改为
/// PermissionMiddleware）。
#[test]
fn meta_harness_gated_override_hidden_only_when_holder_disabled() {
    let mut state = override_state("10_hitl", "HITL-OVERRIDE");

    // 默认状态：持有者装配（收集段恒渲染）→ 覆盖显示
    let result = render_with_state(&state);
    assert!(
        result.contains("HITL-OVERRIDE"),
        "持有者装配时覆盖应显示（gate 不再依赖 permission_mode）"
    );
    // 关闭持有者 → 段落整体消失（覆盖随段落一并隐藏）
    state
        .disabled_middlewares
        .insert("PermissionMiddleware".to_string());
    let result = render_with_state(&state);
    assert!(
        !result.contains("HITL-OVERRIDE"),
        "关闭 PermissionMiddleware 后 10_hitl（含覆盖）不渲染"
    );
}

#[test]
fn meta_harness_persona_full_keeps_overridden_immutable_sections() {
    let state = override_state("01_intro", "OVR-INTRO");
    let overrides = AgentOverrides {
        persona: Some("You are the full persona".into()),
        tone: Some("ignored".into()),
        proactiveness: Some("ignored".into()),
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        &state,
        Some(&overrides),
        "/tmp",
        &AgentCatalogProvider::new(),
        Some("2026-01-01"),
        None,
    );
    assert!(
        result.contains("OVR-INTRO"),
        "full persona 不移除覆盖后的 immutable sections"
    );
    assert!(
        result.contains("You are the full persona"),
        "full body 渲染"
    );
}

#[test]
fn meta_harness_build_and_template_byte_identical() {
    // 同源一致性：build_system_prompt 与直接 PromptTemplate render 字节一致
    let state = override_state("01_intro", "OVR-INTRO");
    let overrides = AgentOverrides {
        persona: Some("extend persona".into()),
        tone: None,
        proactiveness: None,
        mode: None,
    };

    let via_build = build_system_prompt(
        &state,
        Some(&overrides),
        "/tmp",
        &AgentCatalogProvider::new(),
        Some("2026-01-01"),
        Some("zh"),
    );
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let collected = crate::session::build_collected_sections(&state, Some(&overrides), Some("zh"));
    let via_template =
        PromptTemplate::new(&state, &collected).render(&env, &AgentCatalogProvider::new());
    assert_eq!(via_build, via_template, "两条渲染路径字节一致");
}

#[test]
fn meta_harness_disabled_set_does_not_affect_sections() {
    // 两动作独立：disabled_middlewares 不影响段落渲染
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert("01_intro".to_string(), Arc::from("OVR-INTRO"));
    state
        .disabled_middlewares
        .insert("WebMiddleware".to_string());
    let result = render_with_state(&state);
    assert!(result.contains("OVR-INTRO"), "disabled 集合不影响覆盖渲染");
}

#[test]
fn section_ids_match_holders() {
    use peri_acp_types::meta_harness::SECTION_IDS;

    let mut actual: Vec<&str> = DefaultSystemPromptMiddleware::sections(None)
        .iter()
        .map(|section| section.id)
        .chain(LangMiddleware::sections(Some("zh")).iter().map(|s| s.id))
        .chain(
            peri_middlewares::permission::PermissionMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .chain(
            peri_middlewares::hitl::HumanInTheLoopMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .chain(
            peri_middlewares::subagent::SubAgentMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .chain(
            peri_middlewares::skills::SkillsMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .collect();
    actual.sort_unstable();
    let mut expected: Vec<&str> = SECTION_IDS.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected, "SECTION_IDS 与持有者声明 ID 完全一致");
    // 无重复
    let mut seen = std::collections::HashSet::new();
    for id in actual {
        assert!(seen.insert(id), "duplicate section id: {id}");
    }
}

#[test]
fn retired_channel_override_does_not_create_a_section() {
    let state = override_state("15_channel", "RETIRED-CHANNEL-OVERRIDE");
    let result = render_with_state(&state);
    assert!(!result.contains("RETIRED-CHANNEL-OVERRIDE"));
    assert!(!result.contains("Channel 频道消息"));
}

#[test]
fn meta_harness_override_13_skills_gated() {
    // C3：13_skills 由 SkillsMiddleware 持有（收集即装配）：覆盖 13_skills
    // 应生效（持有者装配即渲染）
    let state = override_state("13_skills", "SKILLS-OVERRIDE");

    let result = render_with_state(&state);
    assert!(result.contains("SKILLS-OVERRIDE"));
}

/// P2-2（实施质量审查）：persona 段恒声明 + 空内容——无 overrides 时收集的
/// persona 段内容为空串（渲染面空内容过滤跳过，默认不渲染），但 MetaHarness
/// 覆盖 `.peri/meta/persona.md` 仍可注入（覆盖合并先于空内容过滤）。
#[test]
fn meta_harness_override_persona_without_overrides() {
    // 1. 无 overrides：persona 段恒声明且内容为空（空内容默认不渲染的前提）
    let collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    let persona = collected
        .iter()
        .find(|s| s.id == "persona")
        .expect("persona 段应恒声明（D2）");
    assert!(
        persona.content.as_str().is_empty(),
        "无 overrides 时 persona 内容应为空串"
    );
    // 2. 无用户配置时覆盖仍可注入：覆盖全文渲染（唯一标记）
    let state = override_state("persona", "PERSONA-OVERRIDE-NO-CONFIG");
    let result = render_with_state(&state);
    assert!(
        result.contains("PERSONA-OVERRIDE-NO-CONFIG"),
        "无 overrides 时 persona 覆盖仍应注入"
    );
    // 3. 默认（无覆盖）不渲染空 persona：覆盖标记不出现，段落位置无空残留
    let default_result = render_with_state(&MetaHarnessState::default());
    assert!(
        !default_result.contains("PERSONA-OVERRIDE-NO-CONFIG"),
        "无覆盖时默认输出不含 persona 覆盖标记"
    );
}

/// P2-2（实施质量审查）：language 段恒声明 + 空内容——无 `settings.language`
/// 时收集的 language 段内容为空串（默认不渲染），但 MetaHarness 覆盖
/// `.peri/meta/language.md` 仍可注入。
#[test]
fn meta_harness_override_language_without_config() {
    // 1. 无语言配置：language 段恒声明且内容为空（空内容默认不渲染的前提）
    let collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    let language = collected
        .iter()
        .find(|s| s.id == "language")
        .expect("language 段应恒声明（D2）");
    assert!(
        language.content.as_str().is_empty(),
        "无语言配置时 language 内容应为空串"
    );
    // 2. 无语言配置时覆盖仍可注入：覆盖全文渲染（唯一标记）
    let state = override_state("language", "LANGUAGE-OVERRIDE-NO-CONFIG");
    let result = render_with_state(&state);
    assert!(
        result.contains("LANGUAGE-OVERRIDE-NO-CONFIG"),
        "无语言配置时 language 覆盖仍应注入"
    );
    // 3. 默认（无覆盖）不渲染空 language 段：覆盖标记不出现
    let default_result = render_with_state(&MetaHarnessState::default());
    assert!(
        !default_result.contains("LANGUAGE-OVERRIDE-NO-CONFIG"),
        "无覆盖时默认输出不含 language 覆盖标记"
    );
}

// ─── 波 4 段落持有者基础设施（C1）：装配期收集结果合并 ──────────────────

/// 契约 2：收集段落按"位置 + 段内序号"排序渲染，**不依赖链序**。
///
/// collected 以乱序传入（zz order=9 在前、aa order=8 在后），渲染必须按
/// 段内序号升序输出；非缓存区段落在 07_runtime 之后、Language 段之前
/// （language order=7 < aa order=8 < zz order=9）。
#[test]
fn collected_sections_render_in_position_order() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, Some("zh-CN"));
    // 乱序追加收集段（收集不承诺顺序，排序由渲染面执行）
    collected.push(collected_section(
        "zz_collected",
        PromptSectionZone::Uncached,
        9,
        "ZZ-COLLECTED-LATE",
    ));
    collected.push(collected_section(
        "aa_collected",
        PromptSectionZone::Uncached,
        8,
        "AA-COLLECTED-EARLY",
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    let pos_runtime = result.find("## System Reminders").unwrap();
    let pos_lang = result.find("# Language").unwrap();
    let pos_aa = result.find("AA-COLLECTED-EARLY").unwrap();
    let pos_zz = result.find("ZZ-COLLECTED-LATE").unwrap();
    assert!(
        pos_runtime < pos_lang && pos_lang < pos_aa && pos_aa < pos_zz,
        "收集段落按段内序号升序渲染（不依赖收集顺序）：{pos_runtime} < {pos_lang} < {pos_aa} < {pos_zz}"
    );
    // 基础段由收集注入（C2：数组已删除，收集结果成为唯一来源）
    assert!(
        result.contains("Following conventions"),
        "02_system 经收集结果渲染"
    );
    assert!(
        result.contains("## System Reminders"),
        "07_runtime 经收集渲染"
    );
}

/// 契约 2 + 收集合并：collected 按 ID 覆盖内置段落，位置属性以持有者声明为准。
#[test]
fn collected_section_overrides_builtin_by_id() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    // 01_intro 由收集段按 ID 覆盖（位置属性 Cached + order 1 与持有者一致）
    collected.retain(|s| s.id != "01_intro");
    collected.push(collected_section(
        "01_intro",
        PromptSectionZone::Cached,
        1,
        "COLLECTED-INTRO",
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    assert!(result.contains("COLLECTED-INTRO"), "收集段落内容渲染");
    assert!(
        !result.contains("Assist with defensive security tasks"),
        "内置 01_intro 被收集段按 ID 替换"
    );
    // 位置：缓存区首位（02_system 之前）——持有者声明的 Cached+1
    let pos_intro = result.find("COLLECTED-INTRO").unwrap();
    let pos_system = result.find("Following conventions").unwrap();
    assert!(
        pos_intro < pos_system,
        "收集段位置属性（Cached order=1）生效"
    );
}

/// 契约 4：middleware 提供空内容段落 = 跳过渲染不 fail，其余段落不受影响。
#[test]
fn collected_empty_content_skipped() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_empty",
        PromptSectionZone::Uncached,
        8,
        "",
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    assert!(!result.contains("zz_empty"), "空内容段落不渲染");
    assert!(result.contains("Following conventions"), "其他段落不受影响");
}

/// 动态内容段落（`PromptSectionContent::Dynamic`）正常渲染。
#[test]
fn collected_dynamic_content_rendered() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(PromptSection::dynamic(
        "zz_dyn",
        PromptSectionZone::Uncached,
        8,
        "DYNAMIC-COLLECTED".to_string(),
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    assert!(result.contains("DYNAMIC-COLLECTED"), "动态内容段落渲染");
}

/// 覆盖语义（设计 §2.4/3.5.1 步骤 5）：MetaHarness 覆盖 = 替换持有者对应段落
/// 贡献——`state.section_overrides` 优先于 collected 内容。
#[test]
fn collected_content_merged_with_meta_harness_override() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.retain(|s| s.id != "05_using_tools");
    collected.push(collected_section(
        "05_using_tools",
        PromptSectionZone::Cached,
        5,
        "COLLECTED-TOOLS",
    ));
    let state = override_state("05_using_tools", "OVERRIDE-TOOLS");

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&state, &collected).render(&env, &AgentCatalogProvider::new());
    assert!(result.contains("OVERRIDE-TOOLS"), "覆盖全文替换持有者段落");
    assert!(
        !result.contains("COLLECTED-TOOLS"),
        "覆盖优先于 collected 内容"
    );
}

/// 持有者已装配的收集段参与渲染。
#[test]
fn collected_sections_are_rendered() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_collected",
        PromptSectionZone::Uncached,
        8,
        "GATE-FREE-COLLECTED",
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    assert!(result.contains("GATE-FREE-COLLECTED"), "收集段恒渲染");
}

// ─── C 扩展 / E 补测试（2026-08-14 advisor 矩阵缺口）───────────────────────

/// 覆盖语义边界（empty 定义，C 项裁定）：空串覆盖 → `is_empty()` 过滤 →
/// 段落整体消失（与契约 4"未提供内容 = 跳过渲染"同一路径）。
#[test]
fn meta_harness_override_empty_removes_section() {
    let state = override_state("01_intro", "");
    let result = render_with_state(&state);
    assert!(
        !result.contains("Assist with defensive security tasks"),
        "空串覆盖 → 01_intro 经 is_empty 过滤从输出消失"
    );
    assert!(result.contains("Following conventions"), "其余段落不受影响");
}

/// 覆盖语义边界（empty 定义锁定）：空白串覆盖 → `is_empty()` 为 false →
/// 原样渲染，不 trim 也不消失（与 `meta_harness_override_not_trimmed`
/// 既定不 trim 语义一致；空白段落保留原位）。
#[test]
fn meta_harness_override_whitespace_renders_as_is() {
    let state = override_state("01_intro", "   ");
    let result = render_with_state(&state);
    // 01_intro 为缓存区首段，渲染结果以其内容开头（无前缀分隔符）
    assert!(
        result.starts_with("   "),
        "空白覆盖原样渲染（不 trim）：{:?}",
        &result[..result.len().min(24)]
    );
    assert!(
        result.contains("Following conventions"),
        "空白覆盖不触发空过滤，段落保留"
    );
}

/// 收集契约（C 项矩阵）：collected 中重复 ID → 后者覆盖前者（位置属性随
/// 后者声明），渲染恰好一次。
#[test]
fn collected_duplicate_id_last_wins() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_dup",
        PromptSectionZone::Uncached,
        8,
        "DUP-FIRST",
    ));
    collected.push(collected_section(
        "zz_dup",
        PromptSectionZone::Uncached,
        9,
        "DUP-SECOND",
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    assert!(
        result.contains("DUP-SECOND") && !result.contains("DUP-FIRST"),
        "重复 ID 后者覆盖前者"
    );
    assert_eq!(
        result.matches("DUP-SECOND").count(),
        1,
        "重复 ID 段渲染恰好一次"
    );
}

/// 收集契约（C 项矩阵）：同 (zone, order) 的收集段 → stable 排序保持
/// 收集声明顺序（`sort_by_key` 稳定，不依赖链序的兜底语义）。
#[test]
fn collected_same_zone_order_stable() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_s1",
        PromptSectionZone::Uncached,
        8,
        "STABLE-FIRST",
    ));
    collected.push(collected_section(
        "zz_s2",
        PromptSectionZone::Uncached,
        8,
        "STABLE-SECOND",
    ));

    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected)
        .render(&env, &AgentCatalogProvider::new());
    let pos_first = result.find("STABLE-FIRST").unwrap();
    let pos_second = result.find("STABLE-SECOND").unwrap();
    assert!(
        pos_first < pos_second,
        "同 (zone, order) 稳定排序保持声明顺序：{pos_first} < {pos_second}"
    );
}
