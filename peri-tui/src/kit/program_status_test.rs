use super::*;
use serial_test::serial;

fn no_env(_: &str) -> Option<String> {
    None
}

fn env_with(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |key| (key == name).then(|| value.to_string())
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

#[test]
fn idle_sequence_carries_state_and_app() {
    assert_eq!(
        sequence(ProgramStatus::Idle),
        "\x1b]7501;state=idle:app=peri\x1b\\"
    );
}

#[test]
fn blocked_sequence_carries_kind() {
    for (kind, expected) in [
        (BlockedKind::Permission, "permission"),
        (BlockedKind::Question, "question"),
        (BlockedKind::Auth, "auth"),
    ] {
        assert_eq!(
            sequence(ProgramStatus::Blocked(kind)),
            format!("\x1b]7501;state=blocked:kind={expected}:app=peri\x1b\\")
        );
    }
}

#[test]
fn every_terminal_state_uses_the_spec_state_name() {
    assert_eq!(
        sequence(ProgramStatus::Working),
        "\x1b]7501;state=working:app=peri\x1b\\"
    );
    assert_eq!(
        sequence(ProgramStatus::Done),
        "\x1b]7501;state=done:app=peri\x1b\\"
    );
    assert_eq!(
        sequence(ProgramStatus::Error),
        "\x1b]7501;state=error:app=peri\x1b\\"
    );
}

#[test]
fn clear_removes_the_record_without_app() {
    assert_eq!(clear_sequence(), "\x1b]7501;state=clear\x1b\\");
}

#[test]
fn emit_writes_one_flushed_sequence() {
    let mut writer = TestWriter::new();
    emit(ProgramStatus::Working, &mut writer, &no_env, true).unwrap();

    assert_eq!(writer.bytes, b"\x1b]7501;state=working:app=peri\x1b\\");
    assert_eq!(writer.flushes, 1);
}

#[test]
fn non_tty_suppresses_report_without_writing() {
    let mut writer = TestWriter::new();
    emit(ProgramStatus::Working, &mut writer, &no_env, false).unwrap();

    assert!(writer.bytes.is_empty());
    assert_eq!(writer.flushes, 0);
}

#[test]
fn env_switch_suppresses_report() {
    for value in ["0", "off", "false", "no", " OFF "] {
        let env = move |key: &str| (key == "PERI_PROGRAM_STATUS").then(|| value.to_string());
        let mut writer = TestWriter::new();
        emit(ProgramStatus::Done, &mut writer, &env, true).unwrap();
        assert!(writer.bytes.is_empty(), "value {value:?} 应关闭上报");
    }
}

#[test]
fn unknown_env_value_keeps_reporting() {
    for value in ["1", "on", "true", "auto"] {
        let env = move |key: &str| (key == "PERI_PROGRAM_STATUS").then(|| value.to_string());
        let mut writer = TestWriter::new();
        emit(ProgramStatus::Done, &mut writer, &env, true).unwrap();
        assert!(!writer.bytes.is_empty(), "value {value:?} 应保持上报");
    }
}

#[test]
fn clear_respects_the_same_guards() {
    let mut writer = TestWriter::new();
    emit_clear(&mut writer, &no_env, true).unwrap();
    assert_eq!(writer.bytes, b"\x1b]7501;state=clear\x1b\\");

    let mut writer = TestWriter::new();
    emit_clear(&mut writer, &env_with("PERI_PROGRAM_STATUS", "off"), true).unwrap();
    assert!(writer.bytes.is_empty());
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

    let error = emit(ProgramStatus::Working, &mut FailingWriter, &no_env, true).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
#[serial]
fn publish_updates_atom_only_on_change() {
    use crate::kit::atoms;

    atoms::PROGRAM_STATUS.set(ProgramStatus::Idle);
    publish(ProgramStatus::Working);
    assert_eq!(
        *atoms::PROGRAM_STATUS.state().read(),
        ProgramStatus::Working
    );

    publish(ProgramStatus::Working);
    assert_eq!(
        *atoms::PROGRAM_STATUS.state().read(),
        ProgramStatus::Working
    );

    publish(ProgramStatus::Blocked(BlockedKind::Permission));
    assert_eq!(
        *atoms::PROGRAM_STATUS.state().read(),
        ProgramStatus::Blocked(BlockedKind::Permission)
    );
}
