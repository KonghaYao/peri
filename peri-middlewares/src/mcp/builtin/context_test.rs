//! `mcp::builtin::context` 的 crate 内证据（owner H-02）。
//!
//! 覆盖口径（主 plan §3 IF-P3-04 / A31① / A33）：
//! - **构造保真**：便利构造器不复制、不重排、不改写输入——`scheduler` 仍是注入
//!   时**同一份** `Arc`（`Arc::ptr_eq`），`cwd` / `tick_enabled` / 关闭集逐字保真；
//!   关闭集用 A24 的真实派生入口 `mcp::builtin::closed_instances` 生成（不硬编码第三份
//!   映射）。
//! - **输入齐备判定的真值表**：判定按注册表名派生——`web` / `artifact` 不需要额外输入；
//!   `cron` 由对应输入的有无决定；`workspace` **恒**齐备（AW3-11：它的输入是
//!   session 级，缺输入 = 可见但退化而非不可装配，判定不为它增加 arm）；未注册名字
//!   （表外名）一律不齐备（实例解析由 `runtime` 先行收口 `UnknownInstance`）。

use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use parking_lot::Mutex;
use peri_acp_types::{
    event::BackgroundTaskResult,
    tasks::{BgTaskKind, TaskManager},
};
use peri_agent::agent::async_tasks::TaskManager as ConcreteTaskManager;

use super::{BuiltinInstanceContext, CronInstanceInput};
use crate::mcp::builtin::closed_instances;
use peri_mcp_cron::{CronScheduler, CronTrigger};
use peri_mcp_workspace::WorkspaceInstanceInput;

/// `cron` 输入夹具：真实 scheduler（`unbounded_channel` 不需要 runtime；receiver 直接
/// 丢弃——本文件不驱动 tick）。
fn cron_input(tick_enabled: bool) -> CronInstanceInput {
    let (trigger_tx, _trigger_rx) = tokio::sync::mpsc::unbounded_channel::<CronTrigger>();
    CronInstanceInput {
        scheduler: Arc::new(Mutex::new(CronScheduler::new(trigger_tx))),
        tick_enabled,
    }
}

/// `workspace` 输入夹具（AW3-11）：**真实** per-session `TaskManager` + session 级回调。
///
/// 不注入替身：本文件断言的是「输入原样保真」（`Arc::ptr_eq`），替身会让该断言恒真。
fn workspace_input() -> WorkspaceInstanceInput {
    WorkspaceInstanceInput {
        task_manager: Some(Arc::new(ConcreteTaskManager::new()) as Arc<dyn TaskManager>),
        on_bg_complete: Some(Arc::new(|_: &BackgroundTaskResult, _: BgTaskKind| {})),
    }
}

