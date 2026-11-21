use super::{
    builtin_mcp, BTreeSet, BuiltinMcpInstance, ConfigSource, Error, HashMap, HashSet,
    McpServerConfig, BUILTIN_MCP_INSTANCES,
};

#[cfg(target_os = "emscripten")]
pub(crate) fn default_subscriptions_for(
    _instance: &str,
) -> Option<peri_acp_types::plugin::McpSubscriptionsConfig> {
    None
}

/// `PERI_MCP_BUILTIN` 环境变量名（紧急闸门，A2：显式运维开关，不是静默降级）。
pub(crate) const BUILTIN_INJECTION_ENV: &str = peri_config::mcp::MCP_BUILTIN_ENV;

/// 默认层注入形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinInjectionMode {
    /// v5 默认：只注入单出口 `peri`。
    SingleExit,
    /// 迁移期回滚：逐能力写回 v4 六实例条目（M4 截止）。
    LegacyInstances,
}

/// 默认层注入策略（显式参数；`_with_paths` 与 overlay 都不读 env）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuiltinInjectionPolicy {
    mode: BuiltinInjectionMode,
    /// v5 模式承载能力名；v4 回滚模式承载迁移期实例名。
    pub(super) enabled: Vec<&'static str>,
}

impl BuiltinInjectionPolicy {
    pub(crate) fn from_assembly_mode(
        mode: peri_acp_types::builtin_mcp::BuiltinAssemblyMode,
    ) -> Self {
        use peri_acp_types::builtin_mcp::BuiltinAssemblyMode;
        match mode {
            BuiltinAssemblyMode::SingleExit => Self::all(),
            BuiltinAssemblyMode::LegacyInstances => Self::legacy_instances(),
            BuiltinAssemblyMode::Disabled => Self::none(),
        }
    }

    /// 注入全部能力（v5 默认；`PERI_MCP_BUILTIN` 缺省 / 未知值）。
    pub(crate) fn all() -> Self {
        Self {
            mode: BuiltinInjectionMode::SingleExit,
            enabled: builtin_mcp::BUILTIN_V5_CAPABILITIES
                .iter()
                .map(|capability| capability.name)
                .collect(),
        }
    }

