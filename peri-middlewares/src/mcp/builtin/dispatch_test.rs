//! `dispatch` 层（实例名 → handler）的 crate 内证据（owner H-01，接线扩展 H-05）。
//!
//! 覆盖口径（主 plan §6 H-01/H-05 行的闸门用例）：工厂覆盖的实例是**精确**集合——注册表里
//! 每个已实现实例各得自己的变体（保留名表与已实现实例表在 W3-B 后逐项一致，**未接线集合为空**），
//! 表外的名字一律 `None`：由 `runtime` 按 `BuiltinSpawnError::HandlerNotWired` 收口，**不得**
//! 静默回退到任一已实现实例。
//!
//! `workspace` 是唯一的例外形状（AW3-11）：它的工厂 arm **无条件**构造——缺 session 级输入
//! 时仍返回 `Workspace` 变体（可见但退化），因此 `None` 在这一支上不代表「未接线」。
//!
//! 两条用例的分工：`dispatch_factory_covers_implemented_instances_only` 从注册表派生断言
//! 「未接线集合为空 + 表外名字恒 `None`」；`dispatch_covers_cron_and_lsp_variants` 用带实例
//! 输入的上下文断言各实例拿到**自己的**变体（含「注入的同一份状态对象进了 handler」）以及
//! `workspace` 在**无**输入时的可见但退化。
//!
//! V 矩阵（v4-part-3 sub-plan V §8）第 20/22/23 行的具名用例在同文件末尾：
//! - [`all_registered_instances_have_handler`]：注册表 → 工厂的一一映射（遍历注册表派生）；
//! - [`builtin_source_propagates`]：cron / lsp 的 `ConfigSource::Builtin` 经 overlay →
//!   建传输（三分类）→ status 快照 → `DiscoverMCP` 只读投影保留，transport 分类 = builtin；
//! - [`call_tool_uses_shared_result_mapping`]：真 handler 经 effective 桥的成功 / 业务 Err /
//!   未知工具三形态都走 `web::invoke_tool_call` 唯一共享助手；
//! - [`call_tool_error_text_is_fixed_and_redacted`]：cron 两类 / LSP 六类业务错误经 handler
//!   路径的文本全部等于固定脱敏文本（`LspToolError::NotReady` 的可触发性与现场判定见该
//!   用例文档，属 UNVERIFIED，不写 flaky 断言）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::{
    find, is_reserved_instance_name, BUILTIN_MCP_INSTANCES, BUILTIN_RESERVED_INSTANCE_NAMES,
};
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_agent::agent::async_tasks::TaskManager as ConcreteTaskManager;
use peri_agent::tools::{BaseTool, ToolContext};
use peri_resources::lsp::config::{LspConfigFile, LspServerConfig};
use peri_resources::lsp::pool::LspServerPool;
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode},
    service::{Peer, RoleClient, ServiceError},
    ServerHandler,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{builtin_server_handler, BuiltinServerHandler};
use crate::cron::{CronScheduler, CronTrigger, MAX_CRON_TASKS};
use crate::lsp::tool::LspTool;
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::context::{
    BuiltinInstanceContext, CronInstanceInput, LspInstanceInput, WorkspaceInstanceInput,
};
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BuiltinServerExit,
    TickCloseOutcome, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::builtin::web::invoke_tool_call;
use crate::mcp::builtin::{apply_builtin_overlay, BuiltinInjectionPolicy};
use crate::mcp::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper, OAuthStatus,
};
use crate::mcp::discover_tool::DiscoverMCPTool;
use crate::mcp::tool_bridge::{build_deferred_tool_bridges, McpToolBridge};
use crate::mcp::transport::{require_known_builtin_instance, TransportConfig, TransportKind};
use crate::mcp::ToolCallError;

/// `workspace` 的 session 级输入夹具（AW3-11）：**真实** per-session `TaskManager` +
/// session 级回调（本文件不断言回调触发，只断言「分派不因输入有无而改变」）。
fn workspace_input() -> WorkspaceInstanceInput {
    WorkspaceInstanceInput {
        task_manager: Some(Arc::new(ConcreteTaskManager::new()) as Arc<dyn TaskManager>),
        on_bg_complete: Some(Arc::new(|_: &BackgroundTaskResult, _: BgTaskKind| {})),
    }
}

