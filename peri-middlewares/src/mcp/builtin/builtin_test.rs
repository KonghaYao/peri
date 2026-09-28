//! `mcp/builtin/mod.rs` 的行为测试（纯函数层）。
//!
//! 过滤器：`cargo test -p peri-middlewares --lib -- mcp::builtin::tests`
//! （**禁止**裸 `mcp::builtin`——会命中 `builtin_spike_tests`）。
//!
//! 本文件锁定：冻结 effective 字面量（与 `effective_tool_name()` 输出逐字相等，含 wave 3 的
//! 7 个 `mcp__workspace__*`）、`direct` 集合与 `system_mcp_tools` 集合一致、
//! `apply_builtin_overlay` 的六条覆盖规则（含 A3 保留名 typed error 与 A18 非法关闭片段反例）、
//! 关闭集、`is_declared_direct` 的两半语义（已实现实例恒真 / 未知组合恒假）、注入策略三态。

use std::collections::{BTreeSet, HashMap, HashSet};

use peri_acp_types::builtin_mcp::{BUILTIN_MCP_INSTANCES, BUILTIN_RESERVED_INSTANCE_NAMES};
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};

use super::*;

/// 仅含缺省字段的 typed 配置（`McpServerConfig` 没有 `Default`）。
fn empty_config() -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    }
}

/// 普通（非 builtin）stdio server：overlay 不得触碰。
fn stdio_config() -> McpServerConfig {
    McpServerConfig {
        command: Some("npx".to_string()),
        args: Some(vec!["-y".to_string(), "some-mcp".to_string()]),
        env: Some(HashMap::from([("KEY".to_string(), "val".to_string())])),
        ..empty_config()
    }
}

fn servers(entries: &[(&str, McpServerConfig)]) -> HashMap<String, McpServerConfig> {
    entries
        .iter()
        .map(|(name, config)| ((*name).to_string(), config.clone()))
        .collect()
}

/// 冻结的模型面名字表（IF-D5）：`(实例, 原始工具名, effective name 字面量)`。
const FROZEN_NAMES: &[(&str, &str, &str)] = &[
    ("web", "WebSearch", "WebSearch"),
    ("web", "WebFetch", "WebFetch"),
    ("artifact", "artifact", "artifact"),
];

fn builtin_source(instance: &str) -> Option<ConfigSource> {
    Some(ConfigSource::Builtin {
        instance: instance.to_string(),
    })
}

/// 「非法输入不得产生部分注入」：逐字段对比 overlay 前后的每个条目
/// （`McpServerConfig` 无 `PartialEq`，且 `source` 不参与 JSON，故显式逐字段断言）。
fn assert_no_mutation(
    map: &HashMap<String, McpServerConfig>,
    before: &HashMap<String, McpServerConfig>,
) {
    assert_eq!(map.len(), before.len(), "键集合不得变化");
    for (name, before_cfg) in before {
        let after = map.get(name).expect("原条目不得消失");
        assert_eq!(after.source, before_cfg.source, "{name} 的 source 被改写");
        assert_eq!(
            after.disabled, before_cfg.disabled,
            "{name} 的 disabled 被改写"
        );
        assert_eq!(
            after.system_mcp, before_cfg.system_mcp,
            "{name} 的 system_mcp 被改写"
        );
        assert_eq!(
            after.system_mcp_tools, before_cfg.system_mcp_tools,
            "{name} 的 system_mcp_tools 被改写"
        );
        assert_eq!(
            after.command, before_cfg.command,
            "{name} 的 command 被改写"
        );
        assert_eq!(after.url, before_cfg.url, "{name} 的 url 被改写");
    }
}

// ─── 冻结字面量 ─────────────────────────────────────────────────────────────

/// 冻结表逐字断言：sanitize 规则或 `mcp__{server}__{tool}` 模板漂移即红。
#[test]
fn frozen_effective_literals_match_effective_tool_name() {
    for (instance, original, effective) in FROZEN_NAMES {
        assert_eq!(
            effective_tool_name(instance, original).as_deref(),
            Some(*effective),
            "{instance}/{original} 的 effective name 漂移"
        );
    }
    // 注册表字面量 == 函数输出（逐工具，不遗漏）。
    for instance in BUILTIN_MCP_INSTANCES {
        for tool in instance.tools {
            assert_eq!(
                tool.effective_name,
                effective_tool_name(instance.name, tool.original_name).expect("已实现实例可计算"),
                "注册表字面量与 effective_tool_name() 输出必须逐字相等"
            );
        }
    }
}

