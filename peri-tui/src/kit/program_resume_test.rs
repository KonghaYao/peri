use super::ResumeCommand;
use super::Sender;
use super::Write;

const TOKEN: &str = "test-token";
const PID: u32 = 4242;
const TIME: u64 = 1_700_000_000;
const MAX_ARGV_ELEMENTS: usize = 64;

fn sender() -> Sender<'static> {
    Sender {
        token: TOKEN,
        pid: PID,
        time: TIME,
    }
}

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn no_env(_: &str) -> Option<String> {
    None
}

struct TestWriter {
    bytes: Vec<u8>,
    flushes: usize,
}

impl TestWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            flushes: 0,
        }
    }
}

impl Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

/// 固定向量由 Python `hashlib.sha256` + `base64` 独立算出，不用实现自身生成期望值。
#[test]
fn report_sequence_matches_fixed_vector() {
    let argv = argv(&[
        "/usr/local/bin/peri",
        "--resume",
        "01a20d19-c876-7d00-b8b3-4513bedd42d0",
    ]);
    let sequence = super::format_report(sender(), &argv, Some("/Users/kk/project")).expect(
        "the fixed vector is reportable: the python reference produces the same mac for it",
    );

    assert_eq!(
        sequence,
        concat!(
            "\x1b]7577;pid=4242:time=1700000000:",
            "argv=L3Vzci9sb2NhbC9iaW4vcGVyaQ==,LS1yZXN1bWU=,MDFhMjBkMTktYzg3Ni03ZDAwLWI4YjMtNDUxM2JlZGQ0MmQw",
            ":cwd=L1VzZXJzL2trL3Byb2plY3Q=",
            ":mac=b4444ed5e6cebe6a067bcde97288f6bf488de4289a95c0ef3be3db1f9262433c",
            "\x1b\\"
        )
    );
}

#[test]
fn report_without_cwd_matches_the_reference_mac() {
    let argv = argv(&["peri", "--resume", "abc"]);
    let sequence = super::format_report(sender(), &argv, None).expect(
        "a bare program name without cwd is reportable: the python reference uses the same body",
    );

    assert_eq!(
        sequence,
        concat!(
            "\x1b]7577;pid=4242:time=1700000000:argv=cGVyaQ==,LS1yZXN1bWU=,YWJj",
            ":mac=2f530a446fbe3a79941937c428cab8272ff1eeb36d963e32c6b27361bba93281",
            "\x1b\\"
        )
    );
}

#[test]
fn utf8_text_is_base64_encoded_before_hashing() {
    let sender = Sender {
        token: "tökén",
        pid: 1,
        time: 1,
    };
    let sequence = super::format_report(sender, &argv(&["/bin/peri"]), Some("/tmp/ü"))
        .expect("UTF-8 token and cwd are reportable: the protocol hashes the encoded UTF-8 bytes");

    assert_eq!(
        sequence,
        concat!(
            "\x1b]7577;pid=1:time=1:argv=L2Jpbi9wZXJp:cwd=L3RtcC/DvA==",
            ":mac=02526d501bf46a302e2df8b5c915b2aa4067ce5623f39251cd71d8a9276627ba",
            "\x1b\\"
        )
    );
}

#[test]
fn clear_sequence_matches_fixed_vector() {
    let sequence = super::format_clear(sender()).expect(
        "clear is emitted whenever a report was sent: the python reference signs the same body",
    );

    assert_eq!(
        sequence,
        concat!(
            "\x1b]7577;pid=4242:time=1700000000:clear=1",
            ":mac=03c2dfb54bcc26eb7a08bf3e32f5c6723f24b203ab452df528d433bdb3a5c999",
            "\x1b\\"
        )
    );
}

#[test]
fn unreportable_commands_produce_no_sequence() {
    let mut too_many = argv(&["peri"]);
    for _ in 0..MAX_ARGV_ELEMENTS {
        too_many.push("x".to_owned());
    }
    let cases: [(&str, Vec<String>, Option<&str>); 9] = [
        ("空 argv", Vec::new(), None),
        ("argv[0] 以 - 开头", argv(&["-peri"]), None),
        ("argv[0] 含 / 但不是绝对路径", argv(&["foo/bar"]), None),
        ("argv[0] 含反斜杠但不是绝对路径", argv(&["foo\\bar"]), None),
        (
            "cwd 是相对路径",
            argv(&["peri", "--resume", "abc"]),
            Some("project"),
        ),
        (
            "cwd 含控制字符",
            argv(&["peri", "--resume", "abc"]),
            Some("/tmp/a\u{7}b"),
        ),
        ("argv 含 C0 控制字符", argv(&["peri", "a\u{1b}b"]), None),
        ("argv 含 C1 控制字符", argv(&["peri", "a\u{9b}b"]), None),
        ("超过 64 个参数", too_many, None),
    ];

    for (case, argv, cwd) in cases {
        assert!(
            super::format_report(sender(), &argv, cwd).is_none(),
            "{case} 不应生成上报序列"
        );
    }
}