#[test]
fn dispatch_factory_covers_implemented_instances_only() {
    // 工厂只按实例名分派：上下文的 `cwd` 只被 artifact 用作解析根，实例输入（cron / lsp）
    // 是否齐备由 `runtime` 在调本工厂**之前**判定，因此这里用一个最小上下文即可。
    let ctx = BuiltinInstanceContext::new(".");
    assert!(
        matches!(
            builtin_server_handler("web", &ctx),
            Some(BuiltinServerHandler::Web(_))
        ),
        "web 必须有 handler"
    );
    assert!(
        matches!(
            builtin_server_handler("artifact", &ctx),
            Some(BuiltinServerHandler::Artifact(_))
        ),
        "artifact 必须有 handler"
    );

    // `workspace`（AW3-11）：**无**任何实例输入的最小上下文也必须拿到自己的变体——
    // 它的输入缺失是「可见但退化」而不是 `HandlerNotWired`，故本工厂的 arm 必须**无条件**
    // 构造（若有人把它写成 `ctx.workspace.as_ref().map(...)`，本断言即红）。
    assert!(
        ctx.workspace.is_none(),
        "前置：最小上下文不带 workspace 输入（这正是本断言要覆盖的形态）"
    );
    assert!(
        matches!(
            builtin_server_handler("workspace", &ctx),
            Some(BuiltinServerHandler::Workspace(_))
        ),
        "workspace 缺 session 级输入仍必须有 handler（可见但退化，不是 HandlerNotWired）"
    );

    // AW3-09：未接线名字集合**为空**——保留名表与已实现实例表逐项一致（两表派生，
    // 任一侧新增名字时本段自动跟随）。这里断言的是当前事实而不是「集合非空」：
    // workspace 接线后不再有「已注册但未接线」的名字，`HandlerNotWired` 只在工厂的
    // `_ => None` 分支上留给**将来**新增的保留名。
    let unwired: Vec<&str> = BUILTIN_RESERVED_INSTANCE_NAMES
        .iter()
        .copied()
        .filter(|name| {
            !BUILTIN_MCP_INSTANCES
                .iter()
                .any(|implemented| implemented.name == *name)
        })
        .collect();
    assert!(
        unwired.is_empty(),
        "保留名表必须与已实现实例表一致（未接线集合为空），实际仍缺 handler 的保留名：{unwired:?}"
    );
    // 同一事实的正面表述（遍历注册表派生）：最小上下文里**只有**「需要实例输入」的实例
    // 可以没有 handler——cron / lsp 的缺输入由 seam 在工厂之前收口 `InstanceInputMissing`，
    // 其余每一个已实现实例（含无输入的 workspace）都必须拿到 handler。`workspace` 若被写成
    // 条件构造，本断言即红。
    let none_with_minimal_ctx: Vec<&str> = BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| instance.name)
        .filter(|name| builtin_server_handler(name, &ctx).is_none())
        .collect();
    assert_eq!(
        none_with_minimal_ctx,
        vec!["cron", "lsp"],
        "最小上下文里没有 handler 的已实现实例必须恰为「需要实例输入」的两个（缺输入归 seam 前置判定，\
         不是未接线）；workspace 属可见但退化，必须仍有 handler"
    );

    // 表外的名字（外部 MCP / 拼错的实例名）：非保留性先落成前置断言，再验同样的 None。
    let unknown = "not-a-builtin";
    assert!(
        !is_reserved_instance_name(unknown),
        "前置：{unknown} 必须在保留名表之外"
    );
    assert!(
        find(unknown).is_none(),
        "前置：{unknown} 必须在已实现实例表之外"
    );
    assert!(
        builtin_server_handler(unknown, &ctx).is_none(),
        "{unknown}: 表外名字不得产出 handler（不静默回退）"
    );
}