#[test]
fn effective_tool_name_rejects_unknown_instance_or_tool() {
    assert!(
        effective_tool_name("cron", "CronCreate").is_none(),
        "未声明的工具名（cron 零工具）"
    );
    assert!(effective_tool_name("some-user-server", "WebSearch").is_none());
    assert!(effective_tool_name("not-a-builtin", "Read").is_none());
    assert!(effective_tool_name("web", "Unknown").is_none());
    assert!(
        effective_tool_name("web", "websearch").is_none(),
        "原始名精确匹配"
    );
    assert!(effective_tool_name("", "").is_none());
}

#[test]
fn effective_tool_names_covers_registry() {
    let web: BTreeSet<String> = effective_tool_names("web");
    assert_eq!(
        web,
        BTreeSet::from(["WebSearch".to_string(), "WebFetch".to_string(),])
    );
    assert_eq!(
        effective_tool_names("artifact"),
        BTreeSet::from(["artifact".to_string()])
    );
    // wave 3 的 workspace：7 项全部迁为 effective name（AW3-03 的 7 项成员集合）。
    assert_eq!(
        effective_tool_names("workspace"),
        BTreeSet::from([
            "Read".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
            "Glob".to_string(),
            "Grep".to_string(),
            "folder_operations".to_string(),
            "Bash".to_string(),
        ]),
        "workspace 的 7 个有效名（裸名不得出现在此集合里）"
    );
    // 未知名 / 表外 server：空集（调用方据此不产生任何桥）。
    assert!(effective_tool_names("some-user-server").is_empty());
    assert!(effective_tool_names("not-a-builtin").is_empty());
}

// ─── 直连性声明（IF-D13 / A5）──────────────────────────────────────────────

#[test]
fn declared_direct_tools_is_derived_from_registry() {
    for instance in BUILTIN_MCP_INSTANCES {
        let declared = declared_direct_tools(instance.name).expect("已实现实例有声明集合");
        let from_registry: Vec<&str> = instance
            .tools
            .iter()
            .filter(|tool| tool.direct)
            .map(|tool| tool.original_name)
            .collect();
        assert_eq!(
            declared, from_registry,
            "{} 的声明 direct 必须由注册表逐工具 direct 派生",
            instance.name
        );
        match instance.name {
            "web" | "artifact" => {
                assert!(!declared.is_empty(), "wave 1 的实例都必须有 direct 工具")
            }
            // wave 3：workspace 的 7 项全部 direct（AW3-03：迁移前即在首个请求的直连表内）。
            "workspace" => assert_eq!(
                declared.len(),
                7,
                "workspace 的 7 个本地工具必须全部 declared direct（AW3-03）"
            ),
            // wave 2：cron / lsp 零 direct —— `system_mcp_tools` 恒为空集（A5）。
            "cron" | "lsp" => assert!(
                declared.is_empty(),
                "实例 {} 的声明 direct 必须为空（A4：deferred）",
                instance.name
            ),
            other => panic!("新增实例 {other} 必须在本测试内显式登记 direct 语义"),
        }
        for tool in declared {
            assert!(
                is_declared_direct(instance.name, tool),
                "{}/{tool} 必须判定为 declared direct",
                instance.name
            );
        }
    }
    assert!(
        declared_direct_tools("not-a-builtin").is_none(),
        "表外名字无声明"
    );
    assert!(declared_direct_tools("some-user-server").is_none());
}