#[test]
fn sixty_four_arguments_stay_reportable() {
    let mut values = argv(&["peri"]);
    for _ in 0..MAX_ARGV_ELEMENTS - 1 {
        values.push("x".to_owned());
    }

    assert!(
        super::format_report(sender(), &values, None).is_some(),
        "64 个参数是协议上限，仍应上报"
    );
}

#[test]
fn oversized_sequence_produces_no_report() {
    let long = "a".repeat(2000);
    let values = argv(&["/usr/local/bin/peri", &long]);

    assert!(
        super::format_report(sender(), &values, None).is_none(),
        "超过 2048 字节的序列必须放弃上报"
    );
}

#[test]
fn repeated_report_of_the_same_command_writes_once() {
    let command = ResumeCommand {
        argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
        cwd: Some("/tmp".to_owned()),
    };
    let mut reported = None;
    let mut writer = TestWriter::new();
    super::emit_report(
        &mut writer,
        &mut reported,
        sender(),
        Some(&command),
        &no_env,
        true,
    )
    .expect("the first report writes one flushed sequence");
    let first = writer.bytes.clone();
    assert!(!first.is_empty());

    super::emit_report(
        &mut writer,
        &mut reported,
        sender(),
        Some(&command),
        &no_env,
        true,
    )
    .expect("an unchanged command is a no-op");

    assert_eq!(writer.bytes, first, "相同命令只应发送一次");
    assert_eq!(reported.as_ref(), Some(&command));
}

#[test]
fn changed_command_writes_another_sequence() {
    let first = ResumeCommand {
        argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
        cwd: None,
    };
    let second = ResumeCommand {
        argv: argv(&["/usr/local/bin/peri", "--resume", "def"]),
        cwd: None,
    };
    let mut reported = None;
    let mut writer = TestWriter::new();

    for command in [&first, &second] {
        super::emit_report(
            &mut writer,
            &mut reported,
            sender(),
            Some(command),
            &no_env,
            true,
        )
        .expect("each distinct command writes one flushed sequence");
    }

    let text = String::from_utf8(writer.bytes).expect("sequences are ASCII");
    assert_eq!(text.matches("\x1b]7577;").count(), 2, "命令变化必须重报");
    assert!(text.contains("YWJj"));
    assert!(text.contains("ZGVm"));
    assert_eq!(reported.as_ref(), Some(&second));
}

#[test]
fn invalid_command_after_a_report_clears_the_record() {
    let previous = ResumeCommand {
        argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
        cwd: None,
    };
    let mut reported = Some(previous);
    let mut writer = TestWriter::new();

    super::emit_report(
        &mut writer,
        &mut reported,
        sender(),
        Some(&ResumeCommand {
            argv: argv(&["foo/bar"]),
            cwd: None,
        }),
        &no_env,
        true,
    )
    .expect("a command the protocol cannot carry falls back to clear");

    let expected = super::format_clear(sender()).expect("the clear fixture has a token");
    assert_eq!(
        String::from_utf8(writer.bytes).expect("sequences are ASCII"),
        expected
    );
    assert!(reported.is_none(), "clear 之后终端不再持有命令");
}

#[test]
fn clear_without_a_previous_report_writes_nothing() {
    let mut reported = None;
    let mut writer = TestWriter::new();

    super::emit_clear(&mut writer, &mut reported, sender(), &no_env, true)
        .expect("clear without a report is a no-op");

    assert!(writer.bytes.is_empty());
}

#[test]
fn non_tty_suppresses_report_and_clear() {
    let command = ResumeCommand {
        argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
        cwd: None,
    };
    let mut reported = None;
    let mut writer = TestWriter::new();

    super::emit_report(
        &mut writer,
        &mut reported,
        sender(),
        Some(&command),
        &no_env,
        false,
    )
    .expect("a non tty writer is skipped without error");
    assert!(writer.bytes.is_empty());
    assert!(reported.is_none());

    reported = Some(command);
    super::emit_clear(&mut writer, &mut reported, sender(), &no_env, false)
        .expect("a non tty clear is skipped without error");
    assert!(writer.bytes.is_empty());
    assert!(reported.is_some(), "非 TTY 不改变已上报状态");
}