/// H-05 接线闸门：已实现实例各得**自己的**变体（cron ⇒ `Cron`、lsp ⇒ `Lsp`、
/// workspace ⇒ `Workspace`），状态对象是注入的**同一份**（A1：组合根构造、同一份 `Arc`），
/// 且表外名字恒 `None`。
///
/// `workspace` 的具名用例是 AW3-11 的「可见但退化」：本用例的上下文**不带** workspace
/// 输入（`ctx.workspace == None`），工厂仍必须返回 `Workspace` 变体而不是 `None`。
#[test]
fn dispatch_covers_cron_and_lsp_variants() {
    // cron 输入：真实 scheduler（触发通道无人消费即可——本用例不驱动 tick）。
    let (trigger_tx, _trigger_rx) = mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(trigger_tx)));
    // lsp 输入：真实 pool（`LspServerPool::new` 惰性，不拉任何 language server 进程）。
    let pool = Arc::new(LspServerPool::new(".", LspConfigFile::default()));

    let ctx = BuiltinInstanceContext::new(".")
        .with_cron(CronInstanceInput {
            scheduler: Arc::clone(&scheduler),
            tick_enabled: false,
        })
        .with_lsp(LspInstanceInput {
            pool: Arc::clone(&pool),
        });

    // web / artifact：原有断言保留（各自的变体，不因新增两条 arm 而改派）。
    assert!(
        matches!(
            builtin_server_handler("web", &ctx),
            Some(BuiltinServerHandler::Web(_))
        ),
        "web 必须仍得自己的变体"
    );
    assert!(
        matches!(
            builtin_server_handler("artifact", &ctx),
            Some(BuiltinServerHandler::Artifact(_))
        ),
        "artifact 必须仍得自己的变体"
    );

    // cron：自己的变体。`CronMcpServer` **没有** `#[cfg(test)]` 取值访问器
    // （本任务的文件 owner 清单不含 `cron.rs`，不为断言放宽生产可见性），因此这里以
    // 变体身份为准；「三个工具持同一份 scheduler」由 `mcp::builtin::cron` 的用例覆盖。
    let cron_handler = builtin_server_handler("cron", &ctx).expect("cron 必须有 handler");
    assert!(
        matches!(cron_handler, BuiltinServerHandler::Cron(_)),
        "cron 必须得 cron 变体（不得回退到别的实例）"
    );

    // lsp：自己的变体 + 注入的**同一份** pool（`Arc::ptr_eq`：不是复制、不是重新装配）。
    let lsp_handler = builtin_server_handler("lsp", &ctx).expect("lsp 必须有 handler");
    match lsp_handler {
        BuiltinServerHandler::Lsp(server) => assert!(
            Arc::ptr_eq(server.pool(), &pool),
            "lsp handler 必须持有注入的同一份 pool（A1：组合根构造、同一份 Arc）"
        ),
        _ => panic!("lsp 必须得 lsp 变体（不得回退到别的实例）"),
    }

    // `workspace`（AW3-11）：**可见但退化**——即使上下文**不带** session 级输入，工厂也必须
    // 返回 `Workspace` 变体（`None` 在分支上不是 `HandlerNotWired`）。
    assert!(
        is_reserved_instance_name("workspace"),
        "前置：workspace 必须在保留名表内"
    );
    assert!(
        BUILTIN_MCP_INSTANCES
            .iter()
            .any(|implemented| implemented.name == "workspace"),
        "前置：workspace 必须已进已实现实例表（W3-A 注册表 T1）"
    );
    assert!(
        ctx.workspace.is_none(),
        "前置：本上下文**不带** workspace 输入（正是「可见但退化」要覆盖的形态）"
    );
    let workspace_handler = builtin_server_handler("workspace", &ctx)
        .expect("workspace 缺 session 级输入仍必须装配（可见但退化，不是 HandlerNotWired）");
    assert!(
        matches!(workspace_handler, BuiltinServerHandler::Workspace(_)),
        "workspace 必须得 workspace 变体（不得回退到别的实例）"
    );

    // 带 session 级输入时仍是同一变体（输入只改 `Bash` 的能力，不改分派）。
    let with_input = BuiltinInstanceContext::new(".").with_workspace(workspace_input());
    assert!(
        matches!(
            builtin_server_handler("workspace", &with_input),
            Some(BuiltinServerHandler::Workspace(_))
        ),
        "带齐 workspace 输入时同样得 workspace 变体"
    );

    // 表外名字：与上下文是否带输入无关，恒 `None`。
    let unknown = "not-a-builtin";
    assert!(
        !is_reserved_instance_name(unknown),
        "前置：{unknown} 必须在保留名表之外"
    );
    assert!(
        builtin_server_handler(unknown, &ctx).is_none(),
        "{unknown}: 表外名字不得产出 handler"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 20 行：注册表 → 工厂映射；builtin 身份经 discover / status 传播
// ══════════════════════════════════════════════════════════════════════════════

/// wave 2 的**被验实例**（本文件三处断言的被验对象）。
///
/// 只固定实例身份；工具名、键、数量、effective name 一律读注册表（`find` / `tools`），
/// 不出现第二份清单——注册表增删工具时本文件的循环自动跟随。
const WAVE2_INSTANCES: [&str; 2] = ["cron", "lsp"];

/// V 矩阵第 20 行：注册表里**每一个**已实现实例经工厂都拿到 handler，且拿到的是**自己的**
/// 分支；表外名字恒 `None`。
///
/// 遍历 `BUILTIN_MCP_INSTANCES` 派生（不把实例名写成常量列表），变体身份 → 实例名的对应
/// 关系由注册表名字驱动；「不是静默回退到同一个 handler」由 `ServerInfo` 名字两两不同证伪。
#[test]
fn all_registered_instances_have_handler() {
    let fixture = CrossFixture::new();
    let ctx = &fixture.ctx;

    let mut dispatched: Vec<&'static str> = Vec::new();
    let mut server_names: Vec<String> = Vec::new();
    for instance in BUILTIN_MCP_INSTANCES {
        let handler = builtin_server_handler(instance.name, ctx).unwrap_or_else(|| {
            panic!(
                "{}: 已实现实例必须有 handler（上下文已给齐 cron/lsp 输入）",
                instance.name
            )
        });
        let variant = match &handler {
            BuiltinServerHandler::Web(_) => "web",
            BuiltinServerHandler::Artifact(_) => "artifact",
            BuiltinServerHandler::Cron(_) => "cron",
            BuiltinServerHandler::Lsp(_) => "lsp",
            BuiltinServerHandler::Workspace(_) => "workspace",
        };
        assert_eq!(
            variant, instance.name,
            "注册表实例 {} 必须得到**自己的**变体（实际 {variant}）",
            instance.name
        );
        dispatched.push(variant);
        server_names.push(handler.get_info().server_info.name);
    }
    assert_eq!(
        dispatched.len(),
        BUILTIN_MCP_INSTANCES.len(),
        "分派成功数必须等于注册表已实现实例数"
    );
    let distinct = {
        let mut names = server_names.clone();
        names.sort();
        names.dedup();
        names.len()
    };
    assert_eq!(
        distinct,
        BUILTIN_MCP_INSTANCES.len(),
        "每个实例的 `ServerInfo` 名字必须互不相同（否则是同一 handler 实现被多个名字静默复用）：{server_names:?}"
    );

    // 「保留但未实现」的名字（从保留名表派生）：**当前为空**——五个保留名全部已实现
    // （AW3-09）。这条强断言取代了原来的「集合非空」前置：接线完成后再出现空集才是事实，
    // 保留名若新增未实现项，本断言即红（而不是让下面的 None 循环静默退化成空转）。
    let unimplemented: Vec<&str> = BUILTIN_RESERVED_INSTANCE_NAMES
        .iter()
        .copied()
        .filter(|name| !BUILTIN_MCP_INSTANCES.iter().any(|done| done.name == *name))
        .collect();
    assert!(
        unimplemented.is_empty(),
        "保留名表必须与已实现实例表一致（未接线集合为空），实际仍缺 handler 的保留名：{unimplemented:?}"
    );

    // 未知实例名 ②：表外名字（注册表与保留名表都查不到）。
    let outside = "nope";
    assert!(
        !is_reserved_instance_name(outside) && find(outside).is_none(),
        "前置：{outside} 必须在两张表之外"
    );
    assert!(
        builtin_server_handler(outside, ctx).is_none(),
        "{outside}: 表外名字不得产出 handler"
    );

    println!(
        "[W2 dispatch] instances={} handlers={} distinct_server_info={distinct} unknown_none={}",
        BUILTIN_MCP_INSTANCES.len(),
        dispatched.len(),
        unimplemented.len() + 1
    );
}

/// V 矩阵第 20 行：cron / lsp 的 `ConfigSource::Builtin { instance }` 在 overlay → 建传输
/// （三分类）→ status 快照 → `DiscoverMCP` 只读投影四跳上都保留，transport 分类 = builtin。
///
/// 判定逻辑全部走既有真实函数：`apply_builtin_overlay`（注入点）、
/// `TransportConfig::try_from` + `TransportConfig::kind` + `require_known_builtin_instance`
/// （discover / 建传输面）、`McpClientPool::all_server_infos`（status 面，内含私有
/// `transport_type_of`）、`DiscoverMCPTool::invoke("detail")`（只读投影面）——本用例不重写
/// 任何一条判定，只把同一份 overlay 产物喂进这些入口。
///
/// 句柄装配按生产 `run_initialize` 的提交形状（`peer` / `tools` 取自真实往返，`source` 取自
/// 同一份 overlay 产物）：本用例断言的是 `source` 在两条**只读**投影面上的行为。
#[tokio::test]
async fn builtin_source_propagates() {
    let fixture = CrossFixture::new();

    // ① overlay：空用户配置 ⇒ 每个已实现实例一条完整 builtin 条目，身份写进 `source`。
    let mut servers: HashMap<String, McpServerConfig> = HashMap::new();
    apply_builtin_overlay(&mut servers, &BuiltinInjectionPolicy::all())
        .expect("空用户配置必须被 overlay 接受");
    let mut configs: Vec<(&str, McpServerConfig)> = Vec::new();
    for instance in WAVE2_INSTANCES {
        let expected = ConfigSource::Builtin {
            instance: instance.to_string(),
        };
        let config = servers
            .get(instance)
            .cloned()
            .unwrap_or_else(|| panic!("{instance}: overlay 必须注入 builtin 条目"));
        assert_eq!(
            config.source,
            Some(expected.clone()),
            "{instance}: overlay 必须写入实例身份（唯一来源是代码，用户配置无法构造）"
        );
        assert!(
            config.command.is_none() && config.url.is_none(),
            "{instance}: builtin 条目不得携带 command / url（否则会被误判为 stdio/http）"
        );

        // ② 建传输 / discover 面：三分类 = builtin，实例身份原样穿透。
        let transport = TransportConfig::try_from(&config).expect("{instance}: 条目必须能建传输");
        match &transport {
            TransportConfig::Builtin { instance: carried } => {
                assert_eq!(
                    carried, instance,
                    "{instance}: 传输配置必须携带同一实例身份"
                )
            }
            other => panic!("{instance}: 必须是 Builtin 传输，实际 {other:?}"),
        }
        assert_eq!(
            transport.kind(),
            TransportKind::Builtin,
            "{instance}: 三分类必须是 builtin"
        );
        require_known_builtin_instance(instance).expect("实例必须能在注册表解析");
        configs.push((instance, config));
    }

    // ③ status 面：同一份配置进 pool 后，快照行（config-only 与已连接两种来源）都报 builtin。
    let pool = Arc::new(McpClientPool::new_empty());
    for (instance, config) in &configs {
        pool.configs
            .write()
            .insert((*instance).to_string(), config.clone());
    }
    let assert_row = |row: &crate::mcp::ServerInfo, instance: &str| {
        assert_eq!(
            row.transport_type, "builtin",
            "{instance}: status 快照的 transport 分类必须是 builtin"
        );
        assert_eq!(
            row.source,
            Some(ConfigSource::Builtin {
                instance: instance.to_string()
            }),
            "{instance}: status 快照必须保留 builtin 身份"
        );
    };
    let config_only = pool.all_server_infos();
    for instance in WAVE2_INSTANCES {
        let row = config_only
            .iter()
            .find(|row| row.name == instance)
            .unwrap_or_else(|| panic!("{instance}: config-only 行必须出现在快照里"));
        assert_row(row, instance);
    }

    // 已连接行：cron / lsp 各走一条**真实**链路（dispatch 工厂 → 真实 transport → 生产 client
    // 握手），再按生产提交形状登记句柄。
    let mut pairs: Vec<(&str, Pair)> = Vec::new();
    for (instance, config) in &configs {
        let pair = connect_via_dispatch(instance, &fixture.ctx).await;
        let tools = pair
            .peer()
            .list_all_tools()
            .await
            .expect("真实 tools/list 必须成功");
        assert!(
            !tools.is_empty(),
            "{instance}: 已连接句柄必须带真实工具快照"
        );
        pool.clients.write().insert(
            (*instance).to_string(),
            Arc::new(McpClientHandle {
                name: (*instance).to_string(),
                version: None,
                cache_version: None,
                peer: Some(pair.peer()),
                tools,
                resources: vec![],
                status: ClientStatus::Connected,
                oauth_status: OAuthStatus::default(),
                source: config.source.clone(),
                url: None,
                skills_capable: false,
                channel_capable: false,
            }),
        );
        pairs.push((instance, pair));
    }
    let all_rows = pool.all_server_infos();
    for instance in WAVE2_INSTANCES {
        let row = all_rows
            .iter()
            .find(|row| row.name == instance)
            .unwrap_or_else(|| panic!("{instance}: 已连接行必须出现在快照里"));
        assert_row(row, instance);
        assert_eq!(row.status, ClientStatus::Connected);
    }

    // ④ discover 面（只读投影）：`DiscoverMCP.detail` 的 `source` 字段报 builtin。
    let discover = DiscoverMCPTool::new(Arc::clone(&pool), None);
    for instance in WAVE2_INSTANCES {
        let raw = discover
            .invoke(
                json!({ "method": "detail", "params": { "server": instance } }),
                ToolContext::new(&[], "."),
            )
            .await
            .expect("DiscoverMCP 的错误也走 Ok（JSON-RPC 错误对象），不得 Err");
        let detail: Value = serde_json::from_str(&raw).expect("detail 必须回 JSON 对象");
        assert_eq!(
            detail["source"], "builtin",
            "{instance}: discover 详情必须报 builtin 来源：{detail}"
        );
        assert_eq!(detail["status"], "connected");
    }

    for (instance, pair) in pairs {
        pair.shutdown().await;
        println!("[W2 dispatch/source] instance={instance} source=builtin transport=builtin");
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 22/23 行：IF-D14 结果映射（真 handler + effective 桥 + 固定脱敏文本）
// ══════════════════════════════════════════════════════════════════════════════

/// 非法 cron 表达式（1 段，解析必然失败）：逼出 `CronError::InvalidExpression`。
const INVALID_EXPRESSION: &str = "not-a-cron-expression";
/// 合法 cron 表达式（填充任务上限用）。
const VALID_EXPRESSION: &str = "*/5 * * * *";
/// 用户 prompt 的可辨认形状串：任何失败文本里**不得**出现。
const LEAK_PROMPT: &str = "SENTINEL-PROMPT-must-not-leak";
/// 路径形状串（§9 规则 7：路径不入模型面文本）。
const LEAK_PATH_MARKER: &str = "secret-marker";
/// 无路由扩展名（不得回传给模型）。
const LEAK_EXTENSION: &str = "zzz";

/// `serve_client_auto` 侧握手上界（同进程链路）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// perl 编写的极简 LSP 服务器（与 `lsp_test.rs` 同源，按本文件需要重抄一份：那个常量是
/// 对方文件的私有项，无跨 owner 复用面）：
/// - 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行（本文件据此证明「服务器真的起来了」）；
/// - 对任何带 id 的请求回 `{"result":null}`（满足 initialize 握手；也用来逼出
///   `LspToolError::RequestFailed`——`incomingCalls` 要把 `null` 反序列化成调用层级数组）。
const FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 交叉面夹具：一份齐备的实例上下文（cron 真实 scheduler + lsp 真实惰性 pool）+ 观察端。
///
/// `web` / `artifact` 不需要额外输入（artifact 的解析根是 `ctx.cwd`），因此同一份上下文可
/// 喂给四个已实现实例的工厂。lsp pool 路由 `.rs` → perl fake server；`LspServerPool::new`
/// 惰性，不调用 LSP 工具时**不拉任何进程**。
struct CrossFixture {
    ctx: BuiltinInstanceContext,
    scheduler: Arc<Mutex<CronScheduler>>,
    pool: Arc<LspServerPool>,
    /// cron 触发通道接收端（保持存活，使发送端始终有对端；本文件不驱动 tick）。
    _cron_triggers: mpsc::UnboundedReceiver<CronTrigger>,
    dir: tempfile::TempDir,
}

impl CrossFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (trigger_tx, cron_triggers) = mpsc::unbounded_channel();
        let scheduler = Arc::new(Mutex::new(CronScheduler::new(trigger_tx)));
        let cwd = dir.path().to_string_lossy().to_string();
        let mut env = HashMap::new();
        env.insert(
            "PERI_LSP_TEST_COUNT".to_string(),
            dir.path()
                .join("spawn_count.txt")
                .to_string_lossy()
                .into_owned(),
        );
        let pool = Arc::new(LspServerPool::new(
            &cwd,
            LspConfigFile {
                lsp_servers: HashMap::from([(
                    "fixture-lsp".to_string(),
                    LspServerConfig {
                        name: "fixture-lsp".to_string(),
                        command: "perl".to_string(),
                        args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
                        env: Some(env),
                        extension_to_language: HashMap::from([(
                            ".rs".to_string(),
                            "rust".to_string(),
                        )]),
                        initialization_options: None,
                        disabled: None,
                        max_restarts: Some(3),
                        startup_timeout: None,
                        source: None,
                    },
                )]),
            },
        ));
        let ctx = BuiltinInstanceContext::new(cwd)
            .with_cron(CronInstanceInput {
                scheduler: Arc::clone(&scheduler),
                tick_enabled: false,
            })
            .with_lsp(LspInstanceInput {
                pool: Arc::clone(&pool),
            });
        Self {
            ctx,
            scheduler,
            pool,
            _cron_triggers: cron_triggers,
            dir,
        }
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// 夹具目录里写一个真实文件，返回绝对路径（LSP 成功路径 / 无路由扩展名路径用）。
    fn write_file(&self, rel: &str, body: &str) -> String {
        let path = self.dir().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("夹具目录可创建");
        }
        std::fs::write(&path, body).expect("夹具文件可写");
        path.to_string_lossy().to_string()
    }
}

/// 一条已握手的 builtin 链路：client service + 本代关闭所有权。
struct Pair {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：释放 client（server 侧读到 EOF）→ 本代监督者按冻结顺序有界收敛。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let outcome = self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        assert!(
            matches!(outcome.tick, TickCloseOutcome::NotSpawned),
            "夹具装配恒不挂 tick（A32：tick 归 pool 的唯一 spawn 点）"
        );
        assert!(
            matches!(outcome.server, BuiltinServerExit::Quit(_)),
            "server task 必须在有界等待内正常收敛，实际：{:?}",
            outcome.server
        );
    }
}

