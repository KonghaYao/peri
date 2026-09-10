//! Git 进程边界；NUL framing 的解析由私有 log 模块负责。

mod log;

use std::process::Command;

use crate::commit::ParsedCommit;

/// Build the `git log` command with time window and byte-delimited records.
fn build_git_command(repo: &str, since: &str, until: &str) -> Command {
    let mut cmd = Command::new("git");
    cmd.args([
        "-C",
        repo,
        "log",
        &format!("--since={}", since),
        &format!("--until={}", until),
        // 每条记录以 NUL 开始，五个元数据字段分别以 NUL 结束。
        // -z 同时让 numstat 的完整路径以 NUL 结束，路径内 tab/newline 不分段。
        "--format=%x00%H%x00%an%x00%ae%x00%s%x00%b",
        "--numstat",
        "-z",
        "--no-merges",
        "--no-renames",
    ]);
    cmd
}

/// Run git log and parse output into Vec<ParsedCommit>, in git log's newest-first order.
pub fn fetch_commits(repo: &str, since: &str, until: &str) -> Result<Vec<ParsedCommit>, String> {
    let output = build_git_command(repo, since, until)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git log failed: {}", stderr));
    }
    log::parse_log_output(&output.stdout)
}

#[cfg(test)]
#[path = "git_test.rs"]
mod tests;
