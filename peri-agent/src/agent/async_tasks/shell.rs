use std::sync::Arc;

use super::registry::BackgroundTaskRegistry;
use crate::agent::events::BackgroundTaskResult;
use peri_acp_types::tasks::BgTaskKind;

pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

// ── bg shell 执行链 ──────────────────────────────────────────────────────────

/// 生成 bg shell 任务 id：`shell-{完整 UUID v7}`。
///
/// **禁止截断 UUID**（issue 2026-08-05）：UUID v7 前 48 位是毫秒时间戳，
/// 同一毫秒内生成的前 8 字符必然相同。agent 连续多次 `run_in_background`
/// Bash 调用落在同一毫秒时，截断前缀会导致 task_id 碰撞——registry 覆盖
/// 注册（Started 事件重复、cancel 句柄丢失），且首个 `complete()` 的 retain
/// 清理后其余 `complete()` 因 existed=false 静默跳过，TUI 残留任务条目。
/// 与 bg agent（`bg-{完整 UUID}`）保持一致，用完整 UUID（122 位熵）。
pub fn bg_shell_task_id() -> String {
    format!("shell-{}", uuid::Uuid::now_v7())
}

/// 前台（同步）未传 timeout 时的默认超时：偏短，鼓励高效命令。
pub const FOREGROUND_DEFAULT_TIMEOUT_MS: u64 = 15_000;

/// 前台（同步）最大阻塞时长（硬上限）：显式 timeout 与 `timeout: 0` 都界到此值。
/// 同步执行恒有界——不存在禁用超时的路径；到达上限后进程不杀，
/// 而是 promote 为后台任务续跑（见 `BashTool::invoke_output`）。
pub const FOREGROUND_MAX_TIMEOUT_MS: u64 = 120_000;

/// 后台显式 timeout 的上限（后台不阻塞 Agent，允许更长的显式上限）。
pub const BACKGROUND_MAX_TIMEOUT_MS: u64 = 600_000;

/// 显式 timeout 的下限：Windows 进程创建/终止开销大，过短超时不可靠。
fn min_timeout_ms() -> u64 {
    if cfg!(target_os = "windows") {
        5_000
    } else {
        1
    }
}

/// 解析前台（同步）timeout：结果恒为有界值。
///
/// 返回 `(生效毫秒, 请求值被改写时的原始值)`：
/// - 未传 → [`FOREGROUND_DEFAULT_TIMEOUT_MS`]，未改写
/// - 显式 `0` → [`FOREGROUND_MAX_TIMEOUT_MS`]（`0` 不能表示"不超时"），原始值为 `Some(0)`
/// - 显式 > [`FOREGROUND_MAX_TIMEOUT_MS`] → 上限值，原始值为请求值（供回执说明）
/// - 其余 → 请求值，并按平台下限兜底（Unix 1ms / Windows 5000ms）
pub fn parse_foreground_timeout(input: &serde_json::Value) -> (u64, Option<u64>) {
    match input.get("timeout").and_then(|v| v.as_u64()) {
        None => (FOREGROUND_DEFAULT_TIMEOUT_MS, None),
        Some(0) => (FOREGROUND_MAX_TIMEOUT_MS, Some(0)),
        Some(ms) if ms > FOREGROUND_MAX_TIMEOUT_MS => (FOREGROUND_MAX_TIMEOUT_MS, Some(ms)),
        Some(ms) => (ms.max(min_timeout_ms()), None),
    }
}