    /// v5 单出口的显式能力子集（bare 配置只启用 workspace）。
    pub(crate) fn capabilities(names: &[&'static str]) -> Self {
        debug_assert!(
            names
                .iter()
                .all(|name| builtin_mcp::capability(name).is_some()),
            "capability policy names must come from the registry"
        );
        Self {
            mode: BuiltinInjectionMode::SingleExit,
            enabled: builtin_mcp::BUILTIN_V5_CAPABILITIES
                .iter()
                .filter(|capability| names.contains(&capability.name))
                .map(|capability| capability.name)
                .collect(),
        }
    }

    /// 零注入（`PERI_MCP_BUILTIN=off` / `0`）。
    ///
    /// **语义**：off 的退回态没有 builtin 能力；这是显式运维开关。
    pub(crate) fn none() -> Self {
        Self {
            mode: BuiltinInjectionMode::SingleExit,
            enabled: Vec::new(),
        }
    }

    /// 迁移期回滚：逐能力写回 v4 六实例条目（仅 `PERI_MCP_BUILTIN=legacy`）。
    pub(crate) fn legacy_instances() -> Self {
        Self {
            mode: BuiltinInjectionMode::LegacyInstances,
            enabled: BUILTIN_MCP_INSTANCES
                .iter()
                .map(|instance| instance.name)
                .collect(),
        }
    }

    pub(crate) fn legacy_workspace() -> Self {
        Self {
            mode: BuiltinInjectionMode::LegacyInstances,
            enabled: vec!["workspace"],
        }
    }

    /// 从 env 解析：`off` / `0` → [`Self::none`]；`legacy` / `v4` → v4 回滚；
    /// 缺失 / 其它值 → v5 单出口（未知值 warn + `all`，不引入第三种未知状态）。
    pub(crate) fn from_env() -> Self {
        if !crate::platform::IN_PROCESS_BUILTINS {
            // 浏览器 worker 不能托管任何进程内 builtin 实例（平台能力平面的
            // 唯一声明处）；用户显式配置的 remote workspace 仍走 overlay 的
            // url 分支，不受注入策略影响。
            return Self::none();
        }
        let environment =
            peri_config::source::read_environment(&[BUILTIN_INJECTION_ENV]).unwrap_or_default();
        let raw = environment
            .get(BUILTIN_INJECTION_ENV)
            .map(|value| value.trim().to_ascii_lowercase());
        match raw.as_deref() {
            Some("off" | "0") => Self::none(),
            Some("legacy" | "v4") => Self::legacy_instances(),
            Some("on" | "1" | "v5" | "") | None => Self::all(),
            Some(other) => {
                tracing::warn!(
                    value = other,
                    "unknown PERI_MCP_BUILTIN value; using the v5 single exit"
                );
                Self::all()
            }
        }
    }

    /// v5 模式下为能力名；v4 回滚模式下为实例名。
    pub(super) fn enables(&self, name: &str) -> bool {
        self.enabled.contains(&name)
    }

    /// 是否为零注入（`PERI_MCP_BUILTIN=off`）。
    pub(crate) fn is_none(&self) -> bool {
        self.mode == BuiltinInjectionMode::SingleExit && self.enabled.is_empty()
    }

    pub(crate) fn is_legacy(&self) -> bool {
        self.mode == BuiltinInjectionMode::LegacyInstances
    }

    fn capability_enabled(&self, capability_name: &str) -> bool {
        self.mode == BuiltinInjectionMode::SingleExit && self.enables(capability_name)
    }
}

/// 从 env 构造注入策略（**唯一**读 env 的入口，每次配置加载调用一次）。
pub(crate) fn builtin_injection_policy_from_env() -> BuiltinInjectionPolicy {
    BuiltinInjectionPolicy::from_env()
}

/// 默认层 overlay 的加载期错误（typed；错误文本只含实例名，不含路径 / env / 凭据）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum BuiltinOverlayError {
    /// 保留实例名被用户配置用 `command` / `url` 接管（A3）。
    ///
    /// 必须加载期拒绝：parity 与关闭语义按名字反查，外部同名 server 一旦接管该
    /// 名字会继承「按原始名判定」的审批结果，静默移除 `mcp__*` 审批门。
    #[error("builtin 保留实例名不得被 command/url 接管: {name}")]
    ReservedBuiltinInstanceName { name: String },
    /// 禁用与 system 声明同时出现（A18）：非法关闭片段。
    ///
    /// 唯一合法的用户关闭写法是 `{"web": {"disabled": true}}`；`disabled + system_mcp`
    /// 组合今天会走到 readiness 的 `Err(SystemReadinessError::Disabled)` fatal，阻断
    /// **所有** session，因此必须在加载期拒绝。
    #[error("builtin 实例的关闭片段非法（disabled 与 system_mcp 不得同时声明）: {name}")]
    DisabledWithSystemMcp { name: String },
    #[error("remote workspace requires host or global configuration: {name}")]
    UntrustedWorkspaceSource { name: String },
}

/// `(server_name, original_tool)` 是否为已实现实例声明为 direct 的工具。
///
/// `peri` 单出口按原始工具名在整张能力表中定位（原始名全局唯一）；迁移期六实例
/// 仍按实例声明表判定。
pub(crate) fn is_declared_direct(server_name: &str, original_tool: &str) -> bool {
    if builtin_mcp::is_v5_server_name(server_name) {
        return builtin_mcp::capability_of_original_tool(original_tool)
            .is_some_and(|(_, tool)| tool.direct);
    }
    builtin_mcp::find(server_name).is_some_and(|instance| {
        instance
            .tools
            .iter()
            .any(|tool| tool.direct && tool.original_name == original_tool)
    })
}

/// `(server_name, original_tool)` 是否为已实现实例声明的 **host-only** 工具
/// （`model_visible: false`）。
///
/// 未实现 / 未知名与未声明工具恒返回 `false`：可见性默认对模型开放，只有声明表
/// 显式收窄才隐藏（保守默认，不改变既有实例行为）。发现过滤在
/// `tool_bridge::McpToolBridge` 构造点消费；本函数不是授权边界——服务端仍须
/// 自行校验调用上下文。
pub(crate) fn is_declared_model_hidden(server_name: &str, original_tool: &str) -> bool {
    if builtin_mcp::is_v5_server_name(server_name) {
        return builtin_mcp::capability_of_original_tool(original_tool)
            .is_some_and(|(_, tool)| !tool.model_visible);
    }
    builtin_mcp::find(server_name).is_some_and(|instance| {
        instance
            .tools
            .iter()
            .any(|tool| !tool.model_visible && tool.original_name == original_tool)
    })
}