/// ① 三个便利构造器 + 关闭集必须原样保真：不复制共享状态、不做名字归一、不重排集合。
#[test]
fn constructors_keep_inputs_and_closed_set_verbatim() {
    // 最小构造：无实例输入、关闭集为空。
    let bare = BuiltinInstanceContext::new("ctx-cwd");
    assert_eq!(bare.cwd, "ctx-cwd");
    assert!(bare.cron.is_none(), "最小构造不得携带 cron 输入");
    assert!(
        bare.workspace.is_none(),
        "最小构造不得携带 workspace 输入（缺输入 = 可见但退化，不是默认注入）"
    );
    assert!(bare.closed.is_empty(), "最小构造的关闭集必须为空");

    let cron = cron_input(true);
    let workspace = workspace_input();
    let scheduler = Arc::clone(&cron.scheduler);
    let task_manager = Arc::clone(workspace.task_manager.as_ref().expect("夹具必须带 manager"));
    let on_bg_complete = Arc::clone(
        workspace
            .on_bg_complete
            .as_ref()
            .expect("夹具必须带 bg 完成回调"),
    );
    // 关闭集用 A24 唯一派生入口生成：`policy_key ∈ disabled_middlewares` → 实例名。
    let closed = closed_instances(&HashSet::from(["CronMiddleware".to_string()]));
    assert_eq!(
        closed,
        BTreeSet::from(["cron".to_string()]),
        "前置：关闭集必须由 policy_key 派生实例名"
    );

    let ctx = BuiltinInstanceContext::new("ctx-cwd")
        .with_cron(cron)
        .with_workspace(workspace)
        .with_closed(closed.clone());

    assert_eq!(ctx.cwd, "ctx-cwd", "cwd 必须逐字保真");
    let ctx_cron = ctx.cron.expect("cron 输入必须保真");
    assert!(
        Arc::ptr_eq(&ctx_cron.scheduler, &scheduler),
        "必须是组合根那一份 scheduler（不复制、不另建）"
    );
    assert!(ctx_cron.tick_enabled, "tick_enabled 必须逐字保真");
    let ctx_workspace = ctx.workspace.expect("workspace 输入必须保真");
    assert!(
        Arc::ptr_eq(
            ctx_workspace
                .task_manager
                .as_ref()
                .expect("manager 必须保真"),
            &task_manager
        ),
        "必须是 session 那一个 TaskManager（AW3-11：同一份 Arc，不复制、不另建）"
    );
    assert!(
        Arc::ptr_eq(
            ctx_workspace.on_bg_complete.as_ref().expect("回调必须保真"),
            &on_bg_complete
        ),
        "必须是装配面那一个 bg 完成回调（同一份 Arc）"
    );
    assert_eq!(ctx.closed, closed, "关闭集必须原样保真（不增删、不重排）");

    // `tick_enabled = false` 同样保真（print / stdio 路径的投影不得被改写为 true）。
    let no_tick = BuiltinInstanceContext::new("ctx-cwd").with_cron(cron_input(false));
    assert!(
        !no_tick.cron.expect("cron 输入必须保真").tick_enabled,
        "tick_enabled=false 必须逐字保真"
    );
}

/// ② `instance_input_ready` 真值表：按注册表名派生，不硬编码第二张实例名字表。
///
/// `workspace` 有**两行**（有无输入都必须齐备）：AW3-11 冻结「`None` = 可见但退化」，
/// 把它判成不齐备会让 `runtime` 在装配前就拒掉一个本可用的实例。
#[test]
fn instance_input_ready_truth_table() {
    let bare = BuiltinInstanceContext::new("ctx-cwd");
    assert!(bare.instance_input_ready("web"), "web 不需要额外输入");
    assert!(
        bare.instance_input_ready("artifact"),
        "artifact 不需要额外输入（解析根是 cwd）"
    );
    assert!(!bare.instance_input_ready("cron"), "缺 scheduler ⇒ 不齐备");
    assert!(
        bare.instance_input_ready("workspace"),
        "workspace 无 session 级输入也必须齐备（AW3-11：缺输入 = 可见但退化，不是不可装配）"
    );
    assert!(
        !bare.instance_input_ready("not-a-builtin"),
        "表外名字不得判为齐备"
    );

    let cron_only = BuiltinInstanceContext::new("ctx-cwd").with_cron(cron_input(false));
    assert!(cron_only.instance_input_ready("cron"));
    assert!(cron_only.instance_input_ready("web"));
    assert!(cron_only.instance_input_ready("artifact"));
    assert!(cron_only.instance_input_ready("workspace"));

    // 带齐 workspace 输入（session 级）同样齐备：判定对它恒真，输入的有无不改变结果。
    let workspace_only = BuiltinInstanceContext::new("ctx-cwd").with_workspace(workspace_input());
    assert!(workspace_only.instance_input_ready("workspace"));
    assert!(!workspace_only.instance_input_ready("cron"));

    let both = BuiltinInstanceContext::new("ctx-cwd")
        .with_cron(cron_input(true))
        .with_workspace(workspace_input())
        .with_closed(BTreeSet::from(["web".to_string()]));
    assert!(both.instance_input_ready("cron"));
    assert!(both.instance_input_ready("workspace"));
    assert!(
        both.instance_input_ready("web"),
        "关闭集只是投影面事实，不影响输入齐备判定（非物理关闭）"
    );
}