/// 直连性判定的两半：已实现实例的声明内工具恒 `true`，未知/未声明组合恒 `false`。
///
/// （原用例名 `is_declared_direct_false_for_unimplemented_and_unknown` 的前提是「存在保留
/// 但未实现的实例（workspace）」；W3-B 后五个保留名全部已实现，该前提消失，故按新语义改名并
/// 补上「已实现的 workspace 7 项恒 true」这一半。）
#[test]
fn is_declared_direct_true_for_implemented_false_for_unknown() {
    // 已实现：注册表声明的 7 项恒 true（裸名不是 effective name，仍不得命中别处）。
    for tool in [
        "Read",
        "Write",
        "Edit",
        "Glob",
        "Grep",
        "folder_operations",
        "Bash",
    ] {
        assert!(
            is_declared_direct("workspace", tool),
            "workspace/{tool} 必须判定为 declared direct"
        );
    }
    // 未知名 / 表外 server / 未声明工具：恒 false。
    assert!(!is_declared_direct("workspace", "Nope"));
    assert!(!is_declared_direct("cron", "CronCreate"));
    assert!(!is_declared_direct("web", "Unknown"));
    assert!(!is_declared_direct("some-user-server", "WebSearch"));
    assert!(!is_declared_direct("not-a-builtin", "Read"));
}

#[test]
fn builtin_prompt_declaration_matches_registry() {
    for instance in BUILTIN_MCP_INSTANCES {
        for tool in instance.tools {
            let template = builtin_prompt_declaration(instance.name, tool.original_name);
            match instance.name {
                "web" | "artifact" | "workspace" => {
                    let template = template.expect("这些工具都必须有声明模板（A9 / Q3）");
                    assert_eq!(template, tool.prompt_declaration.unwrap());
                    assert!(
                        template.contains("{{name}}"),
                        "模板必须含占位符: {template}"
                    );
                }
                // wave 2：cron / lsp 冻结 `None`（A4：声明段零变化）。
                "cron" | "lsp" => {
                    assert_eq!(
                        template, None,
                        "实例 {} 的工具 {} 必须无声明模板（A4）",
                        instance.name, tool.original_name
                    );
                    assert_eq!(
                        template, tool.prompt_declaration,
                        "查表结果必须与注册表一致"
                    );
                }
                other => panic!("新增实例 {other} 必须在本测试内显式登记声明模板语义"),
            }
        }
    }
    assert!(builtin_prompt_declaration("web", "Unknown").is_none());
    assert!(builtin_prompt_declaration("not-a-builtin", "Read").is_none());
}

// ─── 关闭集（IF-D10）───────────────────────────────────────────────────────

#[test]
fn closed_instances_maps_policy_keys_only() {
    let none: HashSet<String> = HashSet::new();
    assert!(closed_instances(&none).is_empty(), "空关闭集 ⇒ 无实例关闭");

    let web: HashSet<String> = HashSet::from(["WebMiddleware".to_string()]);
    assert_eq!(closed_instances(&web), BTreeSet::from(["web".to_string()]));

    let artifact: HashSet<String> = HashSet::from(["ArtifactMiddleware".to_string()]);
    assert_eq!(
        closed_instances(&artifact),
        BTreeSet::from(["artifact".to_string()])
    );

    let cron: HashSet<String> = HashSet::from(["CronMiddleware".to_string()]);
    assert_eq!(
        closed_instances(&cron),
        BTreeSet::from(["cron".to_string()])
    );

    let lsp: HashSet<String> = HashSet::from(["LspMiddleware".to_string()]);
    assert_eq!(closed_instances(&lsp), BTreeSet::from(["lsp".to_string()]));

    let both: HashSet<String> = HashSet::from([
        "WebMiddleware".to_string(),
        "ArtifactMiddleware".to_string(),
    ]);
    assert_eq!(
        closed_instances(&both),
        BTreeSet::from(["artifact".to_string(), "web".to_string()])
    );

    // 未知键 / 非 builtin 策略键：不构成关闭（未知键由 provider 配置层 warn + 忽略）。
    for unknown in ["SomeOtherMiddleware", "McpMiddleware", "web", "Artifact"] {
        let set: HashSet<String> = HashSet::from([unknown.to_string()]);
        assert!(
            closed_instances(&set).is_empty(),
            "{unknown} 不得关闭任何 builtin 实例"
        );
    }
}

#[test]
fn is_closed_reads_the_closed_set() {
    let closed: BTreeSet<String> = BTreeSet::from(["web".to_string()]);
    assert!(is_closed("web", &closed));
    assert!(!is_closed("artifact", &closed));
    assert!(!is_closed("some-user-server", &closed));
}

// ─── 注入策略（A2）────────────────────────────────────────────────────────