/// IF-D10 的唯一判定入口：v5 能力名命中即关闭对应能力。
///
/// `disabled_middlewares` 同时承载链槽位键与其他 middleware 键；本函数接受 v5
/// 能力名与 builtin 旧 `*Middleware` 键（确定性映射 adapter，M4 删除），其余键
/// 静默跳过。
pub(crate) fn closed_instances(disabled_middlewares: &HashSet<String>) -> BTreeSet<String> {
    closed_capabilities(disabled_middlewares)
        .into_iter()
        .filter_map(|capability_name| {
            builtin_mcp::BUILTIN_V5_CAPABILITIES
                .iter()
                .find(|capability| capability.name == capability_name)
                .map(|capability| capability.legacy_instance.to_string())
        })
        .collect()
}

/// 关闭集的能力身份（v5 名字）；供 router / readiness 等后续消费方使用。
///
/// 同时接受能力名与迁移期旧 `*Middleware` 键；旧键经
/// [`builtin_mcp::capability_of_switch`] 确定性映射，不能 silent no-op（#409 §8.6）。
pub(crate) fn closed_capabilities(
    disabled_middlewares: &HashSet<String>,
) -> BTreeSet<&'static str> {
    disabled_middlewares
        .iter()
        .filter_map(|key| builtin_mcp::capability_of_switch(key).ok())
        .map(|capability| capability.name)
        .collect()
}

/// 当前会话可见的能力名（声明顺序）：迁移期关闭集（旧实例名）减去后剩余。
pub(crate) fn enabled_capabilities(closed: &BTreeSet<String>) -> Vec<&'static str> {
    builtin_mcp::BUILTIN_V5_CAPABILITIES
        .iter()
        .filter(|capability| !closed.contains(capability.legacy_instance))
        .map(|capability| capability.name)
        .collect()
}

/// 能力是否被会话关闭集命中（旧实例名身份）。
pub(crate) fn capability_closed(capability_name: &str, closed: &BTreeSet<String>) -> bool {
    builtin_mcp::capability(capability_name)
        .is_some_and(|capability| closed.contains(capability.legacy_instance))
}

/// 该来源承载的能力是否在关闭集内。
///
/// - 迁移期六实例：来源实例名就是能力身份；
/// - v5 单出口 `peri`：按调用方给出的能力判定；
/// - 其它来源：永不受 builtin 关闭集影响。
pub(crate) fn is_capability_closed(
    capability_name: &str,
    source: Option<&ConfigSource>,
    closed: &BTreeSet<String>,
) -> bool {
    match source {
        Some(ConfigSource::Builtin { instance }) if instance == capability_name => {
            closed.contains(capability_name)
        }
        Some(ConfigSource::Builtin { instance }) if builtin_mcp::is_v5_server_name(instance) => {
            capability_closed(capability_name, closed)
        }
        _ => false,
    }
}

/// `(server, original_tool)` 是否属于关闭的能力。
pub(crate) fn is_tool_closed(
    server_name: &str,
    original_tool: &str,
    closed: &BTreeSet<String>,
) -> bool {
    if builtin_mcp::is_v5_server_name(server_name) {
        return builtin_mcp::capability_of_original_tool(original_tool)
            .is_some_and(|(capability, _)| closed.contains(capability.legacy_instance));
    }
    closed.contains(server_name)
}

/// 必需工具清单按会话关闭集过滤（v5 单出口按工具所属能力）。
///
/// 迁移期实例整体关闭由调用方先行过滤；本函数只处理同一 server 内的逐工具关闭
/// （`peri` 的 direct 工具跨能力聚合）。
pub(crate) fn visible_required_tools(
    server_name: &str,
    tools: &[String],
    closed: &BTreeSet<String>,
) -> Vec<String> {
    if !builtin_mcp::is_v5_server_name(server_name) {
        return tools.to_vec();
    }
    tools
        .iter()
        .filter(|tool| !is_tool_closed(server_name, tool, closed))
        .cloned()
        .collect()
}

/// 资源 URI 是否属于关闭的能力（只对 v5 `peri` 有意义）。
pub(crate) fn is_resource_closed(uri: &str, closed: &BTreeSet<String>) -> bool {
    builtin_mcp::capability_of_resource_uri(uri)
        .is_ok_and(|capability| closed.contains(capability.legacy_instance))
}

