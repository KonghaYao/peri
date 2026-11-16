use std::collections::BTreeSet;

use serde_json::json;

use super::*;

fn expected_names(names: &[BuiltinCapability]) -> BTreeSet<BuiltinCapability> {
    names.iter().copied().collect()
}

#[test]
fn capability_names_are_stable_and_unique() {
    assert_eq!(BUILTIN_CAPABILITY_NAMES.len(), BuiltinCapability::ALL.len());
    let names: Vec<_> = BuiltinCapability::ALL
        .iter()
        .map(|capability| capability.name())
        .collect();
    assert_eq!(BUILTIN_CAPABILITY_NAMES, names.as_slice());
    for capability in BuiltinCapability::ALL {
        assert_eq!(capability.name(), capability.legacy_instance_name());
        assert_eq!(
            BuiltinCapability::from_name(capability.name()),
            Some(capability)
        );
    }
    assert_eq!(BuiltinCapability::from_name("web, cron"), None);
}

#[test]
fn repeatable_and_comma_separated_tokens_are_unioned() {
    let request = parse_capability_tokens(["web", "cron,artifact"], CapabilitySource::Cli).unwrap();
    assert_eq!(
        request,
        CapabilityRequest::Names(expected_names(&[
            BuiltinCapability::Artifact,
            BuiltinCapability::Cron,
            BuiltinCapability::Web,
        ]))
    );
    assert_eq!(request.describe(), "web,artifact,cron");
}

#[test]
fn selectors_parse() {
    assert_eq!(
        parse_capability_tokens(["all"], CapabilitySource::Cli).unwrap(),
        CapabilityRequest::All
    );
    assert_eq!(
        parse_capability_tokens(["none"], CapabilitySource::Cli).unwrap(),
        CapabilityRequest::None
    );
    assert_eq!(CapabilityRequest::All.selected().unwrap().len(), 6);
    assert_eq!(CapabilityRequest::None.selected(), None);
}

#[test]
fn unknown_capability_is_typed_and_names_the_source() {
    let error = parse_capability_tokens(["sandbox"], CapabilitySource::Cli).unwrap_err();
    assert_eq!(
        error,
        CapabilityError::UnknownCapability {
            name: "sandbox".to_string(),
            origin: CapabilitySource::Cli,
        }
    );
    let message = error.to_string();
    assert!(message.contains("sandbox"), "{message}");
    for name in BUILTIN_CAPABILITY_NAMES {
        assert!(
            message.contains(name),
            "message should list {name}: {message}"
        );
    }
}

