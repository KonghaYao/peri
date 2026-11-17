//! Rehydrate a bounded completion preview without retaining another full result.

use std::io::{self, Read, Write};
use std::path::Path;

use peri_acp_types::workflow::{WorkflowResultPreview, WORKFLOW_RESULT_PREVIEW_BYTES};
use serde_json::Value;

use crate::journal::WorkflowJournalStore;

struct PreviewWriter {
    bytes: Vec<u8>,
    complete: bool,
}

impl Write for PreviewWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = WORKFLOW_RESULT_PREVIEW_BYTES.saturating_sub(self.bytes.len());
        let count = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        if count != bytes.len() {
            self.complete = false;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "preview limit reached",
            ));
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn output_text(directory: &Path, label: &str, limit: usize) -> io::Result<(String, bool)> {
    if label.is_empty()
        || label.contains("..")
        || label.contains(['/', '\\'])
        || label.chars().any(char::is_control)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe output reference",
        ));
    }
    // The preview may read only regular files directly inside this run's outputs.
    let path = directory.join(format!("{label}.txt"));
    if std::fs::symlink_metadata(directory)?
        .file_type()
        .is_symlink()
        || !std::fs::symlink_metadata(&path)?.file_type().is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular output",
        ));
    }
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    let complete = bytes.len() <= limit;
    if !complete {
        bytes.truncate(limit);
        // A byte budget may split the last UTF-8 code point.
        while std::str::from_utf8(&bytes).is_err_and(|error| error.error_len().is_none()) {
            bytes.pop();
        }
    }
    String::from_utf8(bytes)
        .map(|text| (text, complete))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_value(value: &Value, directory: &Path, writer: &mut PreviewWriter) -> io::Result<()> {
    match value {
        Value::String(text) => {
            if let Some(label) = text
                .strip_prefix("${")
                .and_then(|text| text.strip_suffix('}'))
            {
                let remaining = WORKFLOW_RESULT_PREVIEW_BYTES.saturating_sub(writer.bytes.len());
                match output_text(directory, label, remaining) {
                    Ok((expanded, complete)) => {
                        writer.complete &= complete;
                        return serde_json::to_writer(writer, &expanded).map_err(io::Error::other);
                    }
                    Err(_) => writer.complete = false,
                }
            }
            serde_json::to_writer(writer, text).map_err(io::Error::other)
        }
        Value::Object(values) => {
            writer.write_all(b"{")?;
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    writer.write_all(b",")?;
                }
                serde_json::to_writer(&mut *writer, key).map_err(io::Error::other)?;
                writer.write_all(b":")?;
                write_value(value, directory, writer)?;
            }
            writer.write_all(b"}")
        }
        Value::Array(values) => {
            writer.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    writer.write_all(b",")?;
                }
                write_value(value, directory, writer)?;
            }
            writer.write_all(b"]")
        }
        _ => serde_json::to_writer(writer, value).map_err(io::Error::other),
    }
}

pub(super) fn result_preview(
    value: &Value,
    journal: &WorkflowJournalStore,
    run_id: &str,
) -> WorkflowResultPreview {
    let mut writer = PreviewWriter {
        bytes: Vec::new(),
        complete: true,
    };
    if write_value(value, &journal.run_dir(run_id).join("outputs"), &mut writer).is_err() {
        writer.complete = false;
    }
    while std::str::from_utf8(&writer.bytes).is_err_and(|error| error.error_len().is_none()) {
        writer.bytes.pop();
    }
    WorkflowResultPreview {
        text: String::from_utf8(writer.bytes).expect("JSON serialization is valid UTF-8"),
        complete: writer.complete,
    }
}

#[cfg(test)]
#[path = "preview_test.rs"]
mod tests;
