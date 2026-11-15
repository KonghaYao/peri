//! `builtin_mcp::v5` 声明、映射与加载期校验的契约测试。
//!
//! 边界：只断言声明数据与纯查表结果；默认注入、router 与代理行为不在本文件
//! 覆盖范围（它们仍走 v4 路径）。

use std::collections::HashSet;

use crate::tools::ContextRetention;

use super::*;

fn all_tools() -> Vec<(&'static BuiltinCapability, &'static CapabilityTool)> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .flat_map(|capability| capability.tools.iter().map(move |tool| (capability, tool)))
        .collect()
}

const fn test_tool(
    original_name: &'static str,
    model_name: &'static str,
    direct: bool,
) -> CapabilityTool {
    CapabilityTool {
        original_name,
        model_name,
        direct,
        model_visible: true,
        prompt_declaration: None,
        context_retention: ContextRetention::Recomputable,
        completes_on_response: true,
    }
}

const fn test_capability(
    name: &'static str,
    legacy_instance: &'static str,
    tools: &'static [CapabilityTool],
) -> BuiltinCapability {
    BuiltinCapability {
        name,
        legacy_instance,
        legacy_policy_key: "TestMiddleware",
        tools,
        resources: NO_RESOURCES,
        host_injection: NO_HOST_INJECTION,
        execution: in_process("test://capability"),
        state: NO_STATE,
    }
}

const TOOL_A_DIRECT: &[CapabilityTool] = &[test_tool("ToolA", "ToolA", true)];
const TOOL_B_DIRECT: &[CapabilityTool] = &[test_tool("ToolB", "ToolB", true)];
const TOOL_A_DEFERRED: &[CapabilityTool] = &[test_tool("ToolA", "mcp__peri__ToolA", false)];
const TOOL_BAD_MODEL: &[CapabilityTool] = &[test_tool("ToolA", "Wrong", true)];
const TOOL_EMPTY_NAME: &[CapabilityTool] = &[test_tool("", "", true)];
const TOOL_RESERVED_ONLY: &[CapabilityTool] =
    &[test_tool("mcp__peri__Solo", "mcp__peri__Solo", true)];
const TOOL_RESERVED_DIRECT: &[CapabilityTool] =
    &[test_tool("mcp__peri__Shared", "mcp__peri__Shared", true)];
const TOOL_SHARED_DEFERRED: &[CapabilityTool] = &[test_tool("Shared", "mcp__peri__Shared", false)];

#[test]
fn capability_table_is_frozen_and_matches_v5_names() {
    assert_eq!(BUILTIN_V5_SERVER_NAME, "peri");
    assert_eq!(BUILTIN_V5_DEFERRED_TOOL_PREFIX, "mcp__peri__");
    assert_eq!(BUILTIN_V5_RESERVED_SERVER_NAMES, &[BUILTIN_V5_SERVER_NAME]);

    let names: Vec<&str> = BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| capability.name)
        .collect();
    assert_eq!(names, BUILTIN_V5_CAPABILITY_NAMES);
    assert_eq!(
        names.iter().copied().collect::<HashSet<_>>().len(),
        names.len(),
        "能力名必须唯一"
    );
    assert_eq!(
        names,
        vec![
            "workspace",
            "web",
            "artifact",
            "cron",
            "monitor",
            "workflow"
        ]
    );
}

