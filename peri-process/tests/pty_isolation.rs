#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

use peri_process::ProcessTree;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(5);
const MODE: &str = "PERI_PTY_TEST_MODE";
const API: &str = "PERI_PTY_TEST_API";
const BROKER: &str = "PERI_PTY_TEST_BROKER";
const HOST_SESSION: &str = "PERI_PTY_TEST_HOST_SESSION";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn prepare_std_preserves_real_pty_foreground() {
    run_host("std", false);
}

#[test]
fn prepare_tokio_preserves_real_pty_foreground() {
    run_host("tokio", false);
}

#[test]
fn prepare_std_with_broker_preserves_real_pty_foreground() {
    run_host("std", true);
}

#[test]
fn prepare_tokio_with_broker_preserves_real_pty_foreground() {
    run_host("tokio", true);
}

#[test]
#[ignore = "subprocess entry point, invoked by the four PTY regressions"]
fn pty_fixture_entry() {
    match std::env::var(MODE).unwrap().as_str() {
        "host" => host_fixture(),
        "probe" => probe_fixture(),
        mode => panic!("unknown PTY fixture mode: {mode}"),
    }
}

fn fixture_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "pty_fixture_entry",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MODE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_host(api: &str, broker: bool) {
    let home = tempfile::tempdir().unwrap();
    let mut command = fixture_command("host");
    command
        .env_clear()
        .env(MODE, "host")
        .env(API, api)
        .env(BROKER, if broker { "yes" } else { "no" })
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("LANG", "C");
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ReapedChild::new(command.spawn().unwrap());
    let output = child.output(Duration::from_secs(25));
    assert!(
        output.status.success(),
        "PTY host {api}, broker={broker}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct ReapedChild {
    child: Option<Child>,
}

impl ReapedChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn output(&mut self, limit: Duration) -> Output {
        let deadline = Instant::now() + limit;
        let child = self.child.as_mut().unwrap();
        loop {
            if child.try_wait().unwrap().is_some() {
                return self.child.take().unwrap().wait_with_output().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "PTY subprocess exceeded {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let deadline = Instant::now() + LIMIT;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    result => {
                        let diagnostic = format!(
                            "PTY cleanup could not reap PID {} within {LIMIT:?}: {result:?}\n",
                            child.id()
                        );
                        if std::thread::panicking() {
                            let _ = std::io::stderr().write_all(diagnostic.as_bytes());
                        } else {
                            panic!("{diagnostic}");
                        }
                        break;
                    }
                }
            }
        }
    }
}

struct Terminal {
    master: OwnedFd,
    slave: OwnedFd,
    foreground: libc::pid_t,
}

impl Terminal {
    fn acquire() -> Self {
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0,
            "openpty: {}",
            std::io::Error::last_os_error()
        );
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        for descriptor in [&master, &slave] {
            assert_eq!(
                unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        }
        assert_eq!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0) },
            0,
            "TIOCSCTTY: {}",
            std::io::Error::last_os_error()
        );
        let foreground = unsafe { libc::getpgrp() };
        assert_eq!(unsafe { libc::getsid(0) }, unsafe { libc::getpid() });
        assert_eq!(unsafe { libc::tcsetpgrp(slave.as_raw_fd(), foreground) }, 0);
        let terminal = Self {
            master,
            slave,
            foreground,
        };
        terminal.assert_foreground();
        assert!(OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok());
        terminal
    }

    fn assert_foreground(&self) {
        assert_eq!(
            unsafe { libc::tcgetpgrp(self.slave.as_raw_fd()) },
            self.foreground,
            "prepared child stole the host PTY foreground group"
        );
    }

    fn assert_readable(&self) {
        self.assert_foreground();
        let input = b"x\n";
        assert_eq!(
            unsafe { libc::write(self.master.as_raw_fd(), input.as_ptr().cast(), input.len()) },
            input.len() as isize
        );
        let mut poll = libc::pollfd {
            fd: self.slave.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 1000) }, 1);
        let mut received = [0_u8; 2];
        assert_eq!(
            unsafe {
                libc::read(
                    self.slave.as_raw_fd(),
                    received.as_mut_ptr().cast(),
                    received.len(),
                )
            },
            received.len() as isize
        );
        assert_eq!(&received, input);
    }
}

fn probe_fixture() {
    let host_session: libc::pid_t = std::env::var(HOST_SESSION).unwrap().parse().unwrap();
    for descriptor in 0..=2 {
        assert_eq!(unsafe { libc::isatty(descriptor) }, 0);
    }
    let controlling_tty = OpenOptions::new().read(true).write(true).open("/dev/tty");
    if let Ok(terminal) = &controlling_tty {
        unsafe {
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        }
        assert_eq!(
            unsafe { libc::tcsetpgrp(terminal.as_raw_fd(), libc::getpgrp()) },
            0
        );
    }
    assert!(
        controlling_tty.is_err(),
        "prepared child can open /dev/tty despite null stdin and piped output"
    );
    assert_eq!(
        controlling_tty.unwrap_err().raw_os_error(),
        Some(libc::ENXIO)
    );
    let session = unsafe { libc::getsid(0) };
    assert_ne!(
        session, host_session,
        "prepared child inherited host session"
    );
    assert_eq!(
        session,
        unsafe { libc::getpgrp() },
        "session/group identity"
    );
}

