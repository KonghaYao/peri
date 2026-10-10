//! Cron runtime lifecycle tests that exercise the host-owned tick supervisor.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use parking_lot::Mutex;
use peri_mcp_cron::CronScheduler;
use peri_mcp_cron::CronTrigger;
use rmcp::service::QuitReason;
use tokio::sync::mpsc;

use crate::mcp::builtin::runtime::{
    test_supervisor, BuiltinServerExit, TickCloseOutcome, TickGuard, BUILTIN_CONVERGE_TIMEOUT,
};

const REGISTER_EXPRESSION: &str = "*/5 * * * *";
const REGISTER_PROMPT: &str = "cron-gate-test-prompt";
const TICK_PERIOD: Duration = Duration::from_secs(1);
const NO_TICK_OBSERVATION: Duration = Duration::from_millis(2_500);

struct Triggers {
    _primary: mpsc::UnboundedReceiver<CronTrigger>,
    extra: mpsc::UnboundedReceiver<CronTrigger>,
}

fn scheduler_fixture() -> (Arc<Mutex<CronScheduler>>, Triggers) {
    let (primary_tx, primary_rx) = mpsc::unbounded_channel();
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(primary_tx)));
    let extra = scheduler.lock().subscribe();
    (
        scheduler,
        Triggers {
            _primary: primary_rx,
            extra,
        },
    )
}

async fn wait_for_count(ticks: &AtomicUsize, target: usize) {
    tokio::time::timeout(TICK_PERIOD * 2, async {
        while ticks.load(Ordering::SeqCst) < target {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("驱动计数必须在有界等待内到达目标值");
}

/// 立即收敛的 server task：正常关闭形态（`Quit(QuitReason::Closed)`）。
fn quit_server_task() -> tokio::task::JoinHandle<BuiltinServerExit> {
    tokio::spawn(async { BuiltinServerExit::Quit(QuitReason::Closed) })
}

/// 生产 tick 语义的驱动闭包：逐位搬运宿主 tick（`assemble.rs` 的 CronTick 分支，
/// `interval.tick()` → `scheduler.lock().tick()`），额外对**驱动调用次数**计数。
///
/// 计数点是驱动本身而不是 trigger：任务触发后 `next_fire` 会被重算到未来，trigger 条数
/// 无法按 interval 统计（见 `tick_reconnect_has_single_driver_per_interval`）。
fn counting_driver(
    ticks: Arc<AtomicUsize>,
    scheduler: &Arc<Mutex<CronScheduler>>,
) -> impl FnMut() + Send + 'static {
    let driven = Arc::clone(scheduler);
    move || {
        ticks.fetch_add(1, Ordering::SeqCst);
        driven.lock().tick();
    }
}

/// 门禁（A32）：代监督者持有 tick —— 「cancel → 有界 join 完成」，且关闭后本代驱动**不再**
/// 产生任何触发。
///
/// 四段互相咬合，任何一段单独成立都不足以证明：
/// ① 正控：到期任务必须被**这个**驱动跨过一个 interval 触发（驱动真在跑 + 观测通道有效）；
/// ② `shutdown` 必须是 `Joined`，且 task 确实走到终点（`stopped` 已触发）；
/// ③ 反控：已 join 的一代不得再驱动（计数冻结）、到期任务也不得再自行触发；
/// ④ 同一结论由 `BuiltinInstanceSupervisor::close` 承载（tick 侧没有第二条关闭路径）。
#[tokio::test]
async fn tick_shutdown_joins_task_and_stops_triggers() {
    let (scheduler, mut triggers) = scheduler_fixture();
    let task_id = scheduler
        .lock()
        .register(REGISTER_EXPRESSION, REGISTER_PROMPT)
        .expect("夹具注册必须成功");

    let ticks = Arc::new(AtomicUsize::new(0));
    let guard = TickGuard::spawn(TICK_PERIOD, counting_driver(Arc::clone(&ticks), &scheduler));
    // `notify_waiters()` **不存 permit**：观测者必须在 task 退出前注册，否则唤醒会丢。
    let stopped = guard.stopped();
    let stopped_wait = stopped.notified();
    tokio::pin!(stopped_wait);
    assert!(
        !stopped_wait.as_mut().enable(),
        "注册观测者时 tick task 必未退出（否则本用例的终点证据不成立）"
    );

    // ① 正控：先把任务置为到期，再等驱动把它带过来。
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "正控前置：任务必须存在"
    );
    let fired = tokio::time::timeout(TICK_PERIOD * 3, triggers.extra.recv())
        .await
        .expect("正控：跨过一个 interval 后必须收到 CronTrigger（否则本用例空转）")
        .expect("观测通道必须存活");
    assert_eq!(fired.task_id, task_id);
    assert_eq!(fired.prompt, REGISTER_PROMPT);
    assert!(
        ticks.load(Ordering::SeqCst) > 0,
        "触发即证明驱动被真实调用过（不是别的路径送的 trigger）"
    );

    // ② 关闭：有界 join 完成 + task 走到终点。
    let tick_outcome = guard.shutdown(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(tick_outcome, TickCloseOutcome::Joined),
        "tick 必须在有界等待内 join 完成（abort 不是正常路径），实际: {tick_outcome:?}"
    );
    tokio::time::timeout(BUILTIN_CONVERGE_TIMEOUT, stopped_wait)
        .await
        .expect("tick task 必须走到终点：`stopped` 在 task 返回前触发");

    // ③ 反控：已停的一代不再驱动、不再触发。
    while triggers.extra.try_recv().is_ok() {} // 排空 ② 之前的在途 trigger（不属于反控窗口）
    let frozen = ticks.load(Ordering::SeqCst);
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "反控前置：任务必须仍存在"
    );
    assert!(
        NO_TICK_OBSERVATION > 2 * TICK_PERIOD,
        "反控窗口必须 > 2× tick 周期，否则本用例的结论不成立"
    );
    tokio::time::sleep(NO_TICK_OBSERVATION).await;
    assert!(
        triggers.extra.try_recv().is_err(),
        "已 join 的驱动不得再触发：到期任务在无驱动时不得自行产生 CronTrigger"
    );
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        frozen,
        "已 join 的驱动不得再被调用：> 2× tick 周期内计数必须冻结"
    );

    // ④ 同一结论由代监督者承载：`close` 依次给出 tick 侧与 server 侧结论。
    let supervisor = test_supervisor(
        "cron",
        Some(TickGuard::spawn(
            TICK_PERIOD,
            counting_driver(Arc::clone(&ticks), &scheduler),
        )),
        quit_server_task(),
    );
    let close_outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(close_outcome.tick, TickCloseOutcome::Joined),
        "代监督者关闭必须承载 tick 侧结论（不是 NotSpawned/abort），实际: {close_outcome:?}"
    );
    assert!(
        matches!(close_outcome.server, BuiltinServerExit::Quit(_)),
        "server task 必须正常收敛（Quit），实际: {close_outcome:?}"
    );
}