/// 经 **dispatch 工厂**取 handler（`None` = 未接线 / 输入缺失 ⇒ 直接断言失败），装配真实
/// 同进程链路并由生产 `serve_client_auto` 握手——实例名与注册表一致。
async fn connect_via_dispatch(instance: &str, ctx: &BuiltinInstanceContext) -> Pair {
    let handler = builtin_server_handler(instance, ctx)
        .unwrap_or_else(|| panic!("{instance}: 工厂必须给出 handler（本夹具输入齐备）"));
    let transport = spawn_builtin_transport_with_handler(instance, handler);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(
        io,
        None,
        None,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await
    .expect("builtin 握手不得超时（同进程链路）")
    .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
    }
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块。
fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// 断言响应是 `Complete` 并取出结果（IF-D14 的失败形态**不是** `Err`）。
fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

/// handler 路径的**业务失败**文本：协议必须成功（`is_error` 承载语义），且必须有文本块。
async fn call_error_text(pair: &Pair, tool: &str, input: Value) -> String {
    let response = pair
        .peer()
        .call_tool_once(call(tool, input))
        .await
        .unwrap_or_else(|error| panic!("{tool}: 业务失败仍走协议成功（IF-D14），实际：{error:?}"));
    let result = complete(response);
    assert_eq!(
        result.is_error,
        Some(true),
        "{tool}: 业务失败必须是 error 结果"
    );
    first_text(&result).unwrap_or_else(|| panic!("{tool}: error 结果必须有文本块"))
}

/// 只为派生 IF-D14 固定文本的替身工具：`invoke` 恒失败，错误原文可辨认（不得出现在模型面）。
struct FailingStubTool {
    name: String,
}

#[async_trait]
impl BaseTool for FailingStubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "派生 IF-D14 固定文本的替身工具（不触网、不读实例状态）"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("STUB-DETAIL-must-not-leak: raw failure of {}", self.name).into())
    }
}