fn host_fixture() {
    let terminal = Terminal::acquire();
    let api = std::env::var(API).unwrap();
    let broker = std::env::var(BROKER).unwrap() == "yes";
    let mut probe = fixture_command("probe");
    probe.env(HOST_SESSION, unsafe { libc::getsid(0) }.to_string());
    let output = execute(probe, &api, broker);
    terminal.assert_readable();
    assert_success(output, "deterministic controlling-terminal probe");

    let zsh = ["/bin/zsh", "/usr/bin/zsh"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file());
    if let Some(zsh) = zsh {
        let mut shell = Command::new(zsh);
        shell
            .args(["-fic", "print peri-pty-probe"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = execute(shell, &api, broker);
        terminal.assert_readable();
        assert!(String::from_utf8_lossy(&output.stdout).contains("peri-pty-probe"));
        assert_success(output, "zsh -fic without user rc");
    } else {
        std::io::stdout()
            .write_all(b"optional zsh enhancement unavailable; deterministic PTY probe completed\n")
            .unwrap();
    }
}

fn assert_success(output: Output, context: &str) {
    assert!(
        output.status.success(),
        "{context}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn execute(mut command: Command, api: &str, supervised: bool) -> Output {
    let broker = supervised.then(TestBroker::start);
    let mut tree = ProcessTree::new().unwrap();
    let output = if api == "std" {
        tree.prepare_std(&mut command);
        unsafe {
            command.pre_exec(|| {
                libc::alarm(5);
                Ok(())
            });
        }
        let mut child = ReapedChild::new(command.spawn().unwrap());
        tree.attach_pid(child.child.as_ref().unwrap().id()).unwrap();
        let output = child.output(LIMIT);
        let deadline = Instant::now() + LIMIT;
        while !tree.is_stopped() {
            assert!(Instant::now() < deadline, "prepared std group did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        output
    } else {
        assert_eq!(api, "tokio");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut command = tokio::process::Command::from(command);
            tree.prepare(&mut command);
            unsafe {
                command.pre_exec(|| {
                    libc::alarm(5);
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            tree.attach(&child).unwrap();
            let output = tokio::time::timeout(LIMIT, child.wait_with_output())
                .await
                .expect("prepared tokio child timed out")
                .unwrap();
            tokio::time::timeout(LIMIT, tree.wait_for_exit())
                .await
                .expect("prepared tokio group did not exit");
            output
        })
    };
    if let Some(broker) = broker {
        broker.finish();
    }
    output
}

struct TestBroker {
    worker: std::thread::JoinHandle<()>,
    directory: tempfile::TempDir,
}

impl TestBroker {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::env::set_var("PERI_PROCESS_BROKER_SOCKET", &socket);
        std::env::set_var("PERI_PROCESS_BROKER_TOKEN", TOKEN);
        let host_session = unsafe { libc::getsid(0) };
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + LIMIT;
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "broker accept timed out");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("broker accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(LIMIT)).unwrap();
            stream.set_write_timeout(Some(LIMIT)).unwrap();
            let mut hello = [0_u8; 65];
            stream.read_exact(&mut hello).unwrap();
            assert_eq!(hello[0], b'B');
            assert_eq!(&hello[1..], TOKEN.as_bytes());
            stream.write_all(&[0x06]).unwrap();
            let mut registration = [0_u8; 5];
            stream.read_exact(&mut registration).unwrap();
            assert_eq!(registration[0], b'P');
            let leader = u32::from_le_bytes(registration[1..].try_into().unwrap()) as libc::pid_t;
            assert_eq!(unsafe { libc::getpgid(leader) }, leader);
            let session = unsafe { libc::getsid(leader) };
            assert_ne!(
                session, host_session,
                "broker registered host-session child"
            );
            assert_eq!(session, leader, "broker anchor must lead isolated session");
            stream.write_all(&[0x06]).unwrap();
            let mut exited = [0_u8; 1];
            stream.read_exact(&mut exited).unwrap();
            assert_eq!(&exited, b"E");
            stream.write_all(&[0x06]).unwrap();
        });
        Self { worker, directory }
    }

    fn finish(self) {
        std::env::remove_var("PERI_PROCESS_BROKER_SOCKET");
        std::env::remove_var("PERI_PROCESS_BROKER_TOKEN");
        self.worker.join().expect("PTY fixture broker failed");
        drop(self.directory);
    }
}
