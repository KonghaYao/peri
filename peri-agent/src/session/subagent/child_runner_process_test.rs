use super::*;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn group_alive(pgid: i32) -> bool {
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// 真实受控阻塞进程树：D 后父/子进程都存活；正式取消杀死整个进程组并回收输出管道。
#[tokio::test(start_paused = true)]
async fn real_blocking_shell_tree_survives_deadline_then_cancel_cleans_tree() {
    let temp = tempfile::tempdir().unwrap();
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 300 & echo $!; wait")
        .current_dir(temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut process = command.spawn().unwrap();
    let pgid = process.id() as i32;
    assert_eq!(
        unsafe { libc::getpgid(pgid) },
        pgid,
        "sh 必须是独立进程组 leader"
    );
    let mut stdout = BufReader::new(process.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
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
                    unsafe {
                        libc::kill(-pgid, libc::SIGKILL);
                    }
                }
            })),
        })
        .unwrap();
    let child = make_child(manager.clone(), true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run a real blocking shell tree"),
    ));
    let built = fixed_context(&child, "waiting for shell");
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        run_child_until_terminal(built.context, 10, &child),
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

    manager.cancel_async("real-shell").await.unwrap();
    let waited = process.lock().take().unwrap();
    let output = waited.wait_with_output().unwrap();
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