/// IF-D14 失败形态的**固定文本**：由唯一共享助手 [`invoke_tool_call`] 对**同名**替身工具
/// 派生——本文件不复制映射逻辑、不硬编码规则字面量（规则文本只由工具名决定）。
async fn shared_failure_text(tool_name: &str) -> String {
    let stub: Vec<Arc<dyn BaseTool>> = vec![Arc::new(FailingStubTool {
        name: tool_name.to_string(),
    })];
    let response = invoke_tool_call(&stub, "", &call(tool_name, json!({})))
        .await
        .expect("工具级失败必须是 Ok(Complete(error 结果))，不是 Err");
    let result = complete(response);
    assert_eq!(
        result.is_error,
        Some(true),
        "共享助手对工具级失败必须产出 error 结果"
    );
    let text = first_text(&result).expect("error 结果必须有文本块");
    assert!(
        !text.contains("STUB-DETAIL-must-not-leak"),
        "固定文本本身不得含替身工具的原文：{text}"
    );
    text
}

/// 把命中文本的泄漏标记追加进 `leaks`（计数口径：「泄露计数 == 0」）。
fn collect_leaks<'a>(
    text: &str,
    markers: impl IntoIterator<Item = &'a str>,
    leaks: &mut Vec<String>,
) {
    for marker in markers {
        if !marker.is_empty() && text.contains(marker) {
            leaks.push(marker.to_string());
        }
    }
}

