use std::sync::Arc;

use super::registry::BackgroundTaskRegistry;
use crate::agent::events::BackgroundTaskResult;

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
    registry: &Arc<BackgroundTaskRegistry>,
    on_bg_complete: &Option<peri_acp_types::tasks::OnBgCompleteFn>,
    task_id: String,
    prompt_summary: String,
    success: bool,
    output: String,
    duration_ms: u64,
    timed_out: bool,
    shell_output: Option<peri_acp_types::event::ShellOutput>,
) {
    let result = BackgroundTaskResult {
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
    if let Some(delivery) = on_bg_complete {
        if let Err(error) = registry.settle_completed(&task_id, result, Arc::clone(delivery)) {
            tracing::error!(%task_id, %error, "shell terminal delivery is pending");
        }
        return;
    }
    if registry.claim_completion(&task_id) {
        registry.complete(&result.task_id.clone(), result);
    } else {
        registry.claim_cancelled_cleanup(&task_id);
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
    fn callback_panic_retains_registered_shell_until_delivery_retry() {
        let registry = registered_shell();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        registry.set_event_sender(sender, "fixture".into());
        let callback_registry = Arc::clone(&registry);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = Arc::clone(&attempts);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |_, _| {
            assert_eq!(callback_registry.active_count(), 1);
            assert!(matches!(
                callback_registry.cancel("shell-panic"),
                Err(crate::agent::async_tasks::BackgroundRegistryError::TaskCompleting(_))
            ));
            if called.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                panic!("callback failure");
            }
            Ok(())
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
        assert_eq!(registry.active_count(), 1);
        assert!(matches!(
            events.try_recv().unwrap(),
            peri_acp_types::tasks::BgRegistryEvent::Updated { .. }
        ));
        assert_eq!(registry.retry_pending_deliveries(), 1);
        assert_eq!(registry.active_count(), 0);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
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
            Ok(())
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
