use super::*;
use std::process::{Command, Stdio};

/// 真实受控阻塞进程：D 后仍存活；正式取消接口杀死进程并回收输出管道。
#[tokio::test(start_paused = true)]
async fn real_blocking_shell_survives_deadline_then_cancel_cleans_process() {
    let temp = tempfile::tempdir().unwrap();
    let process = Command::new("sh")
        .arg("-c")
        .arg("exec sleep 300")
        .current_dir(temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = process.id();
    let process = Arc::new(parking_lot::Mutex::new(Some(process)));
    let kill_process = Arc::clone(&process);
    let manager = Arc::new(TaskManager::new());
    manager
        .register(BgTaskRegistration {
            task_id: "real-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 300".into(),
            pid: Some(pid),
            kill: Some(Box::new(move || {
                if let Some(process) = kill_process.lock().as_mut() {
                    let _ = process.kill();
                }
            })),
        })
        .unwrap();
    let child = make_child(manager.clone(), true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run a real blocking shell"),
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
        process
            .lock()
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none(),
        "D 后真实进程必须仍存活"
    );
    assert_eq!(manager.active_count(), 1, "Shell 不得被 deadline 取消");

    manager.cancel_async("real-shell").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let exited = process
            .lock()
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_some();
        if exited {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "正式取消接口必须终止真实进程"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let process = process.lock().take().unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(!output.status.success(), "被取消进程不得报告成功退出");
    assert!(output.stdout.is_empty(), "输出管道必须已关闭并回收");
}
