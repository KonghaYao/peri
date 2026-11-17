use crate::agent::async_tasks::TaskManager;
use crate::session::queue::MessageSource;
use crate::session::queue::QueuedMessage;
use peri_acp_types::tasks::TaskManager as _;

use crate::session::subagent::ChildRunOutcome;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::tasks::BgTaskRegistration;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

fn alive(pid: i32) -> bool {
    // SAFETY: Signal 0 only probes this numeric process ID; kill takes no pointers.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn group_alive(pgid: i32) -> bool {
    // SAFETY: Signal 0 probes the test-owned process group without sending a signal.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// 真实受控阻塞进程树：D 后父/子进程都存活；正式取消杀死整个进程组并回收输出管道。
#[tokio::test(start_paused = true)]
async fn real_blocking_shell_tree_survives_deadline_then_cancel_cleans_tree() {
    let temp = tempfile::tempdir().expect("the temporary test directory must be available");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 300 & echo $!; wait")
        .current_dir(temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut process = command
        .spawn()
        .expect("the spawn fixture result is required for this assertion");
    let pgid = process.id() as i32;
    // SAFETY: getpgid only reads the numeric ID of the child created above.
    let actual_pgid = unsafe { libc::getpgid(pgid) };
    assert_eq!(actual_pgid, pgid, "sh 必须是独立进程组 leader");
    let mut stdout = BufReader::new(
        process
            .stdout
            .take()
            .expect("process stdout fixture must provide its pending value exactly once"),
    );
    let mut line = String::new();
    stdout
        .read_line(&mut line)
        .expect("the read line fixture result is required for this assertion");
    let grandchild_pid: i32 = line.trim().parse().expect("shell must print child pid");
    assert!(alive(grandchild_pid), "受控子进程必须存活");
    assert!(alive(pgid), "受控父进程必须存活");
    drop(stdout);

    let process = Arc::new(parking_lot::Mutex::new(Some(process)));
    let kill_process = Arc::clone(&process);
    let manager = Arc::new(TaskManager::new());
    manager
        .register(BgTaskRegistration {
            task_id: "real-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 300 (process tree)".into(),
            pid: Some(pgid as u32),
            kill: Some(Box::new(move || {
                if kill_process.lock().is_some() {
                    // SAFETY: The child is still owned here and was created in its own process
                    // group, so the signal targets only this test-owned process tree.
                    unsafe {
                        libc::kill(-pgid, libc::SIGKILL);
                    }
                }
            })),
        })
        .expect("the register fixture result is required for this assertion");
    let child = crate::session::subagent::child_runner::tests::make_child(manager.clone(), true);
    child.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run a real blocking shell tree"),
    ));
    let built =
        crate::session::subagent::child_runner::tests::fixed_context(&child, "waiting for shell");
    let permit = crate::session::subagent::child_runner::tests::permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        crate::session::subagent::child_runner::run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("deadline must end the settlement wait");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("real shell must be handed off: {outcome:?}");
    };
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "real-shell"));
    assert!(
        alive(pgid) && alive(grandchild_pid),
        "D 后父/子真实进程都必须仍存活"
    );
    assert_eq!(manager.active_count(), 1, "Shell 不得被 deadline 取消");

    manager
        .cancel_async("real-shell")
        .await
        .expect("the cancel async fixture result is required for this assertion");
    let waited = process
        .lock()
        .take()
        .expect("process lock fixture must provide its pending value exactly once");
    let output = waited
        .wait_with_output()
        .expect("the wait with output fixture result is required for this assertion");
    assert!(!output.status.success(), "被取消进程不得报告成功退出");
    assert!(output.stdout.is_empty(), "被接管的 stdout 必须已关闭并回收");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while group_alive(pgid) || alive(grandchild_pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "正式取消必须清理整个进程组（pgid={pgid}, child={grandchild_pid}）"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