/// 订阅资源清单按会话关闭集过滤。
///
/// 迁移期实例：整体关闭由 [`is_closed_source`] 先行判定，本函数原样返回；
/// v5 `peri`：按 URI 归属能力过滤（workspace 关闭时不再订阅 `workspace://git/ref`）。
pub(crate) fn visible_subscription_resources(
    server_name: &str,
    resources: &[String],
    closed: &BTreeSet<String>,
) -> Vec<String> {
    if !builtin_mcp::is_v5_server_name(server_name) {
        return resources.to_vec();
    }
    resources
        .iter()
        .filter(|uri| !is_resource_closed(uri, closed))
        .cloned()
        .collect()
}

/// 该 server name 是否在关闭集内。
pub(crate) fn is_closed(server_name: &str, closed: &BTreeSet<String>) -> bool {
    closed.contains(server_name)
}

/// A policy key closes only the in-process builtin, never an explicitly selected
/// HTTP Workspace using the same logical server name.
pub(crate) fn is_closed_source(
    server_name: &str,
    source: Option<&ConfigSource>,
    closed: &BTreeSet<String>,
) -> bool {
    matches!(source, Some(ConfigSource::Builtin { instance }) if instance == server_name)
        && is_closed(server_name, closed)
}

/// 该来源是否为宿主系统资源面（instructions / meta / agents）的信任来源。
///
/// 这是 **host-assigned 身份**判定，不是 MCP 能力过滤：技能发现与冻结快照
/// 按 `skills_capable` 能力聚合（`McpClientPool::read_skills_catalog`），
/// 不要求来源是 workspace；本函数只决定哪些连接可以承载 `peri-instruction://`
/// / `peri-meta://` 等宿主系统 URI。
pub(crate) fn is_workspace_source(source: Option<&ConfigSource>) -> bool {
    matches!(source, Some(ConfigSource::Builtin { instance }) if instance == "workspace" || builtin_mcp::is_v5_server_name(instance))
        || matches!(source, Some(ConfigSource::WorkspaceRemote))
}

/// 可承载**本地资源面**（project / plugin / builtin 三类资源）的来源。
///
/// 判据是 host-assigned 身份，不是 server 自报：宿主绑定的 builtin 实例
/// （`ConfigSource::Builtin`，不限 `workspace`）与宿主显式选择的远端 Workspace
/// （`ConfigSource::WorkspaceRemote`）。其他 server 上的同 scheme 资源一律按远端
/// 处理（`McpServerConfig::source` 不从用户配置反序列化）。
pub(crate) fn is_local_resource_source(source: Option<&ConfigSource>) -> bool {
    matches!(
        source,
        Some(ConfigSource::Builtin { .. }) | Some(ConfigSource::WorkspaceRemote)
    )
}

/// builtin 实例来源是否在 A24 关闭集内（按来源自带的实例身份匹配）。
///
/// 与 [`is_closed_source`] 的区别：后者要求调用方给出 server name，适用于
/// 「按逻辑名判定」的既有面；本函数直接从 host-assigned 来源取实例身份，
/// 供不绑定单一实例的消费面（Agent registry）使用。
pub(crate) fn is_closed_builtin_source(
    source: Option<&ConfigSource>,
    closed: &BTreeSet<String>,
) -> bool {
    matches!(source, Some(ConfigSource::Builtin { instance }) if closed.contains(instance))
}

