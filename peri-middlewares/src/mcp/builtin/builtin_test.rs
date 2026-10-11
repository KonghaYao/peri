//! `mcp/builtin/mod.rs` 的行为测试（纯函数层）。
//!
//! 过滤器：`cargo test -p peri-middlewares --lib -- mcp::builtin::tests`
//! （**禁止**裸 `mcp::builtin`——会命中 `builtin_apply_tests` / `builtin_runtime_tests`）。
//!
//! 本文件锁定：冻结 effective 字面量（与 `effective_tool_name()` 输出逐字相等，含 wave 3 的
//! 7 个 `mcp__workspace__*`）、`direct` 集合与 `system_mcp_tools` 集合一致、
//! 关闭集、`is_declared_direct` 的两半语义（已实现实例恒真 / 未知组合恒假）、注入策略三态。
//! overlay 六条覆盖规则与 A3/A18 typed error 的加载链路证据见 `builtin_apply_test.rs`。

use std::collections::{BTreeSet, HashSet};

use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;

use super::*;

// ─── 冻结字面量 ─────────────────────────────────────────────────────────────

/// 冻结的模型面名字表（IF-D5）：`(实例, 原始工具名, effective name 字面量)`。
const FROZEN_NAMES: &[(&str, &str, &str)] = &[
    ("web", "WebSearch", "WebSearch"),
    ("web", "WebFetch", "WebFetch"),
    ("artifact", "artifact", "artifact"),
];

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
            // wave 2：cron 零 direct —— `system_mcp_tools` 恒为空集（A5）。
            "cron" => assert!(
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
/// 但未实现的实例（workspace）」；W3-B 后保留名全部已实现，该前提消失，故按新语义改名并
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
                // wave 2：cron 冻结 `None`（A4：声明段零变化）。
                "cron" => {
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
    _lock: peri_mcp_common::process_env::EnvLockFile,
    previous: Option<std::ffi::OsString>,
}

impl BuiltinEnvGuard {
    fn set(value: Option<&str>) -> Self {
        let lock = peri_mcp_common::process_env::lock().expect("process env lock");
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
        vec!["web", "artifact", "cron", "workspace"],
        "默认注入全部已实现实例（注册表顺序）"
    );
    assert!(all.enables("web") && all.enables("artifact"));
    assert!(all.enables("cron"));
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
