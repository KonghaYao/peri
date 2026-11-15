//! v5 声明表的加载期校验（typed error）。
//!
//! 校验只读声明数据，不改变默认装配；生产加载期调用 [`validate_registry`]，
//! 测试用 [`validate_capabilities`] 对合成表覆盖失败分支。

use std::collections::{HashMap, HashSet};

use super::{BuiltinCapability, CapabilityTool, BUILTIN_V5_CAPABILITIES, BUILTIN_V5_SERVER_NAME};

/// 注册表加载期错误（重复 / 冲突 / 保留名 / 资源归属 / 依赖）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityRegistryError {
    /// 能力名不得为空。
    #[error("capability name is empty")]
    EmptyCapabilityName,
    /// 能力名重复。
    #[error("duplicate capability name: {name}")]
    DuplicateCapabilityName { name: String },
    /// `peri` 是 v5 出口保留名，不得被能力或旧实例占用。
    #[error("capability `{capability}` uses reserved builtin name `peri`")]
    ReservedCapabilityName { capability: String },
    /// 迁移期旧实例身份不得为空。
    #[error("capability `{capability}` has an empty legacy instance")]
    EmptyLegacyInstance { capability: String },
    /// 迁移期旧关闭键不得为空。
    #[error("capability `{capability}` has an empty legacy policy key")]
    EmptyLegacyPolicyKey { capability: String },
    /// 旧关闭键必须一对一映射到能力，否则关闭集不确定。
    #[error("duplicate legacy policy key `{key}` in capabilities `{first}` and `{second}`")]
    DuplicateLegacyPolicyKey {
        key: String,
        first: String,
        second: String,
    },
    /// 能力依赖不得引用自身或未声明能力。
    #[error("capability `{capability}` has unknown or self dependency `{dependency}`")]
    InvalidDependency {
        capability: String,
        dependency: String,
    },
    /// 同一依赖不得重复声明。
    #[error("capability `{capability}` declares duplicate dependency `{dependency}`")]
    DuplicateDependency {
        capability: String,
        dependency: String,
    },
    /// 能力依赖必须无环。
    #[error("capability dependency cycle reached `{capability}`")]
    DependencyCycle { capability: String },
    /// 每项能力至少声明一个工具。
    #[error("capability `{capability}` has no tools")]
    EmptyCapabilityTools { capability: String },
    /// 工具原始名不得为空。
    #[error("capability `{capability}` has an empty tool name")]
    EmptyToolName { capability: String },
    /// 迁移期旧工具名必须符合冻结命名规则。
    #[error(
        "capability `{capability}` tool `{original_name}` has legacy name `{legacy_name}`; expected `{expected}`"
    )]
    InvalidLegacyToolName {
        capability: String,
        original_name: String,
        legacy_name: String,
        expected: String,
    },
    /// 迁移期旧实例身份重复会让旧名映射不确定。
    #[error("duplicate legacy instance `{instance}` in capabilities `{first}` and `{second}`")]
    DuplicateLegacyInstance {
        instance: String,
        first: String,
        second: String,
    },
    /// 原始工具名必须全局唯一（v5 §4.1.6）。
    #[error(
        "duplicate original tool name `{original_name}` in capabilities `{first}` and `{second}`"
    )]
    DuplicateOriginalToolName {
        original_name: String,
        first: String,
        second: String,
    },
    /// 模型面名必须全局唯一。
    #[error("duplicate model tool name `{model_name}` in capabilities `{first}` and `{second}`")]
    DuplicateModelToolName {
        model_name: String,
        first: String,
        second: String,
    },
    /// direct / deferred 的模型面命名规则不一致。
    #[error(
        "capability `{capability}` tool `{original_name}` has model name `{model_name}`; expected `{expected}`"
    )]
    InvalidModelToolName {
        capability: String,
        original_name: String,
        model_name: String,
        expected: String,
    },
    /// 原始名占用 MCP 命名空间会与外部 server 名字冲突。
    #[error(
        "capability `{capability}` tool `{original_name}` uses reserved MCP namespace `mcp__`"
    )]
    ReservedToolName {
        capability: String,
        original_name: String,
    },
    /// 资源 URI 前缀不得为空。
    #[error("capability `{capability}` has an empty resource URI prefix")]
    EmptyResourceUriPrefix { capability: String },
    /// 资源 URI 不得为空。
    #[error("capability `{capability}` has an empty resource URI")]
    EmptyResourceUri { capability: String },
    /// 同一资源前缀只能归属一个能力。
    #[error("duplicate resource URI prefix `{prefix}` in capabilities `{first}` and `{second}`")]
    DuplicateResourceUriPrefix {
        prefix: String,
        first: String,
        second: String,
    },
    /// 两个能力的前缀互相包含会让 URI 归属不确定。
    #[error("overlapping resource URI prefixes `{first}` and `{second}` are ambiguous")]
    OverlappingResourceUriPrefix { first: String, second: String },
    /// 资源 URI 必须落在所属能力声明的前缀内。
    #[error("capability `{capability}` resource `{uri}` is outside its declared URI prefixes")]
    ResourceNotOwned { capability: String, uri: String },
    /// 同一静态资源 / 模板只能声明一次。
    #[error("duplicate resource URI `{uri}` in capabilities `{first}` and `{second}`")]
    DuplicateResourceUri {
        uri: String,
        first: String,
        second: String,
    },
    /// 用户 server 名不得为空。
    #[error("user-defined MCP server name is empty")]
    EmptyServerName,
    /// 用户 server 名不得占用 builtin 保留名。
    #[error(
        "user-defined MCP server name `{name}` is reserved for builtin `peri` / migration instances"
    )]
    ReservedUserServerName { name: String },
}