/// 默认配置层 overlay（A1：loader step 6.5 的唯一注入点）。
///
/// 七条冻结规则（IF-D3；规则 7 为 Git Watch 下沉新增）：
/// 1. 实例名缺失 → 插入完整 builtin 条目（协议由客户端自动协商）；
/// 2. 实例名存在且未声明 `command`/`url` → 填 `source`，`disabled != Some(true)` 时
///    **同时**填 `system_mcp = Some(true)` 与 `system_mcp_tools = Some(声明 direct 集合)`
///    （A17：否则 `{"web": {}}` 会从 direct 静默降级为 deferred）；`disabled == Some(true)`
///    时只填 `source`（保持 `Disabled` 注册语义，不构成 system 依赖）。
///    本规则**仅对已实现实例生效**——实现入口只遍历 `BUILTIN_MCP_INSTANCES`
///    （`apply_builtin_overlay` 的 for 循环），表外的名字不写 `command`/`url` 时不注入
///    任何条目。wave 3 后该表与 `BUILTIN_RESERVED_INSTANCE_NAMES` 同集
///    （`peri-acp-types/src/builtin_mcp.rs:15-16`：当前无「预留但未实现」的名字），
///    因此这一限定今日没有额外约束对象；未来新增预留名时它重新生效（注入面仍只认
///    已实现表，与规则 3 的保留名接管校验是两个面）；
/// 3. 保留名不允许 command 接管；除 `workspace` 的宿主/全局显式 HTTP 选择外，
///    url 接管同样在加载期拒绝。远端 Workspace 使用独立来源身份；
/// 4. 结果必须通过 loader step 7 的 `validate_config`（本函数只保证自身产出合法）；
/// 5. 关闭片段形状（A18）：`disabled + system_mcp: true` 组合必须在加载期拒绝；
/// 6. builtin 的 direct 集合来自声明表；远端 Workspace 的工具清单仅来自 live
///    `tools/list`，不要求预声明工具名；
/// 7. 默认订阅（Git Watch 下沉，D-5）：`workspace` 实例的 `subscriptions` 在**缺失**
///    时补默认（git ref 资源）；规则 1 随默认条目一并注入。用户显式写 `subscriptions`
///    （含空配置）时**不覆盖**——空配置 ⇒ 退化为「不订阅」（`!is_empty()` 过滤）。
///    与规则 2 的 `system_mcp` 不同，规则 7 对 `disabled == Some(true)` 的条目也照写：
///    它只是配置物料，实例不连接时该字段无效果，回读一致比省一个字段更重要。
///
/// 错误语义：**先检查、后注入**——任一类非法输入都在任何写入之前返回（合法输入
/// 不受影响；非法输入不产生部分注入）。
///
/// 校验与注入的分工：规则 3（保留名接管）与规则 5（非法关闭片段）是**加载期校验**，
/// 与策略无关——`PERI_MCP_BUILTIN=off` 只抑制注入，不解除这两项保护（IF-D15 归一表是
/// 静态字面量，且 `disabled + system_mcp` 在任何策略下都会走到 readiness 的 fatal）；
/// 规则 1 / 2 是**注入面**，受策略控制。
pub(crate) fn apply_builtin_overlay(
    servers: &mut HashMap<String, McpServerConfig>,
    policy: &BuiltinInjectionPolicy,
) -> Result<(), BuiltinOverlayError> {
    if let Some(config) = servers.get("workspace") {
        if config.url.is_some() && !matches!(config.source, None | Some(ConfigSource::Global(_))) {
            return Err(BuiltinOverlayError::UntrustedWorkspaceSource {
                name: "workspace".to_string(),
            });
        }
    }
    // 规则 3：保留名接管检查（含后续波次预留名），首个错误按名字排序稳定返回。
    if let Some(err) = reserved_name_takeover(servers) {
        return Err(err);
    }
    // 规则 5（A18）：禁用与 system 声明同时出现——必须在加载期拒绝，不得落到
    // readiness 的 `Err(SystemReadinessError::Disabled)` fatal。
    if let Some(err) = disabled_with_system_mcp(servers) {
        return Err(err);
    }
    // 规则 1 / 2：只处理策略启用且**已实现**的实例。
    match policy.mode {
        BuiltinInjectionMode::LegacyInstances => apply_v4_instances(servers, policy),
        BuiltinInjectionMode::SingleExit => apply_v5_single_exit(servers, policy),
    }
    Ok(())
}