#[test]
fn env_switch_suppresses_report_and_clear() {
    for value in ["0", "off", "false", "no", " OFF "] {
        let env = move |key: &str| (key == super::ENABLE_ENV).then(|| value.to_string());
        let command = ResumeCommand {
            argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
            cwd: None,
        };
        let mut reported = None;
        let mut writer = TestWriter::new();
        super::emit_report(
            &mut writer,
            &mut reported,
            sender(),
            Some(&command),
            &env,
            true,
        )
        .expect("the env switch suppresses reporting without error");
        assert!(writer.bytes.is_empty(), "value {value:?} 应关闭上报");
        assert!(reported.is_none());

        reported = Some(command);
        super::emit_clear(&mut writer, &mut reported, sender(), &env, true)
            .expect("the env switch suppresses clearing without error");
        assert!(writer.bytes.is_empty(), "value {value:?} 应关闭清除");
    }
}

#[test]
fn unknown_env_value_keeps_reporting() {
    for value in ["1", "on", "true", "auto", ""] {
        let env = move |key: &str| (key == super::ENABLE_ENV).then(|| value.to_string());
        let mut reported = None;
        let mut writer = TestWriter::new();
        super::emit_report(
            &mut writer,
            &mut reported,
            sender(),
            Some(&ResumeCommand {
                argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
                cwd: None,
            }),
            &env,
            true,
        )
        .expect("an unknown env value keeps reporting");
        assert!(!writer.bytes.is_empty(), "value {value:?} 应保持上报");
    }
}

#[test]
fn write_failure_propagates() {
    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let error = super::emit_report(
        &mut FailingWriter,
        &mut None,
        sender(),
        Some(&ResumeCommand {
            argv: argv(&["/usr/local/bin/peri", "--resume", "abc"]),
            cwd: None,
        }),
        &no_env,
        true,
    )
    .expect_err("write failure propagates: the failing writer must report it");
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
fn resume_argv_starts_with_exe_resume_and_session() {
    let storage = argv(&["--db-path", "/tmp/threads.db"]);

    assert_eq!(
        super::resume_argv(
            "/opt/peri",
            "01a20d19-c876-7d00-b8b3-4513bedd42d0",
            &storage
        ),
        argv(&[
            "/opt/peri",
            "--resume",
            "01a20d19-c876-7d00-b8b3-4513bedd42d0",
            "--db-path",
            "/tmp/threads.db",
        ])
    );
}

#[test]
fn storage_args_keep_only_the_explicit_whitelist() {
    let args = [
        "--model",
        "sonnet",
        "--db-path",
        "/tmp/threads.db",
        "--session-store=postgres://host/db",
        "--sessionStoreEngine",
        "postgres",
        "--session-store-token-env",
        "PERI_STORE_TOKEN",
        "--sessionStore=/tmp/other.db",
        "--resume",
        "abc",
        "-p",
    ];

    assert_eq!(
        super::storage_args(args.map(std::ffi::OsString::from)),
        argv(&[
            "--db-path",
            "/tmp/threads.db",
            "--session-store",
            "postgres://host/db",
            "--sessionStoreEngine",
            "postgres",
            "--session-store-token-env",
            "PERI_STORE_TOKEN",
            "--sessionStore",
            "/tmp/other.db",
        ])
    );
}

#[test]
fn storage_args_skip_a_missing_value() {
    let args = ["--db-path", "--model", "sonnet", "--dbPath"];

    assert!(super::storage_args(args.map(std::ffi::OsString::from)).is_empty());
}

#[test]
fn clear_without_a_token_produces_no_sequence() {
    let sender = Sender {
        token: "",
        pid: PID,
        time: TIME,
    };

    assert!(super::format_clear(sender).is_none());
}

/// 在隔离的子进程里改进程环境：父进程同时跑其它测试，不能在多线程环境里 set_var。
fn isolated_environment(case: &str) -> bool {
    const CHILD: &str = "PERI_PROGRAM_RESUME_FIXTURE";
    if std::env::var(CHILD).as_deref() == Ok(case) {
        return true;
    }
    let module = module_path!()
        .split_once("::")
        .expect("test module has a crate prefix")
        .1;
    let output = std::process::Command::new(
        std::env::current_exe().expect("the running test executable remains available"),
    )
    .args([
        "--exact",
        &format!("{module}::{case}"),
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, case)
    .output()
    .expect("the isolated environment fixture must launch");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the child must execute one fixture"
    );
    false
}

#[test]
fn take_token_removes_it_from_the_process_environment() {
    if !isolated_environment("take_token_removes_it_from_the_process_environment") {
        return;
    }

    // SAFETY: 子进程只执行本用例（--test-threads=1），没有其它线程并发读写环境。
    unsafe {
        std::env::set_var(super::TOKEN_ENV, "dummy-test-token");
    }

    assert_eq!(super::take_token().as_deref(), Some("dummy-test-token"));
    assert!(
        std::env::var_os(super::TOKEN_ENV).is_none(),
        "token 必须从进程环境删除，子进程才不会继承"
    );
    assert!(
        super::take_token().is_none(),
        "重复取走 token 必须返回 None"
    );
}