/// 改写 `PERI_MCP_BUILTIN` 的 guard：持有进程环境锁，drop 时还原。
///
/// 与 `hooks::loader_test::HomeGuard` 同一模式——`std::env::set_var` 是进程级全局，
/// 不串行会与并行测试竞态。断言消息**不得**回显 env 取值。
struct BuiltinEnvGuard {
    _lock: crate::process_env::EnvLockFile,
    previous: Option<std::ffi::OsString>,
}

impl BuiltinEnvGuard {
    fn set(value: Option<&str>) -> Self {
        let lock = crate::process_env::lock().expect("process env lock");
        let previous = std::env::var_os(BUILTIN_INJECTION_ENV);
        match value {
            Some(value) => std::env::set_var(BUILTIN_INJECTION_ENV, value),
            None => std::env::remove_var(BUILTIN_INJECTION_ENV),
        }
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for BuiltinEnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(BUILTIN_INJECTION_ENV, value),
            None => std::env::remove_var(BUILTIN_INJECTION_ENV),
        }
    }
}

#[test]
fn injection_policy_all_and_none_shapes() {
    let all = BuiltinInjectionPolicy::all();
    assert_eq!(
        all.enabled_instances().to_vec(),
        vec!["web", "artifact", "cron", "lsp", "workspace"],
        "默认注入全部已实现实例（注册表顺序）"
    );
    assert!(all.enables("web") && all.enables("artifact"));
    assert!(all.enables("cron") && all.enables("lsp"));
    assert!(
        all.enables("workspace"),
        "workspace 已实现（W3-A 注册表 T1）⇒ 默认策略必须注入它"
    );
    assert!(!all.enables("not-a-builtin"), "表外名字永不注入");

    let none = BuiltinInjectionPolicy::none();
    assert!(none.enabled_instances().is_empty());
    assert!(
        !none.enables("web")
            && !none.enables("artifact")
            && !none.enables("cron")
            && !none.enables("lsp")
            && !none.enables("workspace")
    );
}

#[test]
fn injection_policy_from_env_three_states() {
    for off in ["off", "0", "OFF", " 0 "] {
        let _guard = BuiltinEnvGuard::set(Some(off));
        assert!(
            builtin_injection_policy_from_env()
                .enabled_instances()
                .is_empty(),
            "关闭取值必须解析为 none"
        );
        assert_eq!(
            builtin_injection_policy_from_env(),
            BuiltinInjectionPolicy::none()
        );
    }
    for on in ["", "1", "true", "yes"] {
        let _guard = BuiltinEnvGuard::set(Some(on));
        assert_eq!(
            builtin_injection_policy_from_env(),
            BuiltinInjectionPolicy::all(),
            "缺省 / 未知取值必须解析为 all"
        );
    }
    let _guard = BuiltinEnvGuard::set(None);
    assert_eq!(
        builtin_injection_policy_from_env(),
        BuiltinInjectionPolicy::all(),
        "未设置（缺省语义）必须解析为 all"
    );
}

// ─── 覆盖规则 1：缺失 ⇒ 插入完整 builtin 条目 ──────────────────────────────

#[test]
fn overlay_inserts_complete_default_entries() {
    let mut map: HashMap<String, McpServerConfig> = HashMap::new();
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).expect("空配置必须可注入");

    assert_eq!(
        map.len(),
        5,
        "五个已实现实例都被注入（含 wave 3 的 workspace）"
    );
    for instance in BUILTIN_MCP_INSTANCES {
        let entry = map.get(instance.name).expect("实例条目必须存在");
        assert!(entry.command.is_none() && entry.url.is_none());
        assert!(entry.args.is_none() && entry.env.is_none());
        assert!(entry.headers.is_none() && entry.oauth.is_none());
        assert!(entry.subscriptions.is_none());
        assert_eq!(
            entry.protocol_version, None,
            "protocol_version 必须为 None，否则 Auto 不探测 server/discover"
        );
        assert_eq!(entry.disabled, None);
        assert_eq!(entry.system_mcp, Some(true));
        assert_eq!(
            entry.source.as_ref(),
            builtin_source(instance.instance).as_ref()
        );
        assert_eq!(entry.system_mcp_timeout, None, "缺省 30 s");
        // 规则 4：overlay 产出必须自身合法（step 7 会再校验一次）。
        entry.validate().expect("注入条目必须通过契约校验");
        // 规则 6：direct 一致性。
        let declared = declared_direct_tools(instance.name).unwrap();
        let declared: Vec<String> = declared.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(entry.system_mcp_tools.as_ref(), Some(&declared));
    }
}