/// v4 回滚路径：逐能力写回六实例条目（`PERI_MCP_BUILTIN=legacy`，M4 截止）。
fn apply_v4_instances(
    servers: &mut HashMap<String, McpServerConfig>,
    policy: &BuiltinInjectionPolicy,
) {
    for instance in BUILTIN_MCP_INSTANCES.iter() {
        if instance.name == "workspace" {
            if let Some(entry) = servers.get_mut("workspace") {
                if entry.url.is_some() {
                    entry.source = Some(ConfigSource::WorkspaceRemote);
                    if entry.disabled != Some(true) {
                        entry.system_mcp = Some(true);
                        // The live tools/list result, including an empty list, is authoritative.
                        entry.system_mcp_tools = None;
                    }
                    continue;
                }
            }
        }
        if !policy.enables(instance.name) {
            continue;
        }
        let Some(existing) = servers.get(instance.name) else {
            servers.insert(instance.name.to_string(), builtin_default_entry(instance));
            continue;
        };
        let disabled = existing.disabled;
        let entry = servers
            .get_mut(instance.name)
            .expect("上面刚按同一 key 取过不可变引用");
        entry.source = Some(ConfigSource::Builtin {
            instance: instance.instance.to_string(),
        });
        // 规则 7：默认订阅（用户显式配置优先；空配置也是显式配置 ⇒ 不订阅）。
        if entry.subscriptions.is_none() {
            entry.subscriptions = super::default_subscriptions_for(instance.name);
        }
        if disabled != Some(true) {
            entry.system_mcp = Some(true);
            entry.system_mcp_tools = Some(direct_tool_name_strings(instance));
        }
    }
}

/// v5 默认路径：把旧逐实例条目确定性映射为能力开关，只注入 `peri` 单出口。
///
/// 映射规则：
/// 1. `{"<instance>": {"disabled": true}}` → 对应能力关闭；条目本身被移除，
///    不再产生第二个 builtin endpoint。
/// 2. `{"<instance>": {}}` / 未声明 → 能力开启（默认层）。
/// 3. `{"workspace": {"url": ...}}` → 远端 Workspace 保持独立来源身份
///    （`WorkspaceRemote`），不进入 `peri`；这是宿主任命的外部执行环境。
/// 4. `{"peri": {"env": ...}}` 的 env 保留（能力凭据），但 `system_mcp` /
///    `system_mcp_tools` / 能力集合由宿主重算；`command` / `url` 接管在规则 3 拒绝。
fn apply_v5_single_exit(
    servers: &mut HashMap<String, McpServerConfig>,
    policy: &BuiltinInjectionPolicy,
) {
    if policy.enabled.is_empty() {
        // `PERI_MCP_BUILTIN=off`：只抑制注入，不改写用户条目。
        return;
    }
    let mut remote_workspace = false;
    if let Some(entry) = servers.get_mut("workspace") {
        if entry.url.is_some() {
            entry.source = Some(ConfigSource::WorkspaceRemote);
            if entry.disabled != Some(true) {
                entry.system_mcp = Some(true);
                // The live tools/list result, including an empty list, is authoritative.
                entry.system_mcp_tools = None;
            }
            remote_workspace = true;
        }
    }
    let mut disabled_capabilities = BTreeSet::new();
    let mut legacy_web_env = None;
    for capability in builtin_mcp::BUILTIN_V5_CAPABILITIES {
        if capability.name == "workspace" && remote_workspace {
            continue;
        }
        let Some(legacy) = servers.remove(capability.legacy_instance) else {
            continue;
        };
        if legacy.disabled == Some(true) {
            disabled_capabilities.insert(capability.name);
        } else if capability.name == "web" {
            legacy_web_env = legacy.env;
        }
    }
    if remote_workspace {
        disabled_capabilities.insert("workspace");
    }
    let enabled: Vec<&'static str> = builtin_mcp::BUILTIN_V5_CAPABILITIES
        .iter()
        .filter(|capability| policy.capability_enabled(capability.name))
        .filter(|capability| !disabled_capabilities.contains(capability.name))
        .map(|capability| capability.name)
        .collect();
    if enabled.is_empty() {
        // 全部能力关闭（或部署关闭）：不注入空的 system 依赖。
        servers.remove(builtin_mcp::BUILTIN_V5_SERVER_NAME);
        return;
    }

    let existing = servers.remove(builtin_mcp::BUILTIN_V5_SERVER_NAME);
    let disabled = existing.as_ref().and_then(|entry| entry.disabled);
    let mut env = existing
        .as_ref()
        .and_then(|entry| entry.env.clone())
        .unwrap_or_default();
    if enabled.contains(&"web") {
        for (key, value) in legacy_web_env.unwrap_or_default() {
            env.entry(key).or_insert(value);
        }
    }
    // 运行时标记：宿主每次注入都覆盖，用户配置不能扩大能力集。
    env.insert(
        builtin_mcp::BUILTIN_V5_CAPABILITIES_ENV.to_string(),
        enabled.join(","),
    );
    let subscriptions = existing
        .as_ref()
        .and_then(|entry| entry.subscriptions.clone())
        .or_else(|| {
            enabled
                .contains(&"workspace")
                .then(|| super::default_subscriptions_for("workspace"))
                .flatten()
        });
    let direct_tools = direct_tool_names_for(&enabled);
    servers.insert(
        builtin_mcp::BUILTIN_V5_SERVER_NAME.to_string(),
        McpServerConfig {
            command: None,
            args: None,
            env: Some(env),
            url: None,
            headers: None,
            oauth: None,
            disabled,
            dynamic: false,
            subscriptions,
            system_mcp: (disabled != Some(true)).then_some(true),
            system_mcp_tools: (disabled != Some(true)).then_some(direct_tools),
            system_mcp_timeout: existing.and_then(|entry| entry.system_mcp_timeout),
            source: Some(ConfigSource::Builtin {
                instance: builtin_mcp::BUILTIN_V5_SERVER_NAME.to_string(),
            }),
        },
    );
}