/// V 矩阵第 22 行：真 handler（经 dispatch 工厂 + 真实 wire）的成功 / 业务 Err / 未知工具
/// 三形态，与唯一共享助手 `web::invoke_tool_call` 的输出逐字一致；effective 名字（模型面）
/// 经生产 `McpToolBridge` 打回同一条真实链路。
///
/// 判据来源：固定文本由共享助手对**同名**替身工具派生（不硬编码规则字面量），未知工具协议
/// 错误与共享助手的 `invalid_params` 逐字比较——若 handler 另写了一份映射，任一条立刻红。
#[tokio::test]
async fn call_tool_uses_shared_result_mapping() {
    let fixture = CrossFixture::new();
    let src = fixture.write_file("main.rs", "fn main() {}\n");
    let cases = [
        Wave2CallCase {
            instance: "cron",
            success_tool: "cron_list",
            success_input: json!({}),
            fail_tool: "cron_register",
            fail_input: json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
        },
        Wave2CallCase {
            instance: "lsp",
            success_tool: "LSP",
            success_input: json!({ "operation": "documentSymbol", "file_path": src }),
            fail_tool: "LSP",
            // 缺 `file_path` ⇒ `LspToolError::MissingParam`：不依赖任何 language server 进程。
            fail_input: json!({ "operation": "documentSymbol" }),
        },
    ];

    for case in cases {
        // 注册表前置：两个工具名必须是本实例声明的原始名（名字不凭空写）。
        let declared =
            find(case.instance).unwrap_or_else(|| panic!("{}: 实例必须在注册表内", case.instance));
        for name in [case.success_tool, case.fail_tool] {
            assert!(
                declared.tools.iter().any(|tool| tool.original_name == name),
                "{}/{name}: 必须是注册表声明的原始工具名",
                case.instance
            );
        }
        let effective_of = |name: &str| {
            declared
                .tools
                .iter()
                .find(|tool| tool.original_name == name)
                .unwrap_or_else(|| panic!("{}: 注册表必须声明 {name}", case.instance))
                .effective_name
        };

        // 真实链路：dispatch 工厂 → 真实 transport → 生产 client 握手。
        let pair = connect_via_dispatch(case.instance, &fixture.ctx).await;
        let peer = pair.peer();
        let expected_failure = shared_failure_text(case.fail_tool).await;

        // ① 成功形态：`Complete(success 结果)`，文本非空。
        let success = complete(
            peer.call_tool_once(call(case.success_tool, case.success_input.clone()))
                .await
                .expect("成功路径必须是协议成功"),
        );
        assert_eq!(
            success.is_error,
            Some(false),
            "{}: 成功形态必须是 success 结果",
            case.instance
        );
        let success_text = first_text(&success).expect("成功结果必须有文本块");
        assert!(
            !success_text.is_empty(),
            "{}: 成功文本不得为空",
            case.instance
        );

        // ② 业务 Err 形态：`Complete(error 结果)` + 固定脱敏文本。
        let failure = complete(
            peer.call_tool_once(call(case.fail_tool, case.fail_input.clone()))
                .await
                .expect("业务失败仍走协议成功（is_error 承载语义）"),
        );
        assert_eq!(
            failure.is_error,
            Some(true),
            "{}: 业务失败必须是 error 结果，不是 Err",
            case.instance
        );
        let failure_text = first_text(&failure).expect("error 结果必须有文本块");
        assert_eq!(
            failure_text, expected_failure,
            "{}: 失败文本必须等于共享助手的固定文本",
            case.instance
        );
        assert!(
            failure_text.contains(case.fail_tool),
            "固定文本只带工具名：{failure_text}"
        );

        // ③ 未知工具形态：invalid params，且与共享助手逐字同源（不 panic）。
        let unknown = "wave2-unknown-tool";
        let helper_error = invoke_tool_call(&[], "", &call(unknown, json!({})))
            .await
            .expect_err("共享助手的未知工具分支必须是协议错误");
        let wire_error = peer
            .call_tool_once(call(unknown, json!({})))
            .await
            .expect_err("线上未知工具名必须是协议错误");
        match wire_error {
            ServiceError::McpError(data) => {
                assert_eq!(
                    data.code.0,
                    ErrorCode::INVALID_PARAMS.0,
                    "{}: 未知工具必须是 -32602",
                    case.instance
                );
                assert_eq!(
                    data.message, helper_error.message,
                    "{}: 未知工具错误必须与共享助手逐字一致",
                    case.instance
                );
            }
            other => panic!(
                "{}: 期望 McpError(invalid_params)，实际 {other:?}",
                case.instance
            ),
        }

        // ④ effective 调用桥接：模型面名字（注册表冻结的 effective name）经生产
        //    `build_deferred_tool_bridges` 打回**同一条**真实链路的 handler。
        let pool = bridge_pool(case.instance, &peer).await;
        let bridges = build_deferred_tool_bridges(&pool);
        let success_bridge = bridge_of(&bridges, effective_of(case.success_tool));
        let bridged_success = success_bridge
            .invoke(
                case.success_input.clone(),
                ToolContext::new(&[], &fixture.ctx.cwd),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{}: effective 桥的成功路径不得失败：{error}", case.instance)
            });
        assert_eq!(
            bridged_success, success_text,
            "{}: effective 桥的成功文本必须与 handler 路径逐字一致",
            case.instance
        );

        let failure_bridge = bridge_of(&bridges, effective_of(case.fail_tool));
        let bridged_error = failure_bridge
            .invoke(
                case.fail_input.clone(),
                ToolContext::new(&[], &fixture.ctx.cwd),
            )
            .await
            .expect_err("工具级失败经桥必须 Err（桥把 error 结果转成 CallFailed）");
        match bridged_error.downcast_ref::<ToolCallError>() {
            Some(ToolCallError::CallFailed { reason, tool, .. }) => {
                assert_eq!(
                    reason.as_str(),
                    expected_failure.as_str(),
                    "{}: 桥接失败原因必须是同一份固定脱敏文本",
                    case.instance
                );
                assert_eq!(tool, case.fail_tool);
            }
            other => panic!(
                "{}: 期望 CallFailed（reason = 固定文本），实际 {other:?}",
                case.instance
            ),
        }

        println!(
            "[W2 dispatch/source] instance={} success=1 error=1 invalid_params=1 bridged=2",
            case.instance
        );
        pair.shutdown().await;
    }
}

