use super::*;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};

struct RepositoryFixture {
    root: PathBuf,
}

impl RepositoryFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let path = std::env::temp_dir().join(format!(
                "git-stats-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("无法创建隔离测试目录: {e}"),
            }
        };
        let fixture = Self { root };
        fixture.run(
            &["init", "--quiet"],
            None,
            "Fixture",
            "fixture@example.test",
            "2020-01-01T12:00:00Z",
        );
        fixture
    }

    fn isolate(&self, command: &mut Command) {
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("absent-global-config"))
            .env("GIT_TERMINAL_PROMPT", "0");
    }

    fn run(&self, args: &[&str], input: Option<&str>, author: &str, email: &str, date: &str) {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.root)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(args);
        self.isolate(&mut command);
        command
            .env("GIT_AUTHOR_NAME", author)
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("测试需要安装 Git");
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "Git fixture 命令失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit(&self, message: &str, author: &str, email: &str, date: &str) {
        self.run(&["add", "--all"], None, author, email, date);
        self.run(
            &["commit", "--quiet", "--allow-empty", "--file=-"],
            Some(message),
            author,
            email,
            date,
        );
    }

    fn fetch(&self) -> Vec<ParsedCommit> {
        let mut command =
            build_git_command(self.root.to_str().unwrap(), "2019-12-31", "2020-02-01");
        self.isolate(&mut command);
        let output = command.output().expect("应能读取真实仓库日志");
        assert!(
            output.status.success(),
            "git log 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        log::parse_log_output(&output.stdout).expect("真实 Git framing 应能解析")
    }
}

impl Drop for RepositoryFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// [回归测试] 外层 @@COMMIT@@ 曾把 subject/body/path 中的文字拆成伪提交。
#[test]
fn test_real_git_markers_and_path_delimiters_preserve_commits_and_attribution() {
    let repo = RepositoryFixture::new();
    repo.commit(
        "chore: empty root\n",
        "Older",
        "older@example.test",
        "2020-01-01T12:00:00Z",
    );
    let filename = if cfg!(unix) {
        "@@COMMIT@@\t@@NUMSTAT@@\nfile.txt"
    } else {
        "@@COMMIT@@-@@NUMSTAT@@.txt"
    };
    fs::write(repo.root.join(filename), "line one\nline two\n").unwrap();
    fs::write(repo.root.join("image.bin"), b"\0binary\0").unwrap();
    repo.commit(
        "feat: preserve @@COMMIT@@ subject\n\n@@COMMIT@@\n@@NUMSTAT@@\n\nCo-Authored-By: Pair Before <before@example.test>\nMore @@COMMIT@@ content\nCo-Authored-By: Pair After <after@example.test>\n",
        "Current", "current@example.test", "2020-01-02T12:00:00Z"
    );
    let commits = repo.fetch();
    assert_eq!(commits.len(), 2, "标记文本不能新增或截断提交");
    assert_eq!(commits[0].subject, "feat: preserve @@COMMIT@@ subject");
    assert_eq!(commits[0].commit_type, crate::commit::CommitType::Feat);
    assert_eq!(
        commits[0]
            .co_authors
            .iter()
            .map(|c| c.email.as_str())
            .collect::<Vec<_>>(),
        vec!["before@example.test", "after@example.test"],
        "marker 两侧 trailer 均需保留"
    );
    assert_eq!(commits[0].files.len(), 2, "特殊路径与二进制文件各计一次");
    assert_eq!(commits[0].files.iter().map(|f| f.added).sum::<u64>(), 2);
    assert_eq!(commits[0].files.iter().map(|f| f.deleted).sum::<u64>(), 0);
    assert!(commits[1].files.is_empty(), "空提交不应借用相邻 numstat");
    let stats = crate::analysis::aggregate(&commits);
    for email in [
        "current@example.test",
        "before@example.test",
        "after@example.test",
    ] {
        let person = stats.iter().find(|s| s.email == email).unwrap();
        assert_eq!(
            (person.commits, person.added_lines, person.files_touched),
            (1, 2, 2)
        );
    }
}

/// [回归测试] newest-first 的日志迭代不能用较旧作者名覆盖最新展示名。
#[test]
fn test_real_git_newest_author_and_coauthor_names_win_for_same_email() {
    let repo = RepositoryFixture::new();
    fs::write(repo.root.join("file.txt"), "old\n").unwrap();
    repo.commit(
        "feat: old\n\nCo-Authored-By: Old Pair <pair@example.test>\n",
        "Old Name",
        "same@example.test",
        "2020-01-01T12:00:00Z",
    );
    fs::write(repo.root.join("file.txt"), "old\nnew\n").unwrap();
    repo.commit(
        "fix: new\n\nCo-Authored-By: New Pair <pair@example.test>\n",
        "New Name",
        "same@example.test",
        "2020-01-02T12:00:00Z",
    );
    let commits = repo.fetch();
    assert_eq!(
        commits
            .iter()
            .map(|c| c.author_name.as_str())
            .collect::<Vec<_>>(),
        vec!["New Name", "Old Name"]
    );
    let stats = crate::analysis::aggregate(&commits);
    for (email, name) in [
        ("same@example.test", "New Name"),
        ("pair@example.test", "New Pair"),
    ] {
        let person = stats.iter().find(|s| s.email == email).unwrap();
        assert_eq!(person.name, name, "同一email展示名取最新记录");
        assert_eq!(
            (person.commits, person.feat, person.fix, person.added_lines),
            (2, 1, 1, 2),
            "名称选择不能改变归属与统计"
        );
    }
}
