//! Git ref 采样与 Info 文案（纯逻辑，可单测）。

use std::time::Duration;

/// 监视快照：仅分支 + HEAD（不跟踪 working tree）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSnapshot {
    pub branch: String,
    pub head: String,
}

/// 采样结果（异步 git 子进程解析后）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleOutcome {
    Repository(GitSnapshot),
    NotRepository,
    Failed,
}

/// 与上次快照比较，生成 Info 正文；无变化或尚无基线时返回 `None`。
pub fn info_message_if_changed(
    previous: Option<&GitSnapshot>,
    current: &GitSnapshot,
) -> Option<String> {
    let prev = previous?;
    if prev == current {
        return None;
    }

    let mut lines = vec!["[Git watch] Repository ref changed since the last sample:".to_string()];

    if prev.branch != current.branch {
        lines.push(format!("- Branch: {} → {}", prev.branch, current.branch));
    }
    if prev.head != current.head {
        lines.push(format!(
            "- HEAD: {} → {}",
            short_hash(&prev.head),
            short_hash(&current.head)
        ));
    }

    lines.push(String::new());
    lines.push(
        "Sampled after a tool run or turn start. Run `git status` and `git log -1` before irreversible git operations.".to_string(),
    );

    Some(lines.join("\n"))
}

pub fn short_hash(full: &str) -> String {
    let trimmed = full.trim();
    if trimmed.len() <= 7 {
        trimmed.to_string()
    } else {
        trimmed[..7].to_string()
    }
}

/// 解析 `git rev-parse --is-inside-work-tree` + HEAD + branch 的合并输出。
pub fn parse_sample_stdout(stdout: &str) -> SampleOutcome {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(work_tree) = lines.next() else {
        return SampleOutcome::Failed;
    };
    if work_tree != "true" {
        return SampleOutcome::NotRepository;
    }
    let Some(head) = lines.next() else {
        return SampleOutcome::Failed;
    };
    let Some(branch) = lines.next() else {
        return SampleOutcome::Failed;
    };
    SampleOutcome::Repository(GitSnapshot {
        branch: branch.to_string(),
        head: head.to_string(),
    })
}

pub const GIT_WATCH_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);
pub const GIT_WATCH_THROTTLE: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_repo_output() {
        let out = "true\nabc123def456789\nmain\n";
        match parse_sample_stdout(out) {
            SampleOutcome::Repository(s) => {
                assert_eq!(s.branch, "main");
                assert_eq!(s.head, "abc123def456789");
            }
            other => panic!("expected Repository, got {other:?}"),
        }
    }

    #[test]
    fn parse_not_a_repo() {
        assert_eq!(parse_sample_stdout("false\n"), SampleOutcome::NotRepository);
    }

    #[test]
    fn info_only_on_branch_or_head_change() {
        let a = GitSnapshot {
            branch: "main".into(),
            head: "a".repeat(40),
        };
        assert!(info_message_if_changed(None, &a).is_none());
        let b = GitSnapshot {
            branch: "dev".into(),
            head: a.head.clone(),
        };
        let msg = info_message_if_changed(Some(&a), &b).unwrap();
        assert!(msg.contains("Branch: main → dev"));
        assert!(!msg.contains("HEAD:"));
    }
}