/// A32：同一 scheduler 任一时刻**至多一个** tick 驱动（reconnect 必须先停旧代再建新代）。
///
/// 容差理由：`tokio::time::interval` 的首个 tick 立即完成，把它排除在窗口外后，3 个 interval
/// 的窗口跨过的边界数按调度抖动为 2~3（最多吞掉一个边界）。
/// 证伪逻辑：任一窗口若有两个驱动并存，两个驱动各自 ≈1 次/interval ⇒ 计数 ≈2N（N=3 时
/// ≈6），远超上界 3——上界能拒掉双驱动，不是「宽到什么都测不出」。
#[tokio::test]
async fn tick_reconnect_has_single_driver_per_interval() {
    let (scheduler, _triggers) = scheduler_fixture();
    let ticks = Arc::new(AtomicUsize::new(0));

    // gen1：跨过首个立即 tick 后，统计 3 个完整 interval。
    let gen1 = TickGuard::spawn(TICK_PERIOD, counting_driver(Arc::clone(&ticks), &scheduler));
    wait_for_count(&ticks, 1).await;
    let base1 = ticks.load(Ordering::SeqCst);
    tokio::time::sleep(TICK_PERIOD * 3).await;
    let delta1 = ticks.load(Ordering::SeqCst) - base1;
    assert!(
        (2..=3).contains(&delta1),
        "gen1 在 3 个 interval 内的驱动次数必须 ≈1/interval，实际 {delta1}（双驱动会得到 ≈6）"
    );

    // reconnect 的第一步：旧代必须先有界 join。
    let gen1_outcome = gen1.shutdown(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(gen1_outcome, TickCloseOutcome::Joined),
        "reconnect 前旧代必须 join 完成，实际: {gen1_outcome:?}"
    );
    let c1 = ticks.load(Ordering::SeqCst); // 旧代已 join ⇒ 该值已是终值
    tokio::time::sleep(TICK_PERIOD * 2).await;
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        c1,
        "旧代已 join，计数必须冻结在 c1：睡着还在涨 = 旧代仍活着 = 双驱动"
    );

    // gen2：同一 scheduler 的新一代，增量同样必须 ≈1/interval。
    let gen2 = TickGuard::spawn(TICK_PERIOD, counting_driver(Arc::clone(&ticks), &scheduler));
    wait_for_count(&ticks, c1 + 1).await;
    let base2 = ticks.load(Ordering::SeqCst);
    tokio::time::sleep(TICK_PERIOD * 3).await;
    let delta2 = ticks.load(Ordering::SeqCst) - base2;
    assert!(
        (2..=3).contains(&delta2),
        "gen2 在 3 个 interval 内的驱动次数必须 ≈1/interval，实际 {delta2}：\
         若旧代未停，同一窗口会有两个驱动 ⇒ ≈6"
    );

    let gen2_outcome = gen2.shutdown(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(gen2_outcome, TickCloseOutcome::Joined),
        "gen2 必须有界 join 完成，实际: {gen2_outcome:?}"
    );
}