/// 校验冻结的 v5 声明表。
pub fn validate_registry() -> Result<(), CapabilityRegistryError> {
    validate_capabilities(BUILTIN_V5_CAPABILITIES)
}

/// 校验任意能力表：重复 / 冲突 / 命名规则 / 资源归属 / 依赖 / 保留命名空间。
///
/// 独立成泛型入口是为了让测试能用合成表覆盖失败分支；生产加载期调用
/// [`validate_registry`]。
pub fn validate_capabilities(
    capabilities: &[BuiltinCapability],
) -> Result<(), CapabilityRegistryError> {
    let mut capability_names = HashSet::new();
    for capability in capabilities {
        if capability.name.is_empty() {
            return Err(CapabilityRegistryError::EmptyCapabilityName);
        }
        if !capability_names.insert(capability.name) {
            return Err(CapabilityRegistryError::DuplicateCapabilityName {
                name: capability.name.to_owned(),
            });
        }
    }

    let mut legacy_instances: HashMap<&str, &str> = HashMap::new();
    let mut legacy_policy_keys: HashMap<&str, &str> = HashMap::new();
    let mut original_names: HashMap<&str, &str> = HashMap::new();
    let mut model_names: HashMap<&str, &str> = HashMap::new();
    let mut prefix_owners: HashMap<&str, &str> = HashMap::new();
    let mut resource_owners: HashMap<&str, &str> = HashMap::new();
    let mut reserved_names = Vec::new();

    for capability in capabilities {
        if capability.name == BUILTIN_V5_SERVER_NAME
            || capability.legacy_instance == BUILTIN_V5_SERVER_NAME
        {
            return Err(CapabilityRegistryError::ReservedCapabilityName {
                capability: capability.name.to_owned(),
            });
        }
        if capability.legacy_instance.is_empty() {
            return Err(CapabilityRegistryError::EmptyLegacyInstance {
                capability: capability.name.to_owned(),
            });
        }
        if let Some(first) = legacy_instances.insert(capability.legacy_instance, capability.name) {
            return Err(CapabilityRegistryError::DuplicateLegacyInstance {
                instance: capability.legacy_instance.to_owned(),
                first: first.to_owned(),
                second: capability.name.to_owned(),
            });
        }
        if capability.legacy_policy_key.is_empty() {
            return Err(CapabilityRegistryError::EmptyLegacyPolicyKey {
                capability: capability.name.to_owned(),
            });
        }
        if capability_names.contains(capability.legacy_policy_key) {
            // 同一字符串既是能力名又是旧键会让开关来源不可判定。
            return Err(CapabilityRegistryError::DuplicateLegacyPolicyKey {
                key: capability.legacy_policy_key.to_owned(),
                first: "capability name".to_owned(),
                second: capability.name.to_owned(),
            });
        }
        if let Some(first) =
            legacy_policy_keys.insert(capability.legacy_policy_key, capability.name)
        {
            return Err(CapabilityRegistryError::DuplicateLegacyPolicyKey {
                key: capability.legacy_policy_key.to_owned(),
                first: first.to_owned(),
                second: capability.name.to_owned(),
            });
        }

        let mut seen_dependencies = HashSet::new();
        for dependency in capability.dependencies {
            if !seen_dependencies.insert(*dependency) {
                return Err(CapabilityRegistryError::DuplicateDependency {
                    capability: capability.name.to_owned(),
                    dependency: (*dependency).to_owned(),
                });
            }
            if *dependency == capability.name || !capability_names.contains(*dependency) {
                return Err(CapabilityRegistryError::InvalidDependency {
                    capability: capability.name.to_owned(),
                    dependency: (*dependency).to_owned(),
                });
            }
        }

        for prefix in capability.resources.uri_prefixes {
            if prefix.is_empty() {
                return Err(CapabilityRegistryError::EmptyResourceUriPrefix {
                    capability: capability.name.to_owned(),
                });
            }
            if let Some(first) = prefix_owners.insert(*prefix, capability.name) {
                return Err(CapabilityRegistryError::DuplicateResourceUriPrefix {
                    prefix: (*prefix).to_owned(),
                    first: first.to_owned(),
                    second: capability.name.to_owned(),
                });
            }
        }
        for uri in capability
            .resources
            .static_resources
            .iter()
            .chain(capability.resources.resource_templates)
        {
            if uri.is_empty() {
                return Err(CapabilityRegistryError::EmptyResourceUri {
                    capability: capability.name.to_owned(),
                });
            }
            if !capability
                .resources
                .uri_prefixes
                .iter()
                .any(|prefix| uri.starts_with(prefix))
            {
                return Err(CapabilityRegistryError::ResourceNotOwned {
                    capability: capability.name.to_owned(),
                    uri: (*uri).to_owned(),
                });
            }
            if let Some(first) = resource_owners.insert(*uri, capability.name) {
                return Err(CapabilityRegistryError::DuplicateResourceUri {
                    uri: (*uri).to_owned(),
                    first: first.to_owned(),
                    second: capability.name.to_owned(),
                });
            }
        }

        if capability.tools.is_empty() {
            return Err(CapabilityRegistryError::EmptyCapabilityTools {
                capability: capability.name.to_owned(),
            });
        }
        for tool in capability.tools {
            if tool.original_name.is_empty() {
                return Err(CapabilityRegistryError::EmptyToolName {
                    capability: capability.name.to_owned(),
                });
            }
            if tool.original_name.starts_with("mcp__") {
                // 保留命名空间检查放在全局唯一性之后：先让「direct 原名抢占
                // deferred 模型面」这类跨能力冲突得到确定的 DuplicateModelToolName。
                reserved_names.push((capability.name, tool.original_name));
            }
            let expected = expected_model_name(tool);
            if tool.model_name != expected {
                return Err(CapabilityRegistryError::InvalidModelToolName {
                    capability: capability.name.to_owned(),
                    original_name: tool.original_name.to_owned(),
                    model_name: tool.model_name.to_owned(),
                    expected,
                });
            }
            let expected_legacy = expected_legacy_name(capability, tool);
            if tool.legacy_name != expected_legacy {
                return Err(CapabilityRegistryError::InvalidLegacyToolName {
                    capability: capability.name.to_owned(),
                    original_name: tool.original_name.to_owned(),
                    legacy_name: tool.legacy_name.to_owned(),
                    expected: expected_legacy,
                });
            }
            if let Some(first) = original_names.get(tool.original_name) {
                return Err(CapabilityRegistryError::DuplicateOriginalToolName {
                    original_name: tool.original_name.to_owned(),
                    first: (*first).to_owned(),
                    second: capability.name.to_owned(),
                });
            }
            original_names.insert(tool.original_name, capability.name);
            if let Some(first) = model_names.get(tool.model_name) {
                return Err(CapabilityRegistryError::DuplicateModelToolName {
                    model_name: tool.model_name.to_owned(),
                    first: (*first).to_owned(),
                    second: capability.name.to_owned(),
                });
            }
            model_names.insert(tool.model_name, capability.name);
        }
    }
    if let Some((capability, original_name)) = reserved_names.first().copied() {
        return Err(CapabilityRegistryError::ReservedToolName {
            capability: capability.to_owned(),
            original_name: original_name.to_owned(),
        });
    }

    reject_overlapping_prefixes(&prefix_owners)?;
    reject_dependency_cycles(capabilities)?;
    Ok(())
}

