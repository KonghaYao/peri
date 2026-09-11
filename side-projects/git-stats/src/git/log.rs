//! 解析 Git 的 NUL 字段协议，提交消息与路径不参与记录边界识别。

use crate::commit::{parse_co_authors, CommitType, FileChange, ParsedCommit};

pub(super) fn parse_log_output(mut raw: &[u8]) -> Result<Vec<ParsedCommit>, String> {
    let mut commits = Vec::new();
    while !raw.is_empty() {
        raw = raw.strip_prefix(b"\0").ok_or("Missing commit boundary")?;
        let hash = field_text(take_field(&mut raw, "hash")?);
        let author_name = field_text(take_field(&mut raw, "author name")?);
        let author_email = field_text(take_field(&mut raw, "author email")?);
        let subject = field_text(take_field(&mut raw, "subject")?);
        let body = field_text(take_field(&mut raw, "body")?);
        let mut files = Vec::new();
        // numstat 字段含两个 tab；整条路径在同一 NUL 字段内。
        // 空字段只会是下一条记录的前导 NUL，不能用消息中的可见 marker 分割。
        while !raw.is_empty() && raw[0] != 0 {
            let stat = take_field(&mut raw, "numstat")?;
            let stat = stat.strip_prefix(b"\n").unwrap_or(stat);
            files.push(parse_numstat(stat)?);
        }
        commits.push(ParsedCommit {
            hash,
            author_name,
            author_email,
            commit_type: CommitType::from_subject(&subject),
            subject,
            co_authors: parse_co_authors(&body),
            files,
        });
    }
    Ok(commits)
}

fn take_field<'a>(raw: &mut &'a [u8], name: &str) -> Result<&'a [u8], String> {
    let end = raw
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| format!("Unterminated {name} field"))?;
    let field = &raw[..end];
    *raw = &raw[end + 1..];
    Ok(field)
}

fn field_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn parse_numstat(stat: &[u8]) -> Result<FileChange, String> {
    let mut fields = stat.splitn(3, |b| *b == b'\t');
    let added = fields.next().ok_or("Missing added count")?;
    let deleted = fields.next().ok_or("Missing deleted count")?;
    let path = fields.next().ok_or("Missing numstat path")?;
    if path.is_empty() {
        return Err("Empty numstat path (rename records require --no-renames)".into());
    }
    Ok(FileChange {
        added: line_count(added)?,
        deleted: line_count(deleted)?,
    })
}

fn line_count(bytes: &[u8]) -> Result<u64, String> {
    if bytes == b"-" {
        return Ok(0);
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "Invalid numstat line count".to_string())
}

#[cfg(test)]
#[path = "log_test.rs"]
mod tests;