/// 一个 wave 2 实例的三形态用例输入（工具名必须能在注册表里查到）。
struct Wave2CallCase {
    instance: &'static str,
    success_tool: &'static str,
    success_input: Value,
    fail_tool: &'static str,
    fail_input: Value,
}

/// 把一条**已握手**的真实 builtin 链路登记成 pool 里的 Connected 句柄（形状同生产
/// `run_initialize` 的提交点：`peer` / `tools` 取自真实往返），再经生产
/// `build_deferred_tool_bridges` 取 effective name 的 `McpToolBridge`。
///
/// 句柄 `name` 必须是实例名：bridge 的 effective name 由 `(server_name, 原始工具名)` 计算
/// （`McpToolBridge::new` 用 `client.name`），因此这一步同时验证注册表冻结字面量与生产计算
/// 一致。
async fn bridge_pool(instance: &str, peer: &Peer<RoleClient>) -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    let tools = peer
        .list_all_tools()
        .await
        .expect("真实 tools/list 必须成功");
    pool.clients.write().insert(
        instance.to_string(),
        Arc::new(McpClientHandle {
            name: instance.to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        }),
    );
    pool
}

/// 按注册表冻结的 effective name 取真实桥（缺桥 ⇒ 断言失败）。
fn bridge_of<'a>(bridges: &'a [McpToolBridge], effective_name: &str) -> &'a McpToolBridge {
    bridges
        .iter()
        .find(|bridge| bridge.name() == effective_name)
        .unwrap_or_else(|| {
            panic!(
                "{effective_name}: 真实桥必须存在（tools/list 快照 + 注册表声明）；实际：{:?}",
                bridges
                    .iter()
                    .map(|bridge| bridge.name())
                    .collect::<Vec<_>>()
            )
        })
}

