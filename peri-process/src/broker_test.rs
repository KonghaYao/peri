use super::*;
use std::os::unix::net::UnixListener;
use tokio::process::Command;

#[tokio::test]
async fn child_registers_before_exec_and_uses_dedicated_group() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut hello = [0_u8; 65];
        stream.read_exact(&mut hello).unwrap();
        assert_eq!(hello[0], b'B');
        stream.write_all(&[ACK]).unwrap();
        let mut registered = [0_u8; 5];
        stream.read_exact(&mut registered).unwrap();
        assert_eq!(registered[0], b'P');
        let pid = u32::from_le_bytes(registered[1..].try_into().unwrap());
        assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
        stream.write_all(&[ACK]).unwrap();
        let mut settled = [0_u8; 1];
        stream.read_exact(&mut settled).unwrap();
        assert_eq!(settled[0], b'E');
        assert_eq!(
            unsafe { libc::kill(-(pid as i32), 0) },
            0,
            "anchor must reserve PGID until broker releases it"
        );
        stream.write_all(&[ACK]).unwrap();
        pid
    });
    let registration = Registration::connect(&socket, &"a".repeat(64)).unwrap();
    let mut command = Command::new("sh");
    command.arg("-c").arg("exit 0");
    command.process_group(0);
    registration.prepare_std(command.as_std_mut());
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    assert_eq!(server.join().unwrap(), pid);
    assert!(child.wait().await.unwrap().success());
}

fn supervised_tree() -> (crate::ProcessTree, std::thread::JoinHandle<u32>) {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let host_session = unsafe { libc::getsid(0) };
    let server = std::thread::spawn(move || {
        let _directory = directory;
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut hello = [0_u8; 65];
        stream.read_exact(&mut hello).unwrap();
        assert_eq!(hello[0], b'B');
        stream.write_all(&[ACK]).unwrap();
        let mut registered = [0_u8; 5];
        stream.read_exact(&mut registered).unwrap();
        assert_eq!(registered[0], b'P');
        let pid = u32::from_le_bytes(registered[1..].try_into().unwrap());
        assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
        assert_eq!(unsafe { libc::getsid(pid as i32) }, pid as i32);
        assert_ne!(pid as i32, host_session);
        stream.write_all(&[ACK]).unwrap();
        let mut settled = [0_u8; 1];
        stream.read_exact(&mut settled).unwrap();
        assert_eq!(settled[0], b'E');
        assert_eq!(unsafe { libc::kill(-(pid as i32), 0) }, 0);
        stream.write_all(&[ACK]).unwrap();
        pid
    });
    let registration = Registration::connect(&socket, &"a".repeat(64)).unwrap();
    let mut tree = crate::ProcessTree::new().unwrap();
    tree.broker = Some(registration);
    (tree, server)
}

fn check_anchor_session(command: &mut std::process::Command) {
    unsafe {
        command.pre_exec(|| {
            let group = libc::getpgrp();
            if libc::getsid(0) != group || group == libc::getpid() {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            Ok(())
        });
    }
}

#[tokio::test]
async fn supervised_async_command_shares_isolated_anchor_session() {
    let (mut tree, server) = supervised_tree();
    let mut command = Command::new("true");
    tree.prepare(&mut command);
    check_anchor_session(command.as_std_mut());
    let mut child = command.spawn().unwrap();
    tree.attach(&child).unwrap();
    assert_eq!(server.join().unwrap(), child.id().unwrap());
    assert!(child.wait().await.unwrap().success());
    tree.wait_for_exit().await;
    assert!(tree.is_stopped());
}

#[test]
fn supervised_blocking_command_shares_isolated_anchor_session() {
    let (mut tree, server) = supervised_tree();
    let mut command = std::process::Command::new("true");
    tree.prepare_std(&mut command);
    check_anchor_session(&mut command);
    let mut child = command.spawn().unwrap();
    tree.attach_pid(child.id()).unwrap();
    assert_eq!(server.join().unwrap(), child.id());
    assert!(child.wait().unwrap().success());
    tree.wait_for_exit_blocking();
    assert!(tree.is_stopped());
}

#[test]
fn supervised_duplicate_prepare_reports_session_setup_failure() {
    let (tree, server) = supervised_tree();
    let mut command = std::process::Command::new("true");
    tree.prepare_std(&mut command);
    tree.prepare_std(&mut command);
    assert_eq!(
        command.spawn().unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    let pid = server.join().unwrap();
    assert_eq!(unsafe { libc::kill(-(pid as i32), 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}