/// A32：`tick_enabled = false` 的一代不挂 tick 驱动 —— 到期任务静默，但**工具面不受影响**。
///
/// `tick_enabled=false` 的语义就是「本代不 spawn guard」（W2 由 pool 依 `cron.tick_enabled`
/// 决定是否把 `TickGuard` 交给监督者；本任务不做该接线，这里用「不 spawn」直接建模同一语义）。
#[tokio::test]
async fn tick_guard_reports_join_state() {
    // 运行中的一代：`is_finished()` 为 false，`shutdown` 靠有界 join 收敛为 `Joined`。
    let ticks = Arc::new(AtomicUsize::new(0));
    let guard = TickGuard::spawn(TICK_PERIOD, {
        let counter = Arc::clone(&ticks);
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    assert!(!guard.is_finished(), "运行中的 tick task 不得报已结束");
    let outcome = guard.shutdown(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(outcome, TickCloseOutcome::Joined),
        "cancel 后必须靠有界 join 收敛为 Joined，实际: {outcome:?}"
    );

    // 有 tick 且运行中 ⇒ `tick_is_finished()` 为 false；`close` 同时给出两侧结论。
    let supervisor = test_supervisor(
        "cron",
        Some(TickGuard::spawn(TICK_PERIOD, || {})),
        quit_server_task(),
    );
    assert_eq!(supervisor.instance(), "cron");
    assert!(
        !supervisor.tick_is_finished(),
        "有 tick 且运行中 ⇒ false（否则「本代已无运行中的 tick」不可断言）"
    );
    let close_outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(close_outcome.tick, TickCloseOutcome::Joined),
        "有 tick 的一代 close 必须报 Joined，实际: {close_outcome:?}"
    );
    assert!(
        matches!(close_outcome.server, BuiltinServerExit::Quit(_)),
        "server task 必须正常收敛（Quit），实际: {close_outcome:?}"
    );

    // 无 tick 的一代：`tick_is_finished()` 是 true（不是 false），close → `NotSpawned`。
    let without_tick = test_supervisor("web", None, quit_server_task());
    assert!(
        without_tick.tick_is_finished(),
        "无 tick ⇒ 已无运行中的 tick（读成 false 会让非 cron 实例永远过不了关闭检查）"
    );
    let close_outcome = without_tick.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(close_outcome.tick, TickCloseOutcome::NotSpawned),
        "无 tick 不得报 joined/aborted：NotSpawned 是独立结论，实际: {close_outcome:?}"
    );
    assert!(
        matches!(close_outcome.server, BuiltinServerExit::Quit(_)),
        "server task 必须正常收敛（Quit），实际: {close_outcome:?}"
    );
}