/// V 矩阵第 23 行：cron 两类 + LSP 六类业务错误**经 handler 路径**返回的文本全部等于固定
/// 脱敏文本（只含工具名），且不含原始错误 Display、表达式原文、用户 prompt、文件路径 / 扩展名。
///
/// 触发方式：每类先用**工具实现层**（同一 pool / 同一 scheduler）跑一次同一输入，取其错误
/// Display 作为「确实落到该变体」的正控与泄漏对照，再断言 handler 路径（真实 wire）的文本。
///
/// **UNVERIFIED（不写 flaky 断言）**：`LspToolError::NotReady` 需要
/// `ensure_server_for_file` / `ensure_initialized` 返回 `Ok` 而没有任何 server `Running`；
/// 而 `LspClient::start` 成功即以 `ServerState::Running` 收尾（`peri-lsp/src/client/lifecycle.rs`
/// 的 `do_start`），`is_ready()` 与 `any_server()` 用同一谓词，唯一剩余路径是「ensure 与
/// ready 复查之间服务器转 `Error`」的竞态，无法由输入稳定构造，因此本用例只断言可稳定
/// 触发的 7 类（2 + 5），`NotReady` 记 UNVERIFIED。
#[tokio::test]
async fn call_tool_error_text_is_fixed_and_redacted() {
    let fixture = CrossFixture::new();
    let src = fixture.write_file("main.rs", "fn main() {}\n");
    // 无路由扩展名路径：目录名与扩展名都是可辨认的泄漏标记。
    let no_route = fixture.write_file(
        &format!("{LEAK_PATH_MARKER}/private.{LEAK_EXTENSION}"),
        "no server routes this extension\n",
    );

    let mut matched = 0usize;
    let mut leaks: Vec<String> = Vec::new();

    // ─── cron：两类业务错误 ───────────────────────────────────────────────────
    let cron = connect_via_dispatch("cron", &fixture.ctx).await;
    let expected_cron = shared_failure_text("cron_register").await;
    // 正控 ①：非法表达式在**同一 scheduler** 的工具实现层失败（原文即泄漏对照）。
    let raw_invalid = fixture
        .scheduler
        .lock()
        .register(INVALID_EXPRESSION, LEAK_PROMPT)
        .expect_err("非法表达式必须在 CronScheduler::register 失败")
        .to_string();
    // 正控 ②：先填到上限（次数读 `MAX_CRON_TASKS`，不硬编码 20）再注册。
    while fixture.scheduler.lock().list_tasks().len() < MAX_CRON_TASKS {
        fixture
            .scheduler
            .lock()
            .register(VALID_EXPRESSION, LEAK_PROMPT)
            .expect("上限内的注册必须成功");
    }
    let raw_limit = fixture
        .scheduler
        .lock()
        .register(VALID_EXPRESSION, LEAK_PROMPT)
        .expect_err("达到上限后注册必须失败")
        .to_string();
    assert_ne!(raw_invalid, raw_limit, "两个变体的原文必须可区分");
    let limit_text = MAX_CRON_TASKS.to_string();

    for input in [
        json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
        json!({ "expression": VALID_EXPRESSION, "prompt": LEAK_PROMPT }),
    ] {
        matched += 1;
        let text = call_error_text(&cron, "cron_register", input).await;
        assert_eq!(
            text, expected_cron,
            "cron 的业务错误文本必须等于固定脱敏文本（与失败原因无关）"
        );
        collect_leaks(
            &text,
            [
                raw_invalid.as_str(),
                raw_limit.as_str(),
                limit_text.as_str(),
                INVALID_EXPRESSION,
                LEAK_PROMPT,
            ],
            &mut leaks,
        );
    }
    cron.shutdown().await;

    // ─── lsp：六类中的五类（NotReady 见用例文档：无法稳定构造） ────────────────
    let lsp = connect_via_dispatch("lsp", &fixture.ctx).await;
    let expected_lsp = shared_failure_text("LSP").await;
    let tool = LspTool::new(Arc::clone(&fixture.pool));
    let cases: [(&str, Value); 5] = [
        // MissingParam：缺 `file_path`（不进 pool）。
        ("MissingParam", json!({ "operation": "documentSymbol" })),
        // InvalidOperation：服务器就绪后落到 `_` 分支。
        (
            "InvalidOperation",
            json!({ "operation": "wave2-no-such-operation", "file_path": src, "line": 1, "character": 1 }),
        ),
        // RequestFailed：prepareCallHierarchy 回 `null` ⇒ 反序列化调用层级数组失败。
        (
            "RequestFailed",
            json!({ "operation": "incomingCalls", "file_path": src, "line": 1, "character": 1 }),
        ),
        // InvalidPosition：`line == 0`（在任何服务器访问之前返回）。
        (
            "InvalidPosition",
            json!({ "operation": "goToDefinition", "file_path": src, "line": 0, "character": 1 }),
        ),
        // NoServerForExtension：扩展名无路由（原文内嵌路径与扩展名）。
        (
            "NoServerForExtension",
            json!({ "operation": "documentSymbol", "file_path": no_route }),
        ),
    ];
    let mut raws: Vec<String> = Vec::new();
    for (variant, input) in &cases {
        // 正控：同一输入必须在工具实现层落到该变体（否则本行断言是空转）。
        let raw = tool
            .invoke(input.clone(), ToolContext::new(&[], ""))
            .await
            .unwrap_err()
            .to_string();
        assert!(!raw.is_empty(), "{variant}: 错误原文不得为空");
        // 变体身份：只允许用**本文件给出的**输入事实或可独立复算的事实证伪「其实落到别的
        // 变体」，不硬编码生产 Display 字面量。
        match *variant {
            "MissingParam" => assert!(raw.contains("file_path"), "缺参变体必须回传缺失的参数名"),
            "InvalidOperation" => assert!(
                raw.contains("wave2-no-such-operation"),
                "无效 operation 变体必须回传被拒的 operation"
            ),
            "NoServerForExtension" => assert!(
                raw.contains(LEAK_PATH_MARKER) && raw.contains(LEAK_EXTENSION),
                "无路由扩展名变体的原文必须内嵌路径与扩展名（这正是它的泄漏面）"
            ),
            "RequestFailed" => {
                // 本行的构造是「请求返回 `null` ⇒ 反序列化调用层级数组失败」：serde 的错误
                // 文本可由本文件对同一 `null` 载荷独立复算（不引用生产字面量）。
                let recomputed = serde_json::from_value::<Vec<Value>>(Value::Null)
                    .expect_err("null 不得反序列化成数组")
                    .to_string();
                assert!(
                    raw.contains(&recomputed),
                    "RequestFailed 行必须是 null 载荷反序列化失败：{raw}"
                );
            }
            "InvalidPosition" => {}
            other => panic!("未登记的变体 {other}"),
        }
        raws.push(format!("{variant}: {raw}"));

        matched += 1;
        let text = call_error_text(&lsp, "LSP", input.clone()).await;
        assert_eq!(
            text, expected_lsp,
            "{variant}: LSP 的错误文本必须等于固定脱敏文本"
        );
        collect_leaks(
            &text,
            [
                raw.as_str(),
                src.as_str(),
                no_route.as_str(),
                LEAK_PATH_MARKER,
                LEAK_EXTENSION,
                LEAK_PROMPT,
            ],
            &mut leaks,
        );
    }
    // 正控有效性：五个变体的原文必须两两不同（否则「逐类」是同一类的重复计数）。
    let mut distinct_raws = raws.clone();
    distinct_raws.sort();
    distinct_raws.dedup();
    assert_eq!(
        distinct_raws.len(),
        cases.len(),
        "五个 LSP 变体必须各自触发不同的错误原文：{raws:#?}"
    );
    // InvalidOperation / RequestFailed 两行必须在**就绪**的 language server 上打到（fake
    // server 的 spawn 观测文件非空）；配合 `call_tool_uses_shared_result_mapping` 的成功行，
    // 「就绪」这一前提有独立证据。
    let spawns = std::fs::read_to_string(fixture.dir().join("spawn_count.txt"))
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        spawns >= 1,
        "InvalidOperation / RequestFailed 必须有真实拉起的 language server"
    );
    lsp.shutdown().await;

    // 固定文本只由工具名决定：两条期望的差异必须恰是工具名槽位。
    assert_eq!(
        expected_lsp.replace("`LSP`", "`cron_register`"),
        expected_cron,
        "cron / lsp 的固定文本必须只差工具名槽位"
    );
    // 夹具前置：无路由扩展名文件与真实源文件都存在（否则 NoServerForExtension 会变体漂移）。
    assert!(Path::new(&no_route).exists() && Path::new(&src).exists());

    // 先 assert 再打印（`--nocapture` 抄录用）：matched = 2（cron）+ 5（LSP 可稳定触发类）。
    println!(
        "[W2 dispatch/source] error-variants declared=8 matched={matched} unverified=NotReady leaks={}",
        leaks.len()
    );
    assert_eq!(matched, 7, "8 类中 7 类可在 handler 路径稳定触发");
    assert_eq!(leaks.len(), 0, "业务细节不得进入模型面文本：{leaks:?}");
}
