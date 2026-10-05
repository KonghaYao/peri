//! Builtin handler factory and source propagation contracts.

use super::*;

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
    // 工厂只按实例名分派：上下文的 `cwd` 只被 artifact 用作解析根，实例输入（cron）
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
    // 可以没有 handler——cron 的缺输入由 seam 在工厂之前收口 `InstanceInputMissing`，
    // 其余每一个已实现实例（含无输入的 workspace）都必须拿到 handler。`workspace` 若被写成
    // 条件构造，本断言即红。
    let none_with_minimal_ctx: Vec<&str> = BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| instance.name)
        .filter(|name| builtin_server_handler(name, &ctx).is_none())
        .collect();
    assert_eq!(
        none_with_minimal_ctx,
        vec!["cron"],
        "最小上下文里没有 handler 的已实现实例必须恰为「需要实例输入」的 cron（缺输入归 seam 前置判定，\
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

/// H-05 接线闸门：已实现实例各得**自己的**变体（cron ⇒ `Cron`、
/// workspace ⇒ `Workspace`），状态对象是注入的**同一份**（A1：组合根构造、同一份 `Arc`），
/// 且表外名字恒 `None`。
///
/// `workspace` 的具名用例是 AW3-11 的「可见但退化」：本用例的上下文**不带** workspace
/// 输入（`ctx.workspace == None`），工厂仍必须返回 `Workspace` 变体而不是 `None`。
#[test]
fn dispatch_covers_cron_variant() {
    // cron 输入：真实 scheduler（触发通道无人消费即可——本用例不驱动 tick）。
    let (trigger_tx, _trigger_rx) = mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(trigger_tx)));

    let ctx = BuiltinInstanceContext::new(".").with_cron(CronInstanceInput {
        scheduler: Arc::clone(&scheduler),
        tick_enabled: false,
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
                "{}: 已实现实例必须有 handler（上下文已给齐 cron 输入）",
                instance.name
            )
        });
        let variant = match &handler {
            BuiltinServerHandler::Web(_) => "web",
            BuiltinServerHandler::Artifact(_) => "artifact",
            BuiltinServerHandler::Cron(_) => "cron",
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

    // 「保留但未实现」的名字（从保留名表派生）：**当前为空**——保留名全部已实现
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

/// V 矩阵第 20 行：cron 的 `ConfigSource::Builtin { instance }` 在 overlay → 建传输
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

    // 已连接行：cron 走一条**真实**链路（dispatch 工厂 → 真实 transport → 生产 client
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
