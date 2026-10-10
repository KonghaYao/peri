use std::io::{self, Write};

use crate::{IngestionEvent, LangfuseError};

struct ByteCounter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes);
        if buffer.len() > remaining {
            self.exceeded = true;
            return Err(io::Error::other("event byte budget exceeded"));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn event_bytes(event: &IngestionEvent, limit: usize) -> Result<usize, LangfuseError> {
    let mut counter = ByteCounter {
        bytes: 0,
        limit,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut counter, event);
    if counter.exceeded {
        return Err(LangfuseError::PayloadTooLarge { limit_bytes: limit });
    }
    result?;
    Ok(counter.bytes)
}
