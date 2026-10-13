use std::cell::RefCell;
use std::io::{self, Write};

use base64::Engine;

use super::{Channel, Multiplexer, copy_with, select_channel, write_osc52, write_terminal};

/// 环境读取面替身：只认给出的键值对。
fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    move |name: &str| {
        owned
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn display_environment_uses_native_clipboard() {
    // 本机有显示环境：系统剪贴板即用户面前的剪贴板（macOS 无需 DISPLAY，占位值无影响）。
    assert_eq!(
        select_channel(&env_of(&[("DISPLAY", ":0")])),
        Channel::Native
    );
    assert_eq!(
        select_channel(&env_of(&[("WAYLAND_DISPLAY", "wayland-0")])),
        Channel::Native
    );
}

#[test]
fn ssh_and_herdr_use_terminal_clipboard() {
    for variable in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "HERDR_ENV"] {
        assert_eq!(
            select_channel(&env_of(&[(variable, "1")])),
            Channel::Terminal,
            "{variable}"
        );
    }
    assert_eq!(
        select_channel(&env_of(&[
            ("SSH_CONNECTION", "10.0.0.1 1 10.0.0.2 2"),
            ("SSH_TTY", "/dev/pts/0"),
            ("DISPLAY", "localhost:10.0"),
        ])),
        Channel::Terminal
    );
}

#[test]
fn unix_without_display_uses_terminal_clipboard() {
    // arboard 在非 macOS 的 Unix 上需要 X11/Wayland；两者皆无时本机剪贴板必然写不进去。
    let expected = if cfg!(all(unix, not(target_os = "macos"))) {
        Channel::Terminal
    } else {
        Channel::Native
    };
    assert_eq!(select_channel(&env_of(&[])), expected);
}

#[test]
fn local_multiplexer_keeps_native_when_display_available() {
    // tmux/screen 可能跑在本机：本机有显示环境时系统剪贴板更可靠，不必绕 OSC 52。
    assert_eq!(
        select_channel(&env_of(&[
            ("TMUX", "/tmp/tmux-501/default,123,0"),
            ("DISPLAY", ":0")
        ])),
        Channel::Native
    );
    if cfg!(all(unix, not(target_os = "macos"))) {
        let expected = Channel::Terminal;
        assert_eq!(
            select_channel(&env_of(&[("TMUX", "/tmp/tmux-501/default,123,0")])),
            expected
        );
    }
}

#[test]
fn peri_clipboard_overrides_detection() {
    let ssh = [("SSH_TTY", "/dev/pts/0")];
    let local = [("DISPLAY", ":0")];

    for value in ["native", "NATIVE", " native "] {
        assert_eq!(
            select_channel(&env_of(&[ssh[0], ("PERI_CLIPBOARD", value)])),
            Channel::Native,
            "{value}"
        );
    }
    for value in ["osc52", "OSC52", "terminal", " terminal "] {
        assert_eq!(
            select_channel(&env_of(&[local[0], ("PERI_CLIPBOARD", value)])),
            Channel::Terminal,
            "{value}"
        );
    }
    // 未知取值与 `auto` 等同：交给探测。
    for value in ["auto", "", "native-please"] {
        assert_eq!(
            select_channel(&env_of(&[ssh[0], ("PERI_CLIPBOARD", value)])),
            Channel::Terminal,
            "{value}"
        );
    }
}

#[test]
fn osc52_preserves_unicode_newlines_and_encodes_control_characters() {
    let text = "你好 👋\nsecond line\x1b]52;c;evil\x07";
    let mut output = Vec::new();
    write_osc52(&mut output, text).unwrap();
    assert!(output.starts_with(b"\x1b]52;c;"));
    assert!(output.ends_with(b"\x07"));
    let payload = &output[7..output.len() - 1];
    assert!(!payload.contains(&0x1b));
    assert!(!payload.contains(&0x07));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap(),
        text.as_bytes()
    );
}

#[test]
fn osc52_emits_standard_sequence_without_multiplexer_wrapping() {
    let mut output = Vec::new();
    write_terminal(&mut output, "hello", Multiplexer::None).unwrap();
    assert_eq!(output, b"\x1b]52;c;aGVsbG8=\x07");
}

#[test]
fn tmux_emits_native_sequence_plus_dcs_passthrough_copy() {
    let mut output = Vec::new();
    write_terminal(&mut output, "hello", Multiplexer::Tmux).unwrap();
    // 原生 OSC 52（tmux `set-clipboard on` + 客户端 terminfo `Ms`）在先，
    // 透传副本（tmux `allow-passthrough on`）在后；内层 ESC 不双写。
    assert_eq!(
        output,
        b"\x1b]52;c;aGVsbG8=\x07\x1bPtmux;\x1b]52;c;aGVsbG8=\x07\x1b\\"
    );
}

struct ClipboardWriter {
    bytes: Vec<u8>,
    fail_write: bool,
    fail_flush: bool,
    flushed: bool,
}

impl ClipboardWriter {
    fn working() -> Self {
        Self {
            bytes: Vec::new(),
            fail_write: false,
            fail_flush: false,
            flushed: false,
        }
    }

    fn broken() -> Self {
        Self {
            bytes: Vec::new(),
            fail_write: true,
            fail_flush: false,
            flushed: false,
        }
    }
}

impl Write for ClipboardWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let written = bytes.len().min(3);
        self.bytes.extend_from_slice(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushed = true;
        if self.fail_flush {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn osc52_handles_partial_writes_and_flushes_output() {
    let mut writer = ClipboardWriter::working();
    write_osc52(&mut writer, "hello").unwrap();
    assert_eq!(writer.bytes, b"\x1b]52;c;aGVsbG8=\x07");
    assert!(writer.flushed);
}

#[test]
fn osc52_propagates_write_and_flush_failures() {
    for fail_write in [true, false] {
        let mut writer = ClipboardWriter {
            bytes: Vec::new(),
            fail_write,
            fail_flush: !fail_write,
            flushed: false,
        };
        assert_eq!(
            write_osc52(&mut writer, "hello").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(writer.flushed, !fail_write);
    }
}

#[test]
fn native_channel_success_writes_no_terminal_sequence() {
    let mut output = Vec::new();
    copy_with(
        "hello",
        Channel::Native,
        Multiplexer::None,
        &mut output,
        |_| Ok(()),
    )
    .unwrap();
    assert!(output.is_empty());
}

#[test]
fn native_failure_falls_back_to_terminal() {
    let mut output = Vec::new();
    copy_with(
        "hello",
        Channel::Native,
        Multiplexer::None,
        &mut output,
        |_| Err(anyhow::anyhow!("native boom")),
    )
    .unwrap();
    assert_eq!(output, b"\x1b]52;c;aGVsbG8=\x07");
}

#[test]
fn terminal_failure_falls_back_to_native() {
    let mut writer = ClipboardWriter::broken();
    let native_text = RefCell::new(Vec::new());
    copy_with(
        "hello",
        Channel::Terminal,
        Multiplexer::None,
        &mut writer,
        |text: &str| {
            native_text.borrow_mut().push(text.to_string());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(native_text.borrow().as_slice(), ["hello"]);
}

#[test]
fn both_channels_failing_reports_primary_cause_and_fallback() {
    let mut writer = ClipboardWriter::broken();
    let error = copy_with(
        "hello",
        Channel::Native,
        Multiplexer::None,
        &mut writer,
        |_| Err(anyhow::anyhow!("native boom")),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("native boom"), "{message}");
    assert!(message.contains("Terminal failed"), "{message}");
    assert!(
        message.contains("Failed to send terminal clipboard write"),
        "{message}"
    );
}
