use std::process::Stdio;
use std::sync::Arc;

use super::shell::{kill_process_group, shell_command, tee_pipe_with_output, ShellExecutionGuard};
use super::shell_output::ShellOutputCapture;
use futures::FutureExt;
use peri_acp_types::tasks::{BgShellHandle, BgTaskKind, ExternalExecutionGuard, OnBgCompleteFn};
use peri_agent::agent::async_tasks::{
    bg_shell_task_id, finalize_bg_shell, BackgroundTask, BackgroundTaskRegistry,
    BackgroundTaskStatus, BgCancelHandle, ShellExecutor,
};

pub struct LocalShellExecutor;

impl ShellExecutor for LocalShellExecutor {
    fn cancel_callback(
        &self,
        pid: u32,
        registry: Arc<BackgroundTaskRegistry>,
    ) -> Option<Box<dyn FnOnce() + Send + Sync>> {
        #[cfg(unix)]
        {
            Some(Box::new(move || {
                registry.spawn_execution_cleanup(super::shell::terminate_process_group(pid));
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, registry);
            None
        }
    }

    fn spawn(
        &self,
        registry: Arc<BackgroundTaskRegistry>,
        ownership: Box<dyn ExternalExecutionGuard>,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        let mut execution = ShellExecutionGuard::new(Some(ownership));
        let task_id = bg_shell_task_id();
        let command_owned = command;
        let on_bg_complete_cb = on_bg_complete;
        let task_id_for_return = task_id.clone();

        // 同步 spawn：PID 必须在返回前确定，供工具层回显给 LLM 管理任务
        let mut cmd = shell_command(&command_owned, &[]);
        cmd.current_dir(&cwd)
            // stdin 重定向为 null：后台任务同样不依赖终端输入（与 Bash 工具
            // 同步路径一致），否则读 stdin 的进程会永远阻塞等待 EOF。
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        execution.prepare(&mut cmd)?;
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // Register first, then use the same callback boundary as all
                // other shell completions. A callback panic must not skip the
                // registry terminal transition.
                let bg_task = BackgroundTask {
                    id: task_id.clone(),
                    agent_name: "bg-shell".to_string(),
                    prompt_summary: command_owned.chars().take(80).collect(),
                    status: BackgroundTaskStatus::Running,
                    started_at: peri_time::monotonic_now(),
                    chrono_started_at: chrono::DateTime::<chrono::Utc>::from(peri_time::now_wall()),
                    kind: BgTaskKind::Shell,
                    cancel_handle: BgCancelHandle::Kill(None),
                    cancel_token: None,
                    pid: None,
                    output_preview: None,
                    agent_inbox: None,
                };
                if let Err(registration_error) = registry.register_external_admitted(bg_task) {
                    // No registered task can publish a completion. Return the
                    // failure now instead of promising an eventual notification.
                    return Err(format!("Failed to spawn: {e}; {registration_error}").into());
                }
                finalize_bg_shell(
                    &registry,
                    &on_bg_complete_cb,
                    task_id.clone(),
                    command_owned.chars().take(80).collect(),
                    false,
                    format!("Failed to spawn: {e}"),
                    0,
                    false,
                    None,
                );
                return Ok(BgShellHandle {
                    task_id: task_id_for_return,
                    pid: None,
                    stdout_log: None,
                    stderr_log: None,
                });
            }
        };
        let pid = child
            .id()
            .expect("bg shell: child.id() returned None after successful spawn");
        if let Err(error) = execution.attach(&child) {
            registry.spawn_execution_cleanup(async move {
                let _ = child.kill().await;
                execution.confirm_stopped();
            });
            return Err(error.into());
        }

        // Create durable files before the readers start. The full stream is
        // therefore retained even when the in-memory preview reaches 2 MiB.
        let mut output_capture = ShellOutputCapture::new(&format!("bg-{task_id}"));
        let stdout_log_path = output_capture.stdout_path();
        let stderr_log_path = output_capture.stderr_path();

        // 任务启动即注册：推送 BgTaskStarted 事件，运行期间 TUI 展示栏可见。
        // 完成时 finalize_bg_shell 只调 complete()，不再重复注册。
        let bg_task = BackgroundTask {
            id: task_id.clone(),
            agent_name: "bg-shell".to_string(),
            prompt_summary: command_owned.chars().take(80).collect(),
            status: BackgroundTaskStatus::Running,
            started_at: peri_time::monotonic_now(),
            chrono_started_at: chrono::DateTime::<chrono::Utc>::from(peri_time::now_wall()),
            kind: BgTaskKind::Shell,
            cancel_handle: BgCancelHandle::Kill(
                execution.scoped_cancel_callback(Arc::clone(&registry)),
            ),
            cancel_token: None,
            pid: Some(pid),
            output_preview: None,
            agent_inbox: None,
        };
        if let Err(error) = registry.register_external_admitted(bg_task) {
            kill_process_group(pid, "KILL");
            registry.spawn_execution_cleanup(async move {
                let _ = child.wait().await;
                execution.confirm_stopped();
                output_capture.cleanup().await;
            });
            return Err(error.into());
        }

        // The returned handle publishes live output paths. They outlive the
        // worker and remain readable even after cancellation/completion.
        output_capture.retain_files();
        let stdout_writer = output_capture.stdout_writer();
        let stderr_writer = output_capture.stderr_writer();
        let output_capture = Arc::new(output_capture);

        Arc::clone(&registry).spawn_execution_cleanup(async move {
            // 外層 catch_unwind 保護：確保任何意外 panic 也會調用 registry.complete()，
            // 防止 bg shell 任務殘留在狀態欄。
            let started = peri_time::monotonic_now();
            let result = std::panic::AssertUnwindSafe(async {
                // 流式读取 stdout/stderr：tee 到日志文件（运行期 agent 可读）+ 内存缓冲
                // （wait_with_output 内部消费管道无法 tee，故显式 take pipe 自行读取）
                let stdout_reader = tokio::io::BufReader::new(
                    child.stdout.take().expect("bg shell: stdout is piped"),
                );
                let stderr_reader = tokio::io::BufReader::new(
                    child.stderr.take().expect("bg shell: stderr is piped"),
                );
                let stdout_buf = Arc::new(std::sync::Mutex::new(String::new()));
                let stderr_buf = Arc::new(std::sync::Mutex::new(String::new()));
                let drain_stdout = tokio::spawn(tee_pipe_with_output(
                    stdout_reader,
                    stdout_buf.clone(),
                    stdout_writer,
                    Arc::clone(&output_capture),
                    "stdout",
                ));
                let drain_stderr = tokio::spawn(tee_pipe_with_output(
                    stderr_reader,
                    stderr_buf.clone(),
                    stderr_writer,
                    Arc::clone(&output_capture),
                    "stderr",
                ));

                // 超时包裹 wait（后台未显式传 timeout 或 timeout=0 时不超时）
                let wait_result = match timeout_ms {
                    None => child.wait().await.map(Some),
                    Some(ms) => {
                        match peri_time::timeout(
                            std::time::Duration::from_millis(ms),
                            child.wait(),
                        )
                        .await
                        {
                            Ok(status) => status.map(Some),
                            Err(_elapsed) => {
                                // 超时：kill 整个进程组（bash 为组长，负号 PID 语义），
                                // 等待实际终态后再发完成事件。
                                kill_process_group(pid, "KILL");
                                let exit_status = child.wait().await.ok();
                                if exit_status.is_none() {
                                    output_capture.mark_incomplete("process exit status unavailable");
                                }
                                if let Err(error) = drain_stdout.await {
                                    output_capture.record_task_error("stdout", error);
                                }
                                if let Err(error) = drain_stderr.await {
                                    output_capture.record_task_error("stderr", error);
                                }
                                execution.wait_for_exit().await;
                                execution.confirm_stopped();
                                registry.confirm_external_stopped(&task_id);
                                finalize_bg_shell(
                                    &registry,
                                    &on_bg_complete_cb,
                                    task_id.clone(),
                                    command_owned.chars().take(80).collect(),
                                    false,
                                    format!(
                                        "Command timed out after {}s; process group was terminated.",
                                        ms as f64 / 1000.0
                                    ),
                                    started.elapsed().as_millis() as u64,
                                    true,
                                    Some(output_capture.finish(
                                        exit_status.as_ref().and_then(std::process::ExitStatus::code),
                                    )),
                                );
                                return;
                            }
                        }
                    }
                };

                let output = match wait_result {
                    Ok(Some(status)) => {
                        if let Err(error) = drain_stdout.await {
                            output_capture.record_task_error("stdout", error);
                        }
                        if let Err(error) = drain_stderr.await {
                            output_capture.record_task_error("stderr", error);
                        }
                        let success = status.success();
                        let stdout = match stdout_buf.lock() {
                            Ok(g) => g.clone(),
                            Err(poisoned) => poisoned.into_inner().clone(),
                        };
                        let stderr = match stderr_buf.lock() {
                            Ok(g) => g.clone(),
                            Err(poisoned) => poisoned.into_inner().clone(),
                        };
                        let mut combined = String::new();
                        if !stdout.is_empty() {
                            combined.push_str(&stdout);
                        }
                        if !stderr.is_empty() {
                            if !combined.is_empty() {
                                combined.push('\n');
                            }
                            combined.push_str("[stderr]\n");
                            combined.push_str(&stderr);
                        }
                        if combined.is_empty() {
                            combined = format!("[exit code: {}]", status.code().unwrap_or(-1));
                        }
                        (success, combined, status.code())
                    }
                    Err(e) => {
                        tracing::error!(task_id = %task_id, error = %e, "background shell wait failed");
                        // A failed wait does not prove that the process or
                        // its pipe readers stopped. Settle both before
                        // publishing the failure and output references.
                        kill_process_group(pid, "KILL");
                        let _ = child.wait().await;
                        if let Err(error) = drain_stdout.await {
                            output_capture.record_task_error("stdout", error);
                        }
                        if let Err(error) = drain_stderr.await {
                            output_capture.record_task_error("stderr", error);
                        }
                        (false, format!("Command failed: {e}"), None)
                    }
                    // unreachable: child.wait() 恒返回 Ok(ExitStatus)
                    Ok(None) => {
                        unreachable!("bg shell: child.wait returned Ok(None)")
                    }
                };

                execution.wait_for_exit().await;
                execution.confirm_stopped();
                // Process cleanup is independent of the visible task. Cancel
                // may already have removed it before completion can claim it.
                registry.confirm_external_stopped(&task_id);
                // 回调通知 + 完成（任务在启动时已注册，与 promote 续跑共用收尾逻辑）
                finalize_bg_shell(
                    &registry,
                    &on_bg_complete_cb,
                    task_id.clone(),
                    command_owned.chars().take(80).collect(),
                    output.0,
                    output.1,
                    started.elapsed().as_millis() as u64,
                    false,
                    Some(output_capture.finish(output.2)),
                );
            })
            .catch_unwind()
            .await;
            if let Err(panic_err) = result {
                kill_process_group(pid, "KILL");
                let _ = child.wait().await;
                execution.wait_for_exit().await;
                execution.confirm_stopped();
                registry.confirm_external_stopped(&task_id);
                // spawn 閉包內部 panic：嘗試用現有 task_id 發送失敗事件
                let panic_msg = if let Some(s) = panic_err.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "unknown panic".to_string()
                };
                output_capture.mark_incomplete("background shell worker panicked");
                // The task was registered before the worker was spawned. Do
                // not re-register here: cancellation may already have
                // removed the entry, and `complete` will then suppress the
                // duplicate terminal event as intended.
                finalize_bg_shell(
                    &registry,
                    &on_bg_complete_cb,
                    task_id.clone(),
                    command_owned.chars().take(80).collect(),
                    false,
                    format!("Background shell task panicked: {panic_msg}"),
                    started.elapsed().as_millis() as u64,
                    false,
                    Some(output_capture.finish(None)),
                );
            }
        });

        Ok(BgShellHandle {
            task_id: task_id_for_return,
            pid: Some(pid),
            stdout_log: stdout_log_path,
            stderr_log: stderr_log_path,
        })
    }
}