#[test]
fn v5_tool_sets_match_v4_declarations() {
    for capability in BUILTIN_V5_CAPABILITIES {
        let legacy = crate::builtin_mcp::find(capability.legacy_instance)
            .unwrap_or_else(|| panic!("能力 {} 的旧实例必须存在", capability.legacy_instance));
        assert_eq!(capability.name, legacy.name);
        assert_eq!(capability.legacy_policy_key, legacy.policy_key);
        assert_eq!(
            capability.host_injection.task_owner_scoped, legacy.task_owner_scoped,
            "能力 {} 的 task owner 作用域必须与 v4 一致",
            capability.name
        );
        assert_eq!(
            capability.tools.len(),
            legacy.tools.len(),
            "能力 {} 的工具数必须与 v4 一致",
            capability.name
        );
        for (new, old) in capability.tools.iter().zip(legacy.tools) {
            assert_eq!(new.original_name, old.original_name);
            assert_eq!(
                new.direct, old.direct,
                "{} direct 声明漂移",
                new.original_name
            );
            assert_eq!(
                new.model_visible, old.model_visible,
                "{} model_visible 声明漂移",
                new.original_name
            );
            assert_eq!(
                new.prompt_declaration, old.prompt_declaration,
                "{} prompt 声明漂移",
                new.original_name
            );
            assert_eq!(
                new.context_retention, old.context_retention,
                "{} context_retention 声明漂移",
                new.original_name
            );
            assert_eq!(
                new.completes_on_response, old.completes_on_response,
                "{} completes_on_response 声明漂移",
                new.original_name
            );
            assert_eq!(
                legacy_effective_name_for(capability.legacy_instance, new.original_name)
                    .expect("旧名映射必须存在"),
                old.effective_name,
                "{} 旧 effective name 漂移",
                new.original_name
            );
        }
    }
}

#[test]
fn model_names_follow_single_exit_naming() {
    for (capability, tool) in all_tools() {
        if tool.direct {
            assert_eq!(
                tool.model_name, tool.original_name,
                "direct 工具必须保持原名: {}",
                tool.original_name
            );
            assert!(
                !tool.model_name.starts_with(BUILTIN_V5_DEFERRED_TOOL_PREFIX),
                "direct 工具不得占用 deferred 命名空间: {}",
                tool.original_name
            );
        } else {
            assert_eq!(
                tool.model_name,
                format!("{BUILTIN_V5_DEFERRED_TOOL_PREFIX}{}", tool.original_name),
                "deferred 工具必须使用单出口前缀: {}",
                tool.original_name
            );
            assert_ne!(
                tool.model_name, tool.original_name,
                "deferred 工具不得使用裸原名: {}",
                tool.original_name
            );
        }
        assert_eq!(
            capability_tool(capability.name, tool.original_name)
                .expect("执行身份必须可查")
                .model_name,
            tool.model_name
        );
    }
    // 逐字哨兵：表驱动断言在同一次改错下仍可能自洽通过。
    assert_eq!(
        capability_tool("cron", "cron_register")
            .expect("cron_register")
            .model_name,
        "mcp__peri__cron_register"
    );
    assert_eq!(
        capability_tool("workspace", "Read")
            .expect("Read")
            .model_name,
        "Read"
    );
}

#[test]
fn registry_validation_accepts_frozen_table() {
    validate_registry().expect("冻结的 v5 声明表必须通过加载期校验");
    validate_capabilities(BUILTIN_V5_CAPABILITIES).expect("同一张表显式校验也必须通过");
}

#[test]
fn validation_rejects_duplicate_capability_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability("same", "a", TOOL_A_DIRECT),
        test_capability("same", "b", TOOL_B_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateCapabilityName {
            name: "same".to_owned()
        })
    );
}

