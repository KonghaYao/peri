use crate::ProcessTree;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Stdio;

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
