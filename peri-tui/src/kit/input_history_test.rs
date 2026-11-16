//! Tests for input_history

use super::HISTORY_DRAFT as DRAFT;
use super::HISTORY_DRAFT;
use super::INPUT_HISTORY;
use super::INPUT_HISTORY_INDEX;
use super::MAX_ENTRY_BYTES;
use super::MAX_HISTORY;
use super::MAX_HISTORY_BYTES;
use super::MAX_HISTORY_FILE_BYTES;
use super::SAVE_DEBOUNCE;

use serial_test::serial;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 父进程在子测试退出前持有临时 HOME；后台写线程只访问该目录。
struct IsolatedHome {
    path: PathBuf,
}

impl IsolatedHome {
    #[must_use]
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

const HISTORY_CHILD: &str = "PERI_INPUT_HISTORY_CHILD";

/// 在启动测试进程前固定 HOME，避免与任何线程并发修改进程环境。
fn run_isolated(name: &str, scenario: fn()) {
    if std::env::var(HISTORY_CHILD).as_deref() == Ok(name) {
        scenario();
        return;
    }
    let home = tempfile::tempdir().expect("history fixture requires a temporary HOME");
    let test_module = module_path!()
        .split_once("::")
        .expect("history fixture module has a crate prefix")
        .1;
    let selector = format!("{test_module}::{name}");
    let output = std::process::Command::new(
        std::env::current_exe().expect("history fixture must locate its test executable"),
    )
    .args(["--exact", &selector, "--nocapture", "--test-threads=1"])
    .env(HISTORY_CHILD, name)
    .env("HOME", home.path())
    .env("USERPROFILE", home.path())
    .output()
    .expect("history fixture must start its isolated child");
    assert!(
        output.status.success(),
        "{selector} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the exact history selector must execute one test: {selector}",
    );
}

/// 重置 atom；HOME 由父进程在创建这个独立测试进程前配置。
fn setup() -> IsolatedHome {
    assert!(
        std::env::var_os(HISTORY_CHILD).is_some(),
        "history fixture must run in its isolated child"
    );
    let path =
        PathBuf::from(std::env::var_os("HOME").expect("history child has its isolated HOME"));
    crate::kit::atoms::init_atoms();
    *INPUT_HISTORY.state().write() = VecDeque::new();
    *INPUT_HISTORY_INDEX.state().write() = None;
    *HISTORY_DRAFT.state().write() = None;
    IsolatedHome { path }
}

#[test]
#[serial]
fn test_push_history_grows_stack() {
    run_isolated("test_push_history_grows_stack", || {
        let _home = setup();
        super::push_history("hello");
        super::push_history("world");
        assert_eq!(INPUT_HISTORY.state().read().len(), 2);
    });
}

#[test]
#[serial]
fn test_push_history_ignores_empty() {
    run_isolated("test_push_history_ignores_empty", || {
        let _home = setup();
        super::push_history("   ");
        super::push_history("");
        assert_eq!(INPUT_HISTORY.state().read().len(), 0);
    });
}

#[test]
#[serial]
fn test_push_history_dedup_consecutive() {
    run_isolated("test_push_history_dedup_consecutive", || {
        let _home = setup();
        super::push_history("hello");
        super::push_history("hello"); // 重复不入栈
        assert_eq!(INPUT_HISTORY.state().read().len(), 1);
    });
}

#[test]
#[serial]
fn test_push_history_trims_whitespace() {
    run_isolated("test_push_history_trims_whitespace", || {
        let _home = setup();
        super::push_history("  hello  ");
        assert_eq!(INPUT_HISTORY.state().read().len(), 1);
        let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
        assert_eq!(stored[0], "hello");
    });
}

#[test]
#[serial]
fn test_history_up_navigation() {
    run_isolated("test_history_up_navigation", || {
        let _home = setup();
        super::push_history("first");
        super::push_history("second");
        super::push_history("third");

        // 第一次 Up → 最新（third）
        assert_eq!(super::history_up(None).as_deref(), Some("third"));
        // 第二次 Up → second
        assert_eq!(super::history_up(None).as_deref(), Some("second"));
        // 第三次 Up → first
        assert_eq!(super::history_up(None).as_deref(), Some("first"));
        // 第四次 Up → 已在最旧，保持 first
        assert_eq!(super::history_up(None).as_deref(), Some("first"));
    });
}

#[test]
#[serial]
fn test_history_down_restores_empty_draft_at_bottom() {
    run_isolated("test_history_down_restores_empty_draft_at_bottom", || {
        let _home = setup();
        super::push_history("a");
        super::push_history("b");

        super::history_up(Some("")); // → b，并保存空草稿
        super::history_up(None); // → a
        assert_eq!(super::history_down().as_deref(), Some("b"));
        assert_eq!(super::history_down().as_deref(), Some(""));
        assert_eq!(*INPUT_HISTORY_INDEX.state().read(), None);
    });
}

#[test]
#[serial]
fn test_reset_history_cursor() {
    run_isolated("test_reset_history_cursor", || {
        let _home = setup();
        super::push_history("x");
        super::history_up(None);
        assert!(INPUT_HISTORY_INDEX.state().read().is_some());
        super::reset_history_cursor();
        assert!(INPUT_HISTORY_INDEX.state().read().is_none());
    });
}

#[test]
#[serial]
fn test_history_up_empty_stack() {
    run_isolated("test_history_up_empty_stack", || {
        let _home = setup();
        assert_eq!(super::history_up(None), None);
        assert_eq!(super::history_down(), None);
    });
}

#[test]
#[serial]
fn test_max_history_capacity() {
    run_isolated("test_max_history_capacity", || {
        let _home = setup();
        for i in 0..1050 {
            super::push_history(&format!("cmd-{}", i));
        }
        assert_eq!(INPUT_HISTORY.state().read().len(), MAX_HISTORY);
        let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
        assert_eq!(stored.front().map(String::as_str), Some("cmd-50"));
        assert_eq!(stored.back().map(String::as_str), Some("cmd-1049"));
    });
}

#[test]
#[serial]
fn test_draft_saved_on_history_entry_and_restored() {
    run_isolated("test_draft_saved_on_history_entry_and_restored", || {
        let _home = setup();
        super::push_history("old");
        assert_eq!(
            super::history_up(Some("current draft")).as_deref(),
            Some("old")
        );
        assert_eq!(DRAFT.state().read().as_deref(), Some("current draft"));
        assert_eq!(super::history_down(), Some("current draft".to_string()));
        assert!(DRAFT.state().read().is_none());
    });
}

#[test]
#[serial]
fn test_draft_cleared_on_reset() {
    run_isolated("test_draft_cleared_on_reset", || {
        let _home = setup();
        super::push_history("x");
        super::history_up(Some("draft"));
        assert!(DRAFT.state().read().is_some());
        super::reset_history_cursor();
        assert!(DRAFT.state().read().is_none());
    });
}

#[test]
#[serial]
fn test_empty_draft_is_preserved_for_restore() {
    run_isolated("test_empty_draft_is_preserved_for_restore", || {
        let _home = setup();
        super::push_history("old");
        super::history_up(Some(""));
        // 空草稿也要保存，否则 Down 回到底部时无法清空旧历史项。
        assert_eq!(DRAFT.state().read().as_deref(), Some(""));
    });
}

/// L11：`save_history` 在 `std::thread::spawn` 中异步写盘，push_history 立即返回。
///
/// 父进程通过 `Command.env` 为子测试配置临时 HOME；push 后等后台线程 rename 完成，
/// 读取独占目录里的 `~/.peri/input-history.json` 断言内容已落盘。
#[test]
#[serial]
fn test_save_history_does_not_block_and_persists_async() {
    run_isolated(
        "test_save_history_does_not_block_and_persists_async",
        || {
            // 父进程已为本子测试配置独占 HOME。
            let home = setup();

            // 用唯一 marker 避免与其他测试串扰。
            let marker = "async-persist-marker-7E3A";
            super::push_history(marker);

            // spawn 线程 rename 通常 <10ms，给一个宽松的等待窗口。
            let history_path = home.path().join(".peri").join("input-history.json");
            let mut waited_ms = 0u64;
            let content = loop {
                if let Ok(c) = std::fs::read_to_string(&history_path) {
                    break c;
                }
                if waited_ms >= 1000 {
                    panic!(
                        "input_history async persist 未在 1s 内落盘: {:?}",
                        history_path
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                waited_ms += 10;
            };

            assert!(
                content.contains(marker),
                "持久化文件应包含 marker {:?}，实际内容：{}",
                marker,
                content
            );
        },
    );
}

fn history_file(home: &IsolatedHome) -> PathBuf {
    home.path().join(".peri").join("input-history.json")
}

fn write_history_entries(home: &IsolatedHome, entries: &[String]) {
    let path = history_file(home);
    std::fs::create_dir_all(
        path.parent().expect(
            "write history entries: the fixture path must have its expected parent directory",
        ),
    )
    .expect("write history entries: isolated fixture workspace must remain accessible");
    std::fs::write(
        &path,
        serde_json::to_string(entries)
            .expect("write history entries: fixture must serialize to JSON"),
    )
    .expect("write history entries: fixture must serialize to JSON");
}

/// 等异步 writer 落盘到指定条目数，返回解析后的内容。
fn wait_for_history_file(
    home: &IsolatedHome,
    expected_entries: usize,
    timeout: Duration,
) -> Vec<String> {
    let path = history_file(home);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(entries) = serde_json::from_str::<Vec<String>>(&content)
            && entries.len() == expected_entries
        {
            return entries;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "history file 未在 {timeout:?} 内落盘到 {expected_entries} 条: {:?}",
            path
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn history_bytes(history: &VecDeque<String>) -> usize {
    history.iter().map(|entry| entry.len()).sum()
}

fn history_snapshot() -> VecDeque<String> {
    INPUT_HISTORY.state().read().clone()
}

/// 报告场景：1000 条 × 10000 CJK。条数预算不再是唯一上限，总字节必须平台化。
#[test]
#[serial]
fn test_history_bytes_are_bounded_by_budget() {
    run_isolated("test_history_bytes_are_bounded_by_budget", || {
        let _home = setup();
        let payload = "界".repeat(10_000); // 每条 30_000 bytes
        for i in 0..1000 {
            super::push_history(&format!("{i:04}{payload}"));
        }

        let history = history_snapshot();
        assert!(
            history_bytes(&history) <= MAX_HISTORY_BYTES,
            "总字节 {} 超过预算 {}",
            history_bytes(&history),
            MAX_HISTORY_BYTES
        );
        // 字节预算先于条数预算生效；每条约 30_004 bytes，应保留约 139 条。
        assert!(
            history.len() < MAX_HISTORY,
            "字节预算未生效: {}",
            history.len()
        );
        assert!(history.len() >= MAX_HISTORY_BYTES / 30_008 - 1);
        assert!(
        history
            .back()
            .expect(
                "history bytes are bounded by budget: the back fixture operation uses valid inputs"
            )
            .starts_with("0999")
    );
        assert!(
        !history
            .front()
            .expect(
                "history bytes are bounded by budget: the front fixture operation uses valid inputs"
            )
            .starts_with("0000"),
        "超限时必须淘汰最旧条目"
    );
    });
}

/// 单条超过 `MAX_ENTRY_BYTES` 不保留，但后续提交仍正常入栈。
#[test]
#[serial]
fn test_oversized_entry_is_not_retained() {
    run_isolated("test_oversized_entry_is_not_retained", || {
        let _home = setup();
        super::push_history("small");
        *INPUT_HISTORY_INDEX.state().write() = Some(0);

        super::push_history(&"x".repeat(MAX_ENTRY_BYTES + 1));

        let history = history_snapshot();
        assert_eq!(history.len(), 1);
        assert_eq!(history.back().map(String::as_str), Some("small"));
        assert!(INPUT_HISTORY_INDEX.state().read().is_none());
    });
}

/// 浏览只返回选中项，且不改变栈内容（大条目场景下走同一路径）。
#[test]
#[serial]
fn test_navigation_returns_selected_entry_without_mutating_stack() {
    run_isolated(
        "test_navigation_returns_selected_entry_without_mutating_stack",
        || {
            let _home = setup();
            let older = format!("older-{}", "字".repeat(5_000));
            let newer = format!("newer-{}", "字".repeat(5_000));
            super::push_history(&older);
            super::push_history(&newer);
            let before = INPUT_HISTORY.state().read().clone();

            assert_eq!(super::history_up(None).as_deref(), Some(newer.as_str()));
            assert_eq!(super::history_up(None).as_deref(), Some(older.as_str()));
            assert_eq!(super::history_up(None).as_deref(), Some(older.as_str()));
            assert_eq!(super::history_down().as_deref(), Some(newer.as_str()));

            let after = history_snapshot();
            assert_eq!(before.len(), after.len());
            assert!(before.iter().eq(after.iter()), "浏览不得改变历史栈");
        },
    );
}

/// 启动加载按条数预算裁剪遗留文件，保持「淘汰最旧」语义。
#[test]
#[serial]
fn test_load_history_applies_count_budget() {
    run_isolated("test_load_history_applies_count_budget", || {
        let home = setup();
        let entries: Vec<String> = (0..1050).map(|i| format!("cmd-{i}")).collect();
        write_history_entries(&home, &entries);

        super::load_history();

        let history = history_snapshot();
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history.front().map(String::as_str), Some("cmd-50"));
        assert_eq!(history.back().map(String::as_str), Some("cmd-1049"));
    });
}

/// 启动加载按总字节预算裁剪（100 × 50 KiB ≈ 5 MiB，保留尾部 ≤ 4 MiB）。
#[test]
#[serial]
fn test_load_history_applies_byte_budget() {
    run_isolated("test_load_history_applies_byte_budget", || {
        let home = setup();
        let entries: Vec<String> = (0..100)
            .map(|i| format!("{i:03}-{}", "x".repeat(50 * 1024)))
            .collect();
        write_history_entries(&home, &entries);

        super::load_history();

        let history = history_snapshot();
        assert!(history_bytes(&history) <= MAX_HISTORY_BYTES);
        assert!(history.len() < entries.len());
        assert!(
            history
                .back()
                .expect(
                    "load history applies byte budget: the back fixture operation uses valid inputs"
                )
                .starts_with("099-")
        );
        assert!(
        !history
            .front()
            .expect(
                "load history applies byte budget: the front fixture operation uses valid inputs"
            )
            .starts_with("000-")
    );
    });
}

/// 最坏 JSON 转义闭环：64 × 64 KiB U+0000（载荷恰满 4 MiB，每字节转义为 6 字节）
/// 由 writer 落盘后，`load_history` 必须完整读回。旧 3 倍文件门会拒绝 ~24 MiB 输出。
#[test]
#[serial]
fn test_worst_case_escaped_history_roundtrips_through_disk() {
    run_isolated(
        "test_worst_case_escaped_history_roundtrips_through_disk",
        || {
            let home = setup();
            let entries: Vec<String> = (0..64)
                .map(|i| format!("{i:02}{}", "\0".repeat(MAX_ENTRY_BYTES - 2)))
                .collect();
            for entry in &entries {
                super::push_history(entry);
            }

            let saved = wait_for_history_file(&home, entries.len(), Duration::from_secs(10));
            assert_eq!(saved, entries, "落盘内容应与入栈内容一致");
            let history = history_snapshot();
            assert_eq!(history.len(), entries.len());
            assert_eq!(
                history_bytes(&history),
                MAX_HISTORY_BYTES,
                "用例需占满载荷预算以触发最坏转义"
            );

            let file_bytes = std::fs::metadata(history_file(&home)).expect("worst case escaped history roundtrips through disk: the fixture storage and layout must support the requested operation").len();
            assert!(
                file_bytes > MAX_HISTORY_BYTES as u64 * 5,
                "U+0000 应按 6 字节扩展，实际文件仅 {file_bytes} 字节"
            );
            assert!(
                file_bytes <= MAX_HISTORY_FILE_BYTES,
                "文件 {file_bytes} 字节超过推导上限 {MAX_HISTORY_FILE_BYTES}"
            );

            // 模拟重启：清空内存后只从磁盘恢复。
            *INPUT_HISTORY.state().write() = VecDeque::new();
            super::load_history();

            let restored = history_snapshot();
            assert_eq!(
                restored.len(),
                entries.len(),
                "load_history 必须读回 writer 写出的全部条目"
            );
            assert!(restored.iter().eq(entries.iter()), "恢复内容与落盘前不一致");
        },
    );
}

/// 遗留超大文件在 parse 前按文件大小拒绝，启动不为其分配整份 JSON。
///
/// fixture 是合法 JSON（短哨兵 + 尾部空白），去掉大小门后 sentinel 会被加载，
/// 断言因此能真正区分「大小门生效」与「文件本身解析失败」。
#[test]
#[serial]
fn test_load_history_ignores_file_over_size_budget() {
    run_isolated("test_load_history_ignores_file_over_size_budget", || {
        let home = setup();
        let path = history_file(&home);
        std::fs::create_dir_all(path.parent().expect("load history ignores file over size budget: the fixture path must have its expected parent directory")).expect("load history ignores file over size budget: isolated fixture workspace must remain accessible");
        let mut fixture = String::from("[\"sentinel\"]");
        let padding = MAX_HISTORY_FILE_BYTES as usize + 1 - fixture.len();
        fixture.push_str(&" ".repeat(padding));
        std::fs::write(&path, fixture).expect("load history ignores file over size budget: isolated fixture workspace must remain accessible");

        super::load_history();

        assert!(INPUT_HISTORY.state().read().is_empty());
    });
}

/// 文件内超限单条被过滤，其余条目保持相对顺序。
#[test]
#[serial]
fn test_load_history_skips_oversized_entries() {
    run_isolated("test_load_history_skips_oversized_entries", || {
        let home = setup();
        let entries = vec![
            "kept".to_string(),
            "y".repeat(MAX_ENTRY_BYTES + 1),
            "also-kept".to_string(),
        ];
        write_history_entries(&home, &entries);

        super::load_history();

        let history = history_snapshot();
        let stored: Vec<&str> = history.iter().map(String::as_str).collect();
        assert_eq!(stored, vec!["kept", "also-kept"]);
    });
}

/// 合并写：200 次连续提交在静默窗口内只触发受控次数的落盘。
#[test]
fn test_writer_coalesces_burst_into_bounded_persists() {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let (persisted_tx, persisted_rx) = std::sync::mpsc::channel();
    let writes = Arc::new(AtomicUsize::new(0));
    let worker_writes = Arc::clone(&writes);
    let worker = std::thread::spawn(move || {
        super::run_writer(rx, move |_path| {
            worker_writes.fetch_add(1, Ordering::SeqCst);
            let _ = persisted_tx.send(());
        });
    });

    for i in 0..200 {
        // 与生产路径一致：通道满时丢弃本次请求，worker 落盘时读取最新内容。
        let _ = tx.try_send(PathBuf::from(format!("/tmp/input-history-{i}.json")));
    }
    persisted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker 应完成一次落盘");
    // 再等一个静默窗口：若仍有迟到批次，落盘计数会继续增长。
    std::thread::sleep(SAVE_DEBOUNCE * 2);

    let write_count = writes.load(Ordering::SeqCst);
    assert!(
        write_count <= 3,
        "连续 200 次提交应合并为受控次数落盘，实际 {write_count} 次"
    );

    drop(tx);
    worker.join().expect("writer coalesces burst into bounded persists: the join fixture operation uses valid inputs");
}

/// 兜底：序列化输出超过文件门限时拒绝写入并保留旧文件。
///
/// 正常预算不会产生该状态；用例直接调用落盘函数，确保不变量被破坏时
/// 不会留下 `load_history` 整份忽略的文件，也不静默覆盖旧历史。
#[test]
#[serial]
fn test_persist_history_refuses_output_over_file_cap() {
    run_isolated("test_persist_history_refuses_output_over_file_cap", || {
        let home = setup();
        let path = history_file(&home);
        std::fs::create_dir_all(path.parent().expect("persist history refuses output over file cap: the fixture path must have its expected parent directory")).expect("persist history refuses output over file cap: isolated fixture workspace must remain accessible");
        std::fs::write(&path, "[\"previous\"]").expect("persist history refuses output over file cap: isolated fixture workspace must remain accessible");

        let oversized = format!("\"{}\"", "a".repeat(MAX_HISTORY_FILE_BYTES as usize + 1));
        super::persist_history(&path, &oversized);

        assert_eq!(
        std::fs::read_to_string(&path).expect("persist history refuses output over file cap: the fixture storage and layout must support the requested operation"),
        "[\"previous\"]",
        "超限保存必须被拒绝，旧文件保持可加载"
    );
        assert!(
            !path.with_extension("tmp").exists(),
            "拒绝时不得残留临时文件"
        );
    });
}