#[test]
fn duplicates_are_typed_errors() {
    assert_eq!(
        parse_capability_tokens(["web,web"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::DuplicateCapability {
            name: "web".to_string(),
            origin: CapabilitySource::Cli,
        }
    );
    assert_eq!(
        parse_capability_tokens(["all", "all"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::DuplicateSelector {
            selector: "all",
            origin: CapabilitySource::Cli,
        }
    );
    assert_eq!(
        parse_capability_tokens(["none", "none"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::DuplicateSelector {
            selector: "none",
            origin: CapabilitySource::Cli,
        }
    );
}

#[test]
fn empty_tokens_are_typed_errors() {
    assert_eq!(
        parse_capability_tokens(std::iter::empty::<&str>(), CapabilitySource::Cli).unwrap_err(),
        CapabilityError::EmptySelection {
            origin: CapabilitySource::Cli,
        }
    );
    for token in ["", " ", "web,", ",web", "web,,cron"] {
        assert_eq!(
            parse_capability_tokens([token], CapabilitySource::Cli).unwrap_err(),
            CapabilityError::EmptyCapabilityToken {
                origin: CapabilitySource::Cli,
            },
            "token: {token:?}"
        );
    }
}

#[test]
fn selector_combinations_are_typed_errors() {
    assert_eq!(
        parse_capability_tokens(["all", "web"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::AllWithNames {
            origin: CapabilitySource::Cli,
        }
    );
    assert_eq!(
        parse_capability_tokens(["web", "none"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::NoneWithNames {
            origin: CapabilitySource::Cli,
        }
    );
    assert_eq!(
        parse_capability_tokens(["all", "none"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::AllAndNone {
            origin: CapabilitySource::Cli,
        }
    );
    assert_eq!(
        parse_capability_tokens(["none", "all"], CapabilitySource::Cli).unwrap_err(),
        CapabilityError::AllAndNone {
            origin: CapabilitySource::Cli,
        }
    );
}

#[test]
fn defaults_enable_every_capability() {
    let resolved = resolve_capabilities(None, None).unwrap();
    assert_eq!(resolved.enabled(), expected_names(&BuiltinCapability::ALL));
    for capability in BuiltinCapability::ALL {
        assert_eq!(
            resolved.decision(capability),
            CapabilityDecision {
                enabled: true,
                source: CapabilitySource::Default,
            }
        );
    }
    assert!(resolved.conflicts().is_empty());
    assert_eq!(
        resolved.enabled_names(),
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
fn extension_replaces_defaults() {
    let document = json!({
        "mcpServers": {
            "peri": {"x-peri": {"capabilities": ["cron"]}}
        }
    });
    let file = parse_mcp_file_capabilities(&document).unwrap();
    assert_eq!(
        file.extension(),
        Some(&CapabilityRequest::Names(expected_names(&[
            BuiltinCapability::Cron
        ])))
    );
    let resolved = resolve_capabilities(None, Some(&file)).unwrap();
    assert_eq!(
        resolved.enabled(),
        expected_names(&[BuiltinCapability::Cron])
    );
    assert_eq!(
        resolved.decision(BuiltinCapability::Cron),
        CapabilityDecision {
            enabled: true,
            source: CapabilitySource::PeriExtension,
        }
    );
    assert_eq!(
        resolved.decision(BuiltinCapability::Web).source,
        CapabilitySource::PeriExtension
    );
    assert!(!resolved.is_enabled(BuiltinCapability::Web));
}

#[test]
fn cli_wins_over_extension_and_conflicts_name_both_sources() {
    let document = json!({
        "mcpServers": {
            "peri": {"x-peri": {"capabilities": ["web"]}}
        }
    });
    let file = parse_mcp_file_capabilities(&document).unwrap();
    let cli = parse_capability_tokens(["cron"], CapabilitySource::Cli).unwrap();
    let resolved = resolve_capabilities(Some(&cli), Some(&file)).unwrap();
    assert_eq!(
        resolved.enabled(),
        expected_names(&[BuiltinCapability::Cron])
    );
    assert_eq!(
        resolved.decision(BuiltinCapability::Cron).source,
        CapabilitySource::Cli
    );

    let conflicts = resolved.conflicts();
    assert_eq!(conflicts.len(), 2, "{conflicts:?}");
    let web = conflicts
        .iter()
        .find(|conflict| conflict.capability == BuiltinCapability::Web)
        .unwrap();
    assert_eq!(web.winner, CapabilitySource::Cli);
    assert_eq!(web.overridden, CapabilitySource::PeriExtension);
    assert!(!web.winner_enabled);
    assert!(web.overridden_enabled);

    let cron = conflicts
        .iter()
        .find(|conflict| conflict.capability == BuiltinCapability::Cron)
        .unwrap();
    assert_eq!(cron.winner, CapabilitySource::Cli);
    assert_eq!(cron.overridden, CapabilitySource::PeriExtension);
    assert!(cron.winner_enabled);
    assert!(!cron.overridden_enabled);
}

#[test]
fn legacy_instance_entries_map_to_capabilities() {
    let document = json!({
        "mcpServers": {
            "web": {},
            "cron": {"disabled": true},
            "artifact": {"disabled": false}
        }
    });
    let file = parse_mcp_file_capabilities(&document).unwrap();
    assert_eq!(
        file.legacy_instances(),
        &BTreeMap::from([
            (BuiltinCapability::Web, true),
            (BuiltinCapability::Artifact, true),
            (BuiltinCapability::Cron, false),
        ])
    );
    let resolved = resolve_capabilities(None, Some(&file)).unwrap();
    assert!(resolved.is_enabled(BuiltinCapability::Web));
    assert_eq!(
        resolved.decision(BuiltinCapability::Web).source,
        CapabilitySource::LegacyInstanceEntry
    );
    assert!(!resolved.is_enabled(BuiltinCapability::Cron));
    assert!(resolved.is_enabled(BuiltinCapability::Artifact));
    assert!(resolved.is_enabled(BuiltinCapability::Monitor));
    assert_eq!(
        resolved.decision(BuiltinCapability::Monitor).source,
        CapabilitySource::Default
    );
    assert!(resolved.conflicts().is_empty());
}

#[test]
fn legacy_instance_entries_reject_reserved_overrides_and_bad_types() {
    let command = json!({"mcpServers": {"web": {"command": "npx"}}});
    assert_eq!(
        parse_mcp_file_capabilities(&command).unwrap_err(),
        CapabilityError::ReservedInstanceOverride {
            name: "web".to_string(),
        }
    );
    let url = json!({"mcpServers": {"workspace": {"url": "http://localhost"}}});
    assert_eq!(
        parse_mcp_file_capabilities(&url).unwrap_err(),
        CapabilityError::ReservedInstanceOverride {
            name: "workspace".to_string(),
        }
    );
    let bad_disabled = json!({"mcpServers": {"web": {"disabled": "no"}}});
    assert_eq!(
        parse_mcp_file_capabilities(&bad_disabled).unwrap_err(),
        CapabilityError::InvalidLegacyDisabled {
            name: "web".to_string(),
        }
    );
    let not_object = json!({"mcpServers": {"web": []}});
    assert_eq!(
        parse_mcp_file_capabilities(&not_object).unwrap_err(),
        CapabilityError::InvalidLegacyInstance {
            name: "web".to_string(),
        }
    );
}

#[test]
fn extension_overrides_legacy_and_reports_conflict() {
    let document = json!({
        "mcpServers": {
            "web": {"disabled": true},
            "peri": {"x-peri": {"capabilities": ["web"]}}
        }
    });
    let file = parse_mcp_file_capabilities(&document).unwrap();
    let resolved = resolve_capabilities(None, Some(&file)).unwrap();
    assert!(resolved.is_enabled(BuiltinCapability::Web));
    assert_eq!(
        resolved.decision(BuiltinCapability::Web).source,
        CapabilitySource::PeriExtension
    );
    let conflict = resolved
        .conflicts()
        .iter()
        .find(|conflict| conflict.capability == BuiltinCapability::Web)
        .unwrap();
    assert_eq!(conflict.winner, CapabilitySource::PeriExtension);
    assert_eq!(conflict.overridden, CapabilitySource::LegacyInstanceEntry);
}

#[test]
fn misplaced_capabilities_are_rejected() {
    let top_level = json!({"capabilities": ["web"]});
    assert_eq!(
        parse_mcp_file_capabilities(&top_level).unwrap_err(),
        CapabilityError::MisplacedCapabilities {
            location: "top-level capabilities",
        }
    );
    let server_entry = json!({"mcpServers": {"peri": {"capabilities": ["web"]}}});
    assert_eq!(
        parse_mcp_file_capabilities(&server_entry).unwrap_err(),
        CapabilityError::MisplacedCapabilities {
            location: "mcpServers.peri.capabilities",
        }
    );
}

#[test]
fn extension_shape_is_validated() {
    let not_object = json!({"mcpServers": {"peri": {"x-peri": []}}});
    assert_eq!(
        parse_mcp_file_capabilities(&not_object).unwrap_err(),
        CapabilityError::InvalidExtension
    );
    let not_array = json!({"mcpServers": {"peri": {"x-peri": {"capabilities": "web"}}}});
    assert_eq!(
        parse_mcp_file_capabilities(&not_array).unwrap_err(),
        CapabilityError::InvalidCapabilitiesValue
    );
    let bad_item = json!({"mcpServers": {"peri": {"x-peri": {"capabilities": ["web", 1]}}}});
    assert_eq!(
        parse_mcp_file_capabilities(&bad_item).unwrap_err(),
        CapabilityError::InvalidCapabilityItem { index: 1 }
    );
    let empty = json!({"mcpServers": {"peri": {"x-peri": {"capabilities": []}}}});
    assert_eq!(
        parse_mcp_file_capabilities(&empty).unwrap_err(),
        CapabilityError::EmptySelection {
            origin: CapabilitySource::PeriExtension,
        }
    );
    let mixed = json!({"mcpServers": {"peri": {"x-peri": {"capabilities": ["all", "web"]}}}});
    assert_eq!(
        parse_mcp_file_capabilities(&mixed).unwrap_err(),
        CapabilityError::AllWithNames {
            origin: CapabilitySource::PeriExtension,
        }
    );
}

#[test]
fn document_shape_is_validated() {
    assert_eq!(
        parse_mcp_file_capabilities(&json!([])).unwrap_err(),
        CapabilityError::InvalidDocument
    );
    assert_eq!(
        parse_mcp_file_capabilities(&json!({"mcpServers": []})).unwrap_err(),
        CapabilityError::InvalidMcpServers
    );
    assert_eq!(
        parse_mcp_file_capabilities(&json!({"mcpServers": {"peri": []}})).unwrap_err(),
        CapabilityError::InvalidPeriEntry
    );
}

#[test]
fn absent_or_unrelated_config_is_empty() {
    for document in [
        json!({}),
        json!({"mcpServers": {"external": {"command": "npx"}}}),
        json!({"meta_harness": {"01_intro": true}}),
        json!({"meta_harness": {"WebMiddleware": false}}),
    ] {
        let file = parse_mcp_file_capabilities(&document).unwrap();
        assert!(file.is_empty(), "{document}");
    }
}

#[test]
fn none_selector_and_disabling_every_legacy_instance_are_empty_errors() {
    let none = parse_capability_tokens(["none"], CapabilitySource::Cli).unwrap();
    assert_eq!(
        resolve_capabilities(Some(&none), None).unwrap_err(),
        CapabilityError::EmptySelection {
            origin: CapabilitySource::Cli,
        }
    );

    let all_disabled = json!({
        "mcpServers": {
            "workspace": {"disabled": true},
            "web": {"disabled": true},
            "artifact": {"disabled": true},
            "cron": {"disabled": true},
            "monitor": {"disabled": true},
            "workflow": {"disabled": true}
        }
    });
    let file = parse_mcp_file_capabilities(&all_disabled).unwrap();
    assert_eq!(
        resolve_capabilities(None, Some(&file)).unwrap_err(),
        CapabilityError::EmptySelection {
            origin: CapabilitySource::LegacyInstanceEntry,
        }
    );
}

#[test]
fn capability_names_match_the_v5_registry() {
    let registry: Vec<&str> = peri_acp_types::builtin_mcp::BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| capability.name)
        .collect();
    assert_eq!(BUILTIN_CAPABILITY_NAMES, registry.as_slice());
}