// ─── 覆盖规则 2：存在且无 command/url ──────────────────────────────────────

#[test]
fn overlay_fills_source_and_system_declaration_for_empty_user_entry() {
    // A17：`{"web": {}}` 必须仍是 system 依赖且 direct（不得静默降级为 deferred）。
    let mut map = servers(&[("web", empty_config())]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();

    let web = map.get("web").unwrap();
    assert_eq!(web.source.as_ref(), builtin_source("web").as_ref());
    assert_eq!(web.system_mcp, Some(true));
    let declared: Vec<String> = declared_direct_tools("web")
        .unwrap()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(web.system_mcp_tools.as_ref(), Some(&declared));
    assert_eq!(web.disabled, None);
    web.validate().unwrap();
    // 另一个实例照常注入。
    assert_eq!(
        map.get("artifact").unwrap().source.as_ref(),
        builtin_source("artifact").as_ref()
    );
}

#[test]
fn overlay_keeps_user_disabled_entry_without_system_dependency() {
    // 唯一合法的用户关闭片段（A18）：只写 `disabled: true`。
    let mut map = servers(&[(
        "web",
        McpServerConfig {
            disabled: Some(true),
            ..empty_config()
        },
    )]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();

    let web = map.get("web").unwrap();
    assert_eq!(
        web.source.as_ref(),
        builtin_source("web").as_ref(),
        "禁用实例仍须带 builtin 身份（注册为 Disabled，而不是消失）"
    );
    assert_eq!(web.disabled, Some(true));
    assert_eq!(web.system_mcp, None, "禁用实例不得构成 system 依赖");
    assert_eq!(web.system_mcp_tools, None);
    web.validate().unwrap();
    // 另一实例不受影响。
    assert_eq!(map.get("artifact").unwrap().system_mcp, Some(true));
}

#[test]
fn overlay_preserves_user_fields_without_deep_merge() {
    let mut map = servers(&[(
        "web",
        McpServerConfig {
            system_mcp_timeout: Some(2_000),
            subscriptions: Some(Default::default()),
            ..empty_config()
        },
    )]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();

    let web = map.get("web").unwrap();
    assert_eq!(web.system_mcp_timeout, Some(2_000), "其余字段以用户值为准");
    assert!(web.subscriptions.is_some());
    assert_eq!(web.system_mcp, Some(true), "A17 填充");
    web.validate().unwrap();
}

#[test]
fn overlay_normalizes_user_system_mcp_tools_to_declared_direct_set() {
    // 规则 6：`{"web": {}}` 形态的用户条目里，`system_mcp_tools` 恒等于声明 direct 集合。
    let mut map = servers(&[(
        "web",
        McpServerConfig {
            system_mcp: Some(true),
            system_mcp_tools: Some(vec!["WebSearch".to_string()]),
            ..empty_config()
        },
    )]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();

    let web = map.get("web").unwrap();
    let declared: Vec<String> = declared_direct_tools("web")
        .unwrap()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        web.system_mcp_tools.as_ref(),
        Some(&declared),
        "system_mcp_tools 必须等于声明 direct 集合（含未写全的用户值）"
    );
    web.validate().unwrap();
}

#[test]
fn overlay_leaves_other_servers_untouched() {
    let mut map = servers(&[("some-mcp", stdio_config())]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();

    let other = map.get("some-mcp").unwrap();
    assert_eq!(other.command.as_deref(), Some("npx"));
    assert_eq!(other.system_mcp, None);
    assert_eq!(other.source, None, "非 builtin 条目的 source 不得被改写");
    assert_eq!(map.len(), 6, "五个实例 + 原有 server");
}

// ─── 覆盖规则 3：保留名接管 ⇒ 加载期 typed error ───────────────────────────

#[test]
fn overlay_rejects_reserved_instance_name_takeover() {
    for name in ["web", "artifact", "cron", "lsp", "workspace"] {
        for declared in [
            McpServerConfig {
                command: Some("npx".to_string()),
                ..empty_config()
            },
            McpServerConfig {
                url: Some("https://example.com/mcp".to_string()),
                ..empty_config()
            },
        ] {
            let mut map = servers(&[(name, declared), ("some-mcp", stdio_config())]);
            let before = map.clone();
            let err = apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all())
                .expect_err("保留名被 command/url 接管必须报 typed error");
            assert!(
                matches!(
                    &err,
                    BuiltinOverlayError::ReservedBuiltinInstanceName { name: n } if n == name
                ),
                "{name} 必须以保留名错误拒绝: {err:?}"
            );
            // 错误文本只含实例名，不含路径 / env / 凭据。
            let text = err.to_string();
            assert!(text.contains(name), "错误必须含实例名: {text}");
            assert!(!text.contains("npx"), "错误不得回显 command: {text}");
            assert!(!text.contains("https://"), "错误不得回显 url: {text}");
            assert_no_mutation(&map, &before);
        }
    }
}

#[test]
fn overlay_leaves_non_reserved_transport_declaration_alone() {
    // 非保留名照旧：overlay 不触碰（既不报错也不改 source）。
    let mut map = servers(&[("web", empty_config()), ("some-web", stdio_config())]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();
    let other = map.get("some-web").unwrap();
    assert_eq!(other.command.as_deref(), Some("npx"));
    assert_eq!(other.source, None);
}

/// 保留名表里的每个名字都**已实现** ⇒ overlay 必须为每一个注入完整默认条目。
///
/// （原用例名 `overlay_does_not_inject_reserved_unimplemented_instances` 的前提是「存在保留
/// 但未实现的实例（workspace）」，W3-B 后该前提消失。重述为强断言：不再有「预留但未注入」
/// 的名字——保留名表 ⊆ overlay 产物键集，且每个条目带完整 builtin 身份。）
#[test]
fn overlay_injects_every_reserved_instance() {
    let mut map: HashMap<String, McpServerConfig> = HashMap::new();
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();

    // 强断言：保留名表由 overlay **逐项**覆盖（不是「workspace 不在其中」这类空集断言）。
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        let entry = map
            .get(*name)
            .unwrap_or_else(|| panic!("保留名 {name} 已实现 ⇒ overlay 必须注入它"));
        assert_eq!(
            entry.source.as_ref(),
            builtin_source(name).as_ref(),
            "{name} 必须带 builtin 身份"
        );
        assert_eq!(entry.system_mcp, Some(true), "{name} 必须是 system 依赖");
        assert_eq!(entry.disabled, None, "{name} 默认启用");
        entry.validate().expect("注入条目必须通过契约校验");
    }
    assert_eq!(
        map.len(),
        BUILTIN_RESERVED_INSTANCE_NAMES.len(),
        "空用户配置下的产物键集必须恰等于保留名表（不多不少）"
    );

    // workspace 的声明集合是 7 项（AW3-03），且与注册表派生一致。
    let workspace_tools = map
        .get("workspace")
        .expect("workspace 必须已注入")
        .system_mcp_tools
        .clone()
        .expect("workspace 必须是 system 依赖");
    assert_eq!(
        workspace_tools.len(),
        7,
        "workspace 的 `system_mcp_tools` 必须含全部 7 项（direct）"
    );

    // 已实现的 cron / lsp 同批注入，且带完整 builtin 身份（A5：零工具提升）。
    for instance in ["cron", "lsp"] {
        let entry = map.get(instance).expect("已实现实例必须注入");
        assert_eq!(entry.source.as_ref(), builtin_source(instance).as_ref());
        assert_eq!(entry.system_mcp, Some(true));
        assert_eq!(
            entry.system_mcp_tools,
            Some(Vec::new()),
            "{instance} 的 system_mcp_tools 必须为空集"
        );
    }
    // 用户已有的同名条目（未声明 command/url）必须被填上 builtin 身份（规则 2）。
    let mut map = servers(&[("cron", empty_config())]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();
    assert_eq!(
        map.get("cron").unwrap().source.as_ref(),
        builtin_source("cron").as_ref()
    );
}

// ─── 覆盖规则 5：非法关闭片段 ⇒ 加载期拒绝（A18）────────────────────────────

#[test]
fn overlay_rejects_disabled_with_system_declaration() {
    for name in ["web", "artifact"] {
        let mut map = servers(&[
            (
                name,
                McpServerConfig {
                    disabled: Some(true),
                    system_mcp: Some(true),
                    ..empty_config()
                },
            ),
            ("some-mcp", stdio_config()),
        ]);
        let before = map.clone();
        let err = apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all())
            .expect_err("disabled + system_mcp 组合必须被加载期拒绝");
        assert!(
            matches!(
                &err,
                BuiltinOverlayError::DisabledWithSystemMcp { name: n } if n == name
            ),
            "{name} 必须报非法关闭片段: {err:?}"
        );
        assert!(err.to_string().contains(name), "错误必须含实例名");
        assert_no_mutation(&map, &before);
    }

    // 该检查是**加载期校验**，与注入策略无关：off 时同样拒绝（否则用户在 off 下
    // 仍会撞上 readiness 的 `Err(SystemReadinessError::Disabled)` fatal）。
    let mut off = servers(&[(
        "web",
        McpServerConfig {
            disabled: Some(true),
            system_mcp: Some(true),
            ..empty_config()
        },
    )]);
    assert!(
        apply_builtin_overlay(&mut off, &BuiltinInjectionPolicy::none()).is_err(),
        "非法关闭片段不得因 off 策略被放过"
    );
}

#[test]
fn overlay_does_not_repair_invalid_disabled_entry() {
    // `system_mcp_tools` 无 `system_mcp = true` 是既有契约错误（step 7 的 validate）：
    // overlay 不得把它“修好”，也不得拒绝——保持用户在其余字段上的原值，
    // 由 loader 的 `validate_config` 可见失败。
    let mut map = servers(&[(
        "web",
        McpServerConfig {
            disabled: Some(true),
            system_mcp_tools: Some(vec!["WebSearch".to_string()]),
            ..empty_config()
        },
    )]);
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).unwrap();
    let web = map.get("web").unwrap();
    assert_eq!(
        web.system_mcp_tools,
        Some(vec!["WebSearch".to_string()]),
        "禁用条目只填 source，其余字段以用户值为准"
    );
    assert_eq!(web.source.as_ref(), builtin_source("web").as_ref());
    assert!(
        web.validate().is_err(),
        "非法组合仍必须经由 step 7 的契约校验可见失败"
    );
}

// ─── 策略 none ⇒ 零注入（A2）──────────────────────────────────────────────

#[test]
fn overlay_is_noop_for_none_policy() {
    let mut map = servers(&[("some-mcp", stdio_config())]);
    let before = map.clone();
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::none()).unwrap();
    assert_no_mutation(&map, &before);
    assert_eq!(map.len(), before.len(), "none 策略不得注入任何实例");

    let mut with_user_entry = servers(&[("web", empty_config())]);
    apply_builtin_overlay(&mut with_user_entry, &BuiltinInjectionPolicy::none()).unwrap();
    assert_eq!(
        with_user_entry.get("web").unwrap().source,
        None,
        "none 策略不得为已存在条目补 builtin 身份"
    );
    assert_eq!(with_user_entry.get("web").unwrap().system_mcp, None);

    // 保留名保护**不随策略关闭**：off 只抑制注入，不解除保留名接管拒绝
    // （IF-D15 归一表是静态字面量，与是否注入无关）。
    let mut takeover = servers(&[(
        "web",
        McpServerConfig {
            command: Some("npx".to_string()),
            ..empty_config()
        },
    )]);
    assert!(
        apply_builtin_overlay(&mut takeover, &BuiltinInjectionPolicy::none()).is_err(),
        "off 策略下保留名仍不得被 command/url 接管"
    );
}

// ─── 只对启用的实例生效 ────────────────────────────────────────────────────

#[test]
fn overlay_only_touches_enabled_instances() {
    let only_web = BuiltinInjectionPolicy {
        enabled: vec!["web"],
    };
    let mut map: HashMap<String, McpServerConfig> = HashMap::new();
    apply_builtin_overlay(&mut map, &only_web).unwrap();
    assert!(map.contains_key("web"));
    assert!(!map.contains_key("artifact"));
}
