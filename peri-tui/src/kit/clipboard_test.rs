use std::io::{self, Write};

use base64::Engine;

use super::{ClipboardBackend, backend_for, write_osc52};

#[test]
fn local_copy_uses_native_clipboard() {
    assert_eq!(backend_for(|_| false), ClipboardBackend::Native);
}

#[test]
fn ssh_and_multiplexers_use_terminal_clipboard() {
    for variable in [
        "SSH_CONNECTION",
        "SSH_CLIENT",
        "SSH_TTY",
        "TMUX",
        "HERDR_ENV",
    ] {
        assert_eq!(
            backend_for(|name| name == variable),
            ClipboardBackend::Terminal,
            "{variable}"
        );
    }
    assert_eq!(
        backend_for(|name| matches!(name, "SSH_CONNECTION" | "TMUX" | "HERDR_ENV")),
        ClipboardBackend::Terminal
    );
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
    write_osc52(&mut output, "hello").unwrap();
    assert_eq!(output, b"\x1b]52;c;aGVsbG8=\x07");
}

struct ClipboardWriter {
    bytes: Vec<u8>,
    fail_write: bool,
    fail_flush: bool,
    flushed: bool,
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
    let mut writer = ClipboardWriter {
        bytes: Vec::new(),
        fail_write: false,
        fail_flush: false,
        flushed: false,
    };
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