/// 启用能力集合的 direct 工具原始名（v5 模型面保持原名）。
fn direct_tool_names_for(capabilities: &[&'static str]) -> Vec<String> {
    let mut names = Vec::new();
    for capability in builtin_mcp::BUILTIN_V5_CAPABILITIES {
        if !capabilities.contains(&capability.name) {
            continue;
        }
        names.extend(
            capability
                .tools
                .iter()
                .filter(|tool| tool.direct)
                .map(|tool| tool.original_name.to_string()),
        );
    }
    names
}

/// 保留名被 `command`/`url` 接管的首个错误（按名字排序，稳定可复核）。
pub(super) fn reserved_name_takeover(
    servers: &HashMap<String, McpServerConfig>,
) -> Option<BuiltinOverlayError> {
    let mut names: Vec<&String> = servers
        .iter()
        .filter(|(name, config)| {
            builtin_mcp::is_reserved_server_name(name)
                && (config.command.is_some()
                    || (config.url.is_some() && name.as_str() != "workspace"))
        })
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names
        .first()
        .map(|name| BuiltinOverlayError::ReservedBuiltinInstanceName {
            name: (*name).clone(),
        })
}

/// A18 检查：已实现实例不得同时声明 `disabled = true` 与 `system_mcp = true`
/// （按注册表顺序取首个，确定性）。
///
/// 范围限定为 overlay 管理的**已实现**实例：实现入口只遍历 `BUILTIN_MCP_INSTANCES`；
/// wave 3 后该表与 `BUILTIN_RESERVED_INSTANCE_NAMES` 同集（`peri-acp-types/src/builtin_mcp.rs:15-16`），
/// 表外名字的同形条目不由本批次管理、保持迁移前语义（既有 `validate_config` / readiness 路径）。
pub(super) fn disabled_with_system_mcp(
    servers: &HashMap<String, McpServerConfig>,
) -> Option<BuiltinOverlayError> {
    let configured = |name: &str| {
        servers
            .get(name)
            .is_some_and(|config| config.disabled == Some(true) && config.system_mcp == Some(true))
    };
    BUILTIN_MCP_INSTANCES
        .iter()
        .find(|instance| configured(instance.name))
        .map(|instance| BuiltinOverlayError::DisabledWithSystemMcp {
            name: instance.name.to_string(),
        })
        .or_else(|| {
            configured("peri").then(|| BuiltinOverlayError::DisabledWithSystemMcp {
                name: "peri".to_string(),
            })
        })
}

/// 规则 1 的完整 builtin 条目。
pub(super) fn builtin_default_entry(instance: &BuiltinMcpInstance) -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        dynamic: false,
        // 必须为 None：显式版本会跳过 Auto 的 `server/discover` 探测。
        // 规则 7：`workspace` 的默认订阅（其余实例为 None）。
        subscriptions: super::default_subscriptions_for(instance.name),
        system_mcp: Some(true),
        system_mcp_tools: Some(direct_tool_name_strings(instance)),
        system_mcp_timeout: None,
        source: Some(ConfigSource::Builtin {
            instance: instance.instance.to_string(),
        }),
    }
}

/// 实例声明为 direct 的原始工具名（`Vec<String>` 形态，供配置写入）。
pub(super) fn direct_tool_name_strings(instance: &BuiltinMcpInstance) -> Vec<String> {
    instance
        .tools
        .iter()
        .filter(|tool| tool.direct)
        .map(|tool| tool.original_name.to_string())
        .collect()
}
