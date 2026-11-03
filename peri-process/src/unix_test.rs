use crate::ProcessTree;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn check_session(command: &mut std::process::Command) {
    let host_session = unsafe { libc::getsid(0) };
    unsafe {
        command.pre_exec(move || {
            let pid = libc::getpid();
            let session = libc::getsid(0);
            if session == host_session || session != pid || libc::getpgrp() != pid {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            Ok(())
        });
    }
}

#[tokio::test]
async fn async_prepare_creates_session_and_dedicated_group() {
    let mut command = tokio::process::Command::new("true");
    command.stdin(Stdio::null());
    let mut tree = ProcessTree::new().unwrap();
    tree.prepare(&mut command);
    check_session(command.as_std_mut());
    let mut child = command.spawn().unwrap();
    tree.attach(&child).unwrap();
    assert!(child.wait().await.unwrap().success());
    tree.wait_for_exit().await;
    assert!(tree.is_stopped());
}

#[test]
fn blocking_prepare_creates_session_and_dedicated_group() {
    let mut command = std::process::Command::new("true");
    command.stdin(Stdio::null());
    let mut tree = ProcessTree::new().unwrap();
    tree.prepare_std(&mut command);
    check_session(&mut command);
    let mut child = command.spawn().unwrap();
    tree.attach_pid(child.id()).unwrap();
    assert!(child.wait().unwrap().success());
    tree.wait_for_exit_blocking();
    assert!(tree.is_stopped());
}

#[test]
fn prepare_overrides_existing_group_configuration() {
    let mut command = std::process::Command::new("true");
    command.process_group(0);
    let tree = ProcessTree::new().unwrap();
    tree.prepare_std(&mut command);
    check_session(&mut command);
    assert!(command.status().unwrap().success());
}

#[tokio::test]
async fn prepare_survives_later_group_configuration() {
    let mut command = tokio::process::Command::new("true");
    let tree = ProcessTree::new().unwrap();
    tree.prepare(&mut command);
    command.process_group(0);
    check_session(command.as_std_mut());
    assert!(command.status().await.unwrap().success());
}

#[test]
fn duplicate_prepare_reports_session_setup_failure() {
    let mut command = std::process::Command::new("true");
    let tree = ProcessTree::new().unwrap();
    tree.prepare_std(&mut command);
    tree.prepare_std(&mut command);
    assert_eq!(
        command.spawn().unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
}

#[test]
fn one_owner_can_prepare_distinct_commands() {
    let tree = ProcessTree::new().unwrap();
    for _attempt in 0..2 {
        let mut command = std::process::Command::new("true");
        tree.prepare_std(&mut command);
        check_session(&mut command);
        assert!(command.status().unwrap().success());
    }
}

#[test]
fn prepared_command_can_spawn_again() {
    let tree = ProcessTree::new().unwrap();
    let mut command = std::process::Command::new("true");
    tree.prepare_std(&mut command);
    check_session(&mut command);
    for _attempt in 0..2 {
        assert!(command.status().unwrap().success());
    }
}

async fn echo_probe(child: &mut tokio::process::Child, marker: &[u8]) -> io::Result<Vec<u8>> {
    child
        .stdin
        .as_mut()
        .ok_or_else(|| io::Error::other("probe stdin missing"))?
        .write_all(marker)
        .await?;
    let mut response = vec![0; marker.len()];
    child
        .stdout
        .as_mut()
        .ok_or_else(|| io::Error::other("probe stdout missing"))?
        .read_exact(&mut response)
        .await?;
    Ok(response)
}

#[tokio::test]
async fn settled_owner_does_not_signal_reassigned_owned_group() {
    let mut command = tokio::process::Command::new("true");
    let mut settled_tree = ProcessTree::new().unwrap();
    settled_tree.prepare(&mut command);
    let mut child = command.spawn().unwrap();
    settled_tree.attach(&child).unwrap();
    assert!(child.wait().await.unwrap().success());
    tokio::time::timeout(Duration::from_secs(5), settled_tree.wait_for_exit())
        .await
        .unwrap();
    assert!(settled_tree.attempted);
    assert!(settled_tree.settled.load(Ordering::Acquire));
    // 第一棵树已经回收，先清除旧数值；后续只注入本测试仍持有的活跃组。
    settled_tree.pid = None;

    let mut command = tokio::process::Command::new("/bin/cat");
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut target_tree = ProcessTree::new().unwrap();
    target_tree.prepare(&mut command);
    let mut target = command.spawn().unwrap();
    target_tree.attach(&target).unwrap();
    let target_pid = target.id().unwrap();
    let ready =
        tokio::time::timeout(Duration::from_secs(5), echo_probe(&mut target, b"READY\n")).await;
    let response = if matches!(&ready, Ok(Ok(bytes)) if bytes == b"READY\n") {
        // 修改实际信号 owner 的私有 pid，而非调用方的旁路字段；不迫使系统复用 PID。
        settled_tree.pid = Some(target_pid);
        settled_tree.terminate();
        // 无论防护是否正确，任何后续等待/断言前撤掉注入，Drop 不会再次信号该组。
        settled_tree.pid = None;
        Some(
            tokio::time::timeout(Duration::from_secs(5), echo_probe(&mut target, b"AFTER\n")).await,
        )
    } else {
        None
    };

    // 包括红测误杀目标的路径：先回收自有目标，再断言，不留下活进程或未 reap 的 leader。
    target_tree.terminate();
    let reaped = tokio::time::timeout(Duration::from_secs(5), target.wait()).await;
    let stopped = tokio::time::timeout(Duration::from_secs(5), target_tree.wait_for_exit()).await;
    if stopped.is_ok() {
        target_tree.pid = None;
    }
    assert!(matches!(&reaped, Ok(Ok(_))), "目标未 reap：{reaped:?}");
    assert!(stopped.is_ok(), "目标组未确认退出：{stopped:?}");
    assert!(
        matches!(&ready, Ok(Ok(bytes)) if bytes == b"READY\n"),
        "前置握手失败：{ready:?}"
    );
    assert!(
        matches!(&response, Some(Ok(Ok(bytes))) if bytes == b"AFTER\n"),
        "settled owner 不得终止另一个自有进程组：{response:?}"
    );
}