#[test]
fn validation_rejects_duplicate_legacy_instance() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability("a", "shared", TOOL_A_DIRECT),
        test_capability("b", "shared", TOOL_B_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateLegacyInstance {
            instance: "shared".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_duplicate_original_tool_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability("a", "a", TOOL_A_DEFERRED),
        test_capability("b", "b", TOOL_A_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateOriginalToolName {
            original_name: "ToolA".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_duplicate_model_tool_name() {
    // direct 工具占用 `mcp__peri__*` 原名，与 deferred 模型面冲突。
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability("a", "a", TOOL_RESERVED_DIRECT),
        test_capability("b", "b", TOOL_SHARED_DEFERRED),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateModelToolName {
            model_name: "mcp__peri__Shared".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_invalid_model_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[test_capability("a", "a", TOOL_BAD_MODEL)];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::InvalidModelToolName {
            capability: "a".to_owned(),
            original_name: "ToolA".to_owned(),
            model_name: "Wrong".to_owned(),
            expected: "ToolA".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_reserved_tool_namespace() {
    const CAPABILITIES: &[BuiltinCapability] = &[test_capability("a", "a", TOOL_RESERVED_ONLY)];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::ReservedToolName {
            capability: "a".to_owned(),
            original_name: "mcp__peri__Solo".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_empty_capability_and_tool_name() {
    const EMPTY_TOOLS: &[CapabilityTool] = &[];
    assert_eq!(
        validate_capabilities(&[test_capability("a", "a", EMPTY_TOOLS)]),
        Err(CapabilityRegistryError::EmptyCapabilityTools {
            capability: "a".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability("a", "a", TOOL_EMPTY_NAME)]),
        Err(CapabilityRegistryError::EmptyToolName {
            capability: "a".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability("", "", TOOL_A_DIRECT)]),
        Err(CapabilityRegistryError::EmptyCapabilityName)
    );
}

#[test]
fn user_defined_peri_and_migration_names_are_reserved() {
    for name in [
        "peri",
        "workspace",
        "web",
        "artifact",
        "cron",
        "monitor",
        "workflow",
    ] {
        assert!(is_reserved_server_name(name), "{name} 必须是保留名");
        assert_eq!(
            validate_user_server_name(name),
            Err(CapabilityRegistryError::ReservedUserServerName {
                name: name.to_owned()
            }),
            "{name} 不得被用户配置接管"
        );
    }
    assert!(!is_reserved_server_name("my-server"));
    assert_eq!(validate_user_server_name("my-server"), Ok(()));
    assert_eq!(
        validate_user_server_name(""),
        Err(CapabilityRegistryError::EmptyServerName)
    );
}

#[test]
fn legacy_names_normalize_to_v5_model_names() {
    for capability in BUILTIN_V5_CAPABILITIES {
        let legacy = crate::builtin_mcp::find(capability.legacy_instance).expect("旧实例");
        for (tool, old_tool) in capability.tools.iter().zip(legacy.tools) {
            let legacy_name = old_tool.effective_name;
            let normalized =
                resolve_display_tool_name(legacy_name).expect("旧名必须可归一到展示名");
            assert_eq!(normalized.capability, capability.name);
            assert_eq!(normalized.original_name, tool.original_name);
            assert_eq!(normalized.model_name, tool.model_name);
            assert_eq!(normalized.legacy_effective_name, legacy_name);
            assert_eq!(
                normalized.source,
                if tool.direct {
                    ToolNameSource::SharedDirectName
                } else {
                    ToolNameSource::LegacyName
                }
            );
            assert_eq!(
                normalize_legacy_tool_name(legacy_name).expect("旧名归一 API 必须命中"),
                normalized
            );

            let current = resolve_display_tool_name(tool.model_name).expect("新名必须可归一");
            assert_eq!(current.capability, capability.name);
            assert_eq!(current.original_name, tool.original_name);
            assert_eq!(current.legacy_effective_name, legacy_name);
            assert_eq!(
                current.source,
                if tool.direct {
                    ToolNameSource::SharedDirectName
                } else {
                    ToolNameSource::ModelName
                }
            );
        }
    }
    // 旧 transcript 可能保存 direct 工具的 `mcp__<instance>__*` 拼接拼写；
    // 只做展示归一，模型面名字仍是 direct 原名。
    let namespaced_direct =
        resolve_display_tool_name("mcp__web__WebSearch").expect("旧 namespaced direct 名可归一");
    assert_eq!(namespaced_direct.capability, "web");
    assert_eq!(namespaced_direct.original_name, "WebSearch");
    assert_eq!(namespaced_direct.model_name, "WebSearch");
    assert_eq!(namespaced_direct.source, ToolNameSource::LegacyName);
    assert_eq!(
        normalize_legacy_tool_name("mcp__workspace__Read")
            .expect("旧 namespaced direct 名可归一")
            .model_name,
        "Read"
    );
}

#[test]
fn legacy_instance_and_tool_mapping_is_deterministic() {
    for capability in BUILTIN_V5_CAPABILITIES {
        assert_eq!(
            capability_of_legacy_instance(capability.legacy_instance)
                .expect("旧实例必须映射")
                .name,
            capability.name
        );
        for tool in capability.tools {
            let mapped = map_legacy_tool(capability.legacy_instance, tool.original_name)
                .expect("旧工具必须映射");
            assert_eq!(mapped.capability, capability.name);
            assert_eq!(mapped.original_name, tool.original_name);
            assert_eq!(mapped.model_name, tool.model_name);
            assert_eq!(
                mapped.source,
                if tool.direct {
                    ToolNameSource::SharedDirectName
                } else {
                    ToolNameSource::LegacyName
                }
            );
        }
    }
    assert_eq!(
        capability_of_legacy_instance("nope"),
        Err(CapabilityNameError::UnknownLegacyInstance {
            instance: "nope".to_owned()
        })
    );
    assert_eq!(
        map_legacy_tool("workspace", "nope"),
        Err(CapabilityNameError::UnknownTool {
            capability: "workspace".to_owned(),
            original_name: "nope".to_owned()
        })
    );
}

#[test]
fn execution_routing_rejects_legacy_names() {
    let routed = resolve_model_tool("mcp__peri__cron_register").expect("新 deferred 名可路由");
    assert_eq!(routed.capability, "cron");
    assert_eq!(routed.tool.original_name, "cron_register");

    let direct = resolve_model_tool("Read").expect("direct 原名可路由");
    assert_eq!(direct.capability, "workspace");
    assert_eq!(direct.tool.original_name, "Read");

    for legacy in [
        "mcp__cron__cron_register",
        "mcp__workflow__Workflow",
        "mcp__web__WebSearch",
    ] {
        assert_eq!(
            resolve_model_tool(legacy),
            Err(CapabilityNameError::LegacyNameNotExecutable {
                name: legacy.to_owned()
            }),
            "旧 deferred 名字不得作为执行别名"
        );
    }
    assert_eq!(
        normalize_legacy_tool_name("mcp__peri__cron_register"),
        Err(CapabilityNameError::NotLegacyToolName {
            name: "mcp__peri__cron_register".to_owned()
        }),
        "旧名归一 API 必须拒绝 v5-only 名字"
    );
    assert!(
        normalize_legacy_tool_name("mcp__cron__cron_register").is_ok(),
        "旧名归一 API 必须接受旧 deferred 名"
    );
    for unknown in ["", "mcp__unknown__x", "mcp__peri__unknown", "websearch"] {
        assert_eq!(
            resolve_model_tool(unknown),
            Err(CapabilityNameError::UnknownToolName {
                name: unknown.to_owned()
            }),
            "{unknown} 必须显式失败"
        );
    }
}

#[test]
fn unknown_capability_and_tool_are_explicit() {
    assert!(capability("nope").is_none());
    assert_eq!(
        capability_tool("nope", "Read"),
        Err(CapabilityNameError::UnknownCapability {
            name: "nope".to_owned()
        })
    );
    assert_eq!(
        capability_tool("workspace", "read"),
        Err(CapabilityNameError::UnknownTool {
            capability: "workspace".to_owned(),
            original_name: "read".to_owned()
        }),
        "原始名匹配区分大小写"
    );
    assert_eq!(
        resolve_display_tool_name("mcp__cron__unknown"),
        Err(CapabilityNameError::UnknownToolName {
            name: "mcp__cron__unknown".to_owned()
        })
    );
}

#[test]
fn host_injection_and_resources_are_frozen() {
    let scoped: HashSet<&str> = BUILTIN_V5_CAPABILITIES
        .iter()
        .filter(|capability| capability.host_injection.task_owner_scoped)
        .map(|capability| capability.name)
        .collect();
    assert_eq!(scoped, HashSet::from(["monitor", "workspace"]));
    for capability in BUILTIN_V5_CAPABILITIES {
        if capability.host_injection.task_owner_scoped {
            assert_eq!(
                capability.host_injection.task_owner_meta_key,
                Some(TASK_OWNER_META_KEY)
            );
            assert_eq!(
                capability.host_injection.thread_owner_meta_key,
                Some(THREAD_OWNER_META_KEY)
            );
        } else {
            assert_eq!(capability.host_injection.task_owner_meta_key, None);
            assert_eq!(capability.host_injection.thread_owner_meta_key, None);
        }
    }
    assert_eq!(
        capability("workflow")
            .unwrap()
            .host_injection
            .consumer_meta_key,
        Some(WORKFLOW_CONSUMER_META_KEY)
    );

    let workspace = capability("workspace").unwrap();
    assert_eq!(
        workspace.resources.default_subscriptions,
        &["workspace://git/ref"]
    );
    assert!(workspace.resources.uri_prefixes.contains(&"workspace://"));
    assert!(workspace.resources.uri_prefixes.contains(&"skill://"));
    assert!(capability("cron")
        .unwrap()
        .resources
        .static_resources
        .contains(&"cron://jobs"));
    assert!(capability("workflow")
        .unwrap()
        .resources
        .static_resources
        .contains(&"workflow://runs"));

    let mut prefixes = HashSet::new();
    for capability in BUILTIN_V5_CAPABILITIES {
        for prefix in capability.resources.uri_prefixes {
            assert!(
                prefixes.insert(*prefix),
                "资源 URI 前缀 {prefix} 不得被多个能力声明"
            );
        }
    }
}

#[test]
fn execution_binding_and_state_roots_are_frozen() {
    for capability in BUILTIN_V5_CAPABILITIES {
        match capability.name {
            "workspace" => {
                assert_eq!(
                    capability.execution.binding,
                    CapabilityExecutionBinding::SandboxHttpProxy
                );
                assert_eq!(
                    capability.state.state_root,
                    CapabilityStateRoot::SandboxWorkspace
                );
            }
            _ => {
                assert_eq!(
                    capability.execution.binding,
                    CapabilityExecutionBinding::InProcess
                );
            }
        }
    }
    let web = capability("web").unwrap().state;
    assert_eq!(web.credential_namespace, Some("web"));
    assert_eq!(
        web.credential_keys,
        &[
            "PERI_WEB_SEARCH_URL",
            "PERI_WEB_SEARCH_TOKEN",
            "PERI_WEB_FETCH_URL",
            "PERI_WEB_FETCH_TOKEN",
        ]
    );
    let artifact = capability("artifact").unwrap().state;
    assert_eq!(artifact.credential_namespace, Some("artifact"));
    assert_eq!(
        artifact.credential_keys,
        &["PERI_ARTIFACTS_URL", "PERI_ARTIFACTS_TOKEN"]
    );
    for name in ["workspace", "cron", "monitor", "workflow"] {
        assert_eq!(
            capability(name).unwrap().state.credential_namespace,
            None,
            "{name} 不应声明环境凭据命名空间"
        );
    }
    assert_eq!(
        capability("cron").unwrap().state.state_root,
        CapabilityStateRoot::HostScheduler
    );
    assert_eq!(
        capability("monitor").unwrap().state.state_root,
        CapabilityStateRoot::SessionHostMemory
    );
    assert_eq!(
        capability("workflow").unwrap().state.state_root,
        CapabilityStateRoot::WorkflowRuntime
    );
}

#[test]
fn host_only_and_completion_tools_stay_non_direct() {
    for (_, tool) in all_tools() {
        if matches!(
            tool.original_name,
            "cron_acknowledge"
                | "WorkflowTaskObserved"
                | "WorkflowAgentClaim"
                | "WorkflowConsumerReady"
                | "WorkflowCompletionAck"
                | "WorkflowClose"
                | "WorkflowAgentResult"
        ) {
            assert!(!tool.model_visible, "{} 必须 host-only", tool.original_name);
            assert!(!tool.direct, "{} 必须 deferred", tool.original_name);
        }
    }
}
