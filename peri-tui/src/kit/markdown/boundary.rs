#[cfg(test)]
thread_local! {
    pub(super) static SCANNED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn stable_end(input: &str, start: usize) -> usize {
    let mut end = start;
    let mut offset = start;
    let mut fence = None;
    let mut backtick_parity = false;
    for line in input[start..].split_inclusive('\n') {
        #[cfg(test)]
        SCANNED_BYTES.with(|count| count.set(count.get() + line.len()));
        offset += line.len();
        if !line.ends_with('\n') {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            backtick_parity = !backtick_parity;
        }
        if let Some((marker, length)) = fence {
            let run = trimmed.bytes().take_while(|byte| *byte == marker).count();
            if run >= length
                && trimmed[run..].is_empty()
                && line.len() - line.trim_start_matches(' ').len() <= 3
                && line.trim_start_matches(' ').as_bytes().first() == Some(&marker)
            {
                fence = None;
            }
            continue;
        }
        if line.starts_with("    ") || line.trim_start_matches(' ').starts_with('\t') {
            break;
        }
        if let Some(marker @ (b'`' | b'~')) = trimmed.bytes().next() {
            let length = trimmed.bytes().take_while(|byte| *byte == marker).count();
            if length >= 3 {
                if marker == b'`' && trimmed[length..].contains('`') {
                    break;
                }
                fence = Some((marker, length));
                continue;
            }
        }
        if trimmed.contains('[')
            || trimmed.contains('|')
            || trimmed.starts_with('>')
            || trimmed.starts_with('<')
            || trimmed
                .strip_prefix(['-', '*', '+'])
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            || trimmed
                .split_once(['.', ')'])
                .is_some_and(|(number, rest)| {
                    !number.is_empty()
                        && number.bytes().all(|byte| byte.is_ascii_digit())
                        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                })
        {
            break;
        }
        if trimmed.is_empty() && !backtick_parity {
            end = offset;
        }
    }
    end
}