/// 解析后台 timeout：`None` = 不超时（后台语义：跑完为止，由 Tasks 面板取消）。
///
/// - 未传 / 显式 `0` → None
/// - 显式 >0 → clamp 到 [min, `BACKGROUND_MAX_TIMEOUT_MS`]
pub fn parse_background_timeout(input: &serde_json::Value) -> Option<u64> {
    match input.get("timeout").and_then(|v| v.as_u64()) {
        None | Some(0) => None,
        Some(ms) => Some(ms.clamp(min_timeout_ms(), BACKGROUND_MAX_TIMEOUT_MS)),
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn finalize_bg_shell(
    registry: &BackgroundTaskRegistry,
    on_bg_complete: &Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    task_id: String,
    prompt_summary: String,
    success: bool,
    output: String,
    duration_ms: u64,
    timed_out: bool,
    shell_output: Option<peri_acp_types::event::ShellOutput>,
) {
    let mut result = BackgroundTaskResult {
        task_id: task_id.clone(),
        agent_name: "bg-shell".to_string(),
        prompt_summary,
        success,
        // Shell output is projected through the typed file reference below;
        // keep this field short so callback/reminder size is independent of
        // process output volume.
        output: if shell_output.is_some() {
            if success {
                "Shell command completed; read the output files as needed.".into()
            } else {
                "Shell command failed; read the output files as needed.".into()
            }
        } else {
            output
        },
        tool_calls_count: 0,
        duration_ms,
        child_thread_id: None,
        timed_out,
        subagent_failure: None,
        shell_output: shell_output.map(Box::new),
    };
    // Linearize completion against cancel before publishing anything. A
    // claimed task remains active until the callback and terminal commit
    // finish, so the idle loop cannot exit before its result is enqueued.
    let claimed_completion = registry.claim_completion(&task_id);
    if !claimed_completion && !registry.claim_cancelled_shell_cleanup(&task_id) {
        return;
    }
    if !claimed_completion {
        result.success = false;
        result.output = "Shell command cancelled; read the output files as needed.".into();
    }
    // 回调通知 Agent inbox（在 registry.complete() 之前，与 execute_bg.rs 对齐）
    if let Some(ref cb) = on_bg_complete {
        let callback = std::panic::AssertUnwindSafe(|| cb(&result, BgTaskKind::Shell));
        if let Err(panic) = std::panic::catch_unwind(callback) {
            let detail = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_else(|| "unknown panic".to_string());
            tracing::error!(task_id = %result.task_id, error = %detail, "background shell completion callback panicked");
        }
    }
    // 任务已在启动时注册（run_in_background / promote 路径），此处只收尾推送 Completed。
    if claimed_completion {
        registry.complete(&result.task_id.clone(), result);
    }
}

#[cfg(test)]
mod tests {
    use super::finalize_bg_shell;
    use crate::agent::async_tasks::{
        BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle,
    };
    use peri_acp_types::tasks::BgTaskKind;
    use std::sync::Arc;

    fn registered_shell() -> Arc<BackgroundTaskRegistry> {
        let registry = Arc::new(BackgroundTaskRegistry::new());
        registry
            .register_with_kind(BackgroundTask {
                id: "shell-panic".into(),
                agent_name: "bg-shell".into(),
                prompt_summary: "test".into(),
                status: BackgroundTaskStatus::Running,
                started_at: std::time::Instant::now(),
                chrono_started_at: chrono::Utc::now(),
                kind: BgTaskKind::Shell,
                cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
                cancel_token: None,
                pid: None,
                output_preview: None,
                agent_inbox: None,
                initiator_session_id: None,
                owner_session_id: None,
                owner_identity: None,
            })
            .expect("register test shell");
        registry
    }

    #[test]
    fn callback_panic_still_completes_registered_shell() {
        let registry = registered_shell();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        registry.set_event_sender(sender, "fixture".into());
        let callback_registry = Arc::clone(&registry);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |_, _| {
            assert_eq!(callback_registry.active_count(), 1);
            assert!(matches!(
                callback_registry.cancel("shell-panic"),
                Err(crate::agent::async_tasks::BackgroundRegistryError::TaskCompleting(_))
            ));
            panic!("callback failure");
        });
        finalize_bg_shell(
            &registry,
            &Some(Arc::clone(&callback)),
            "shell-panic".into(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        assert_eq!(registry.active_count(), 0);
        let peri_acp_types::tasks::BgRegistryEvent::Completed { result, .. } =
            events.try_recv().expect("one terminal event")
        else {
            panic!("completion claim must win over cancellation");
        };
        assert!(
            result.success,
            "callback panic must not change process outcome"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn cancelled_shell_reports_owner_cleanup_once_without_second_registry_terminal() {
        let registry = registered_shell();
        let mut changes = registry.subscribe_events();
        registry.cancel("shell-panic").unwrap();
        let called = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_called = Arc::clone(&called);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |result, _| {
            assert!(!result.success);
            callback_called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        finalize_bg_shell(
            &registry,
            &Some(Arc::clone(&callback)),
            "shell-panic".into(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        finalize_bg_shell(
            &registry,
            &Some(callback),
            "shell-panic".into(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(registry.active_count(), 0);
        assert!(matches!(
            changes.try_recv().unwrap().event,
            peri_acp_types::tasks::BgRegistryEvent::Cancelled { .. }
        ));
        assert!(changes.try_recv().is_err());
    }
}