fn reject_overlapping_prefixes(
    prefix_owners: &HashMap<&str, &str>,
) -> Result<(), CapabilityRegistryError> {
    let mut prefixes: Vec<&str> = prefix_owners.keys().copied().collect();
    prefixes.sort_unstable();
    for window in prefixes.windows(2) {
        if window[1].starts_with(window[0]) {
            return Err(CapabilityRegistryError::OverlappingResourceUriPrefix {
                first: window[0].to_owned(),
                second: window[1].to_owned(),
            });
        }
    }
    Ok(())
}

fn reject_dependency_cycles(
    capabilities: &[BuiltinCapability],
) -> Result<(), CapabilityRegistryError> {
    fn visit<'a>(
        name: &'a str,
        capabilities: &'a [BuiltinCapability],
        visiting: &mut HashSet<&'a str>,
        done: &mut HashSet<&'a str>,
    ) -> Result<(), CapabilityRegistryError> {
        if done.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name) {
            return Err(CapabilityRegistryError::DependencyCycle {
                capability: name.to_owned(),
            });
        }
        if let Some(capability) = capabilities.iter().find(|candidate| candidate.name == name) {
            for dependency in capability.dependencies {
                visit(dependency, capabilities, visiting, done)?;
            }
        }
        visiting.remove(name);
        done.insert(name);
        Ok(())
    }

    let mut done = HashSet::new();
    for capability in capabilities {
        visit(
            capability.name,
            capabilities,
            &mut HashSet::new(),
            &mut done,
        )?;
    }
    Ok(())
}

fn expected_legacy_name(capability: &BuiltinCapability, tool: &CapabilityTool) -> String {
    if tool.direct {
        tool.original_name.to_owned()
    } else {
        format!(
            "mcp__{}__{}",
            capability.legacy_instance, tool.original_name
        )
    }
}

fn expected_model_name(tool: &CapabilityTool) -> String {
    if tool.direct {
        tool.original_name.to_owned()
    } else {
        format!(
            "{}{}",
            super::BUILTIN_V5_DEFERRED_TOOL_PREFIX,
            tool.original_name
        )
    }
}
