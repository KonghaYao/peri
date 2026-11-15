//! Tests for input_history

use super::HISTORY_DRAFT as DRAFT;
use super::*;
use serial_test::serial;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 用例级隔离 HOME：持有 TempDir 与恢复守卫。
///
/// 字段顺序保证先恢复 HOME、再删除临时目录。
struct IsolatedHome {
    _guard: HomeGuard,
    dir: tempfile::TempDir,
}

impl IsolatedHome {
    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

/// 重置 atom，并把 HOME 指向用例独占的临时目录。
///
/// 返回的守卫必须绑定到局部变量：`push_history` 会异步写 `~/.peri/input-history.json`，
/// 不隔离 HOME 就会覆盖真实用户历史。
fn setup() -> IsolatedHome {
    let dir = tempfile::tempdir().unwrap();
    let guard = HomeGuard::redirect_to(dir.path());
    crate::kit::atoms::init_atoms();
    *INPUT_HISTORY.state().write() = VecDeque::new();
    *INPUT_HISTORY_INDEX.state().write() = None;
    *HISTORY_DRAFT.state().write() = None;
    IsolatedHome { _guard: guard, dir }
}

/// RAII guard：测试期间改 HOME，Drop 时恢复进入前的值。
///
/// 断言失败或提前 panic 时 Drop 仍会执行，不会把临时 HOME 残留给其他测试。
struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn redirect_to(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("HOME");
        // SAFETY: #[serial] 保证 HOME 修改与其他测试互斥。
        unsafe {
            std::env::set_var("HOME", path);
        }
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: 同上。守卫由测试线程在 panic 展开时析构。
        unsafe {
            match self.previous.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

#[test]
#[serial]
fn test_push_history_grows_stack() {
    let _home = setup();
    push_history("hello");
    push_history("world");
    assert_eq!(INPUT_HISTORY.state().read().len(), 2);
}

#[test]
#[serial]
fn test_push_history_ignores_empty() {
    let _home = setup();
    push_history("   ");
    push_history("");
    assert_eq!(INPUT_HISTORY.state().read().len(), 0);
}

#[test]
#[serial]
fn test_push_history_dedup_consecutive() {
    let _home = setup();
    push_history("hello");
    push_history("hello"); // 重复不入栈
    assert_eq!(INPUT_HISTORY.state().read().len(), 1);
}

#[test]
#[serial]
fn test_push_history_trims_whitespace() {
    let _home = setup();
    push_history("  hello  ");
    assert_eq!(INPUT_HISTORY.state().read().len(), 1);
    let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
    assert_eq!(stored[0], "hello");
}

#[test]
#[serial]
fn test_history_up_navigation() {
    let _home = setup();
    push_history("first");
    push_history("second");
    push_history("third");

    // 第一次 Up → 最新（third）
    assert_eq!(history_up(None).as_deref(), Some("third"));
    // 第二次 Up → second
    assert_eq!(history_up(None).as_deref(), Some("second"));
    // 第三次 Up → first
    assert_eq!(history_up(None).as_deref(), Some("first"));
    // 第四次 Up → 已在最旧，保持 first
    assert_eq!(history_up(None).as_deref(), Some("first"));
}

#[test]
#[serial]
fn test_history_down_restores_empty_draft_at_bottom() {
    let _home = setup();
    push_history("a");
    push_history("b");

    history_up(Some("")); // → b，并保存空草稿
    history_up(None); // → a
    assert_eq!(history_down().as_deref(), Some("b"));
    assert_eq!(history_down().as_deref(), Some(""));
    assert_eq!(*INPUT_HISTORY_INDEX.state().read(), None);
}

#[test]
#[serial]
fn test_reset_history_cursor() {
    let _home = setup();
    push_history("x");
    history_up(None);
    assert!(INPUT_HISTORY_INDEX.state().read().is_some());
    reset_history_cursor();
    assert!(INPUT_HISTORY_INDEX.state().read().is_none());
}

#[test]
#[serial]
fn test_history_up_empty_stack() {
    let _home = setup();
    assert_eq!(history_up(None), None);
    assert_eq!(history_down(), None);
}

#[test]
#[serial]
fn test_max_history_capacity() {
    let _home = setup();
    for i in 0..1050 {
        push_history(&format!("cmd-{}", i));
    }
    assert_eq!(INPUT_HISTORY.state().read().len(), MAX_HISTORY);
    let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
    assert_eq!(stored.front().map(String::as_str), Some("cmd-50"));
    assert_eq!(stored.back().map(String::as_str), Some("cmd-1049"));
}

#[test]
#[serial]
fn test_draft_saved_on_history_entry_and_restored() {
    let _home = setup();
    push_history("old");
    assert_eq!(history_up(Some("current draft")).as_deref(), Some("old"));
    assert_eq!(DRAFT.state().read().as_deref(), Some("current draft"));
    assert_eq!(history_down(), Some("current draft".to_string()));
    assert!(DRAFT.state().read().is_none());
}

#[test]
#[serial]
fn test_draft_cleared_on_reset() {
    let _home = setup();
    push_history("x");
    history_up(Some("draft"));
    assert!(DRAFT.state().read().is_some());
    reset_history_cursor();
    assert!(DRAFT.state().read().is_none());
}

#[test]
#[serial]
fn test_empty_draft_is_preserved_for_restore() {
    let _home = setup();
    push_history("old");
    history_up(Some(""));
    // 空草稿也要保存，否则 Down 回到底部时无法清空旧历史项。
    assert_eq!(DRAFT.state().read().as_deref(), Some(""));
}

/// L11：`save_history` 在 `std::thread::spawn` 中异步写盘，push_history 立即返回。
///
/// 通过临时改 `HOME` 指向 tempdir，push 一条 unique marker，等后台线程 rename 完成，
/// 读 `~/.peri/input-history.json` 断言内容已落盘。`#[serial]` 防止并发污染其他测试。
///
/// 注意：edition 2024 中 `std::env::set_var`/`remove_var` 为 unsafe，
/// 需在 `unsafe` 块中调用。`#[serial]` 保证这些 mutation 与其他测试互斥。
#[test]
#[serial]
fn test_save_history_does_not_block_and_persists_async() {
    // setup() 已把 HOME 指向用例独占的临时目录（守卫在用例结束时恢复）。
    let home = setup();

    // 用唯一 marker 避免与其他测试串扰。
    let marker = "async-persist-marker-7E3A";
    push_history(marker);

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
}

fn history_file(home: &IsolatedHome) -> PathBuf {
    home.path().join(".peri").join("input-history.json")
}

fn write_history_entries(home: &IsolatedHome, entries: &[String]) {
    let path = history_file(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(entries).unwrap()).unwrap();
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
    let _home = setup();
    let payload = "界".repeat(10_000); // 每条 30_000 bytes
    for i in 0..1000 {
        push_history(&format!("{i:04}{payload}"));
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
    assert!(history.back().unwrap().starts_with("0999"));
    assert!(
        !history.front().unwrap().starts_with("0000"),
        "超限时必须淘汰最旧条目"
    );
}

/// 单条超过 `MAX_ENTRY_BYTES` 不保留，但后续提交仍正常入栈。
#[test]
#[serial]
fn test_oversized_entry_is_not_retained() {
    let _home = setup();
    push_history("small");
    *INPUT_HISTORY_INDEX.state().write() = Some(0);

    push_history(&"x".repeat(MAX_ENTRY_BYTES + 1));

    let history = history_snapshot();
    assert_eq!(history.len(), 1);
    assert_eq!(history.back().map(String::as_str), Some("small"));
    assert!(INPUT_HISTORY_INDEX.state().read().is_none());
}

/// 浏览只返回选中项，且不改变栈内容（大条目场景下走同一路径）。
#[test]
#[serial]
fn test_navigation_returns_selected_entry_without_mutating_stack() {
    let _home = setup();
    let older = format!("older-{}", "字".repeat(5_000));
    let newer = format!("newer-{}", "字".repeat(5_000));
    push_history(&older);
    push_history(&newer);
    let before = INPUT_HISTORY.state().read().clone();

    assert_eq!(history_up(None).as_deref(), Some(newer.as_str()));
    assert_eq!(history_up(None).as_deref(), Some(older.as_str()));
    assert_eq!(history_up(None).as_deref(), Some(older.as_str()));
    assert_eq!(history_down().as_deref(), Some(newer.as_str()));

    let after = history_snapshot();
    assert_eq!(before.len(), after.len());
    assert!(before.iter().eq(after.iter()), "浏览不得改变历史栈");
}

/// 启动加载按条数预算裁剪遗留文件，保持「淘汰最旧」语义。
#[test]
#[serial]
fn test_load_history_applies_count_budget() {
    let home = setup();
    let entries: Vec<String> = (0..1050).map(|i| format!("cmd-{i}")).collect();
    write_history_entries(&home, &entries);

    load_history();

    let history = history_snapshot();
    assert_eq!(history.len(), MAX_HISTORY);
    assert_eq!(history.front().map(String::as_str), Some("cmd-50"));
    assert_eq!(history.back().map(String::as_str), Some("cmd-1049"));
}

/// 启动加载按总字节预算裁剪（100 × 50 KiB ≈ 5 MiB，保留尾部 ≤ 4 MiB）。
#[test]
#[serial]
fn test_load_history_applies_byte_budget() {
    let home = setup();
    let entries: Vec<String> = (0..100)
        .map(|i| format!("{i:03}-{}", "x".repeat(50 * 1024)))
        .collect();
    write_history_entries(&home, &entries);

    load_history();

    let history = history_snapshot();
    assert!(history_bytes(&history) <= MAX_HISTORY_BYTES);
    assert!(history.len() < entries.len());
    assert!(history.back().unwrap().starts_with("099-"));
    assert!(!history.front().unwrap().starts_with("000-"));
}

/// 遗留超大文件在 parse 前按文件大小拒绝，启动不为其分配整份 JSON。
#[test]
#[serial]
fn test_load_history_ignores_file_over_size_budget() {
    let home = setup();
    let path = history_file(&home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_HISTORY_FILE_BYTES + 1).unwrap();
    drop(file);

    load_history();

    assert!(INPUT_HISTORY.state().read().is_empty());
}

/// 文件内超限单条被过滤，其余条目保持相对顺序。
#[test]
#[serial]
fn test_load_history_skips_oversized_entries() {
    let home = setup();
    let entries = vec![
        "kept".to_string(),
        "y".repeat(MAX_ENTRY_BYTES + 1),
        "also-kept".to_string(),
    ];
    write_history_entries(&home, &entries);

    load_history();

    let history = history_snapshot();
    let stored: Vec<&str> = history.iter().map(String::as_str).collect();
    assert_eq!(stored, vec!["kept", "also-kept"]);
}

/// 合并写：200 次连续提交在静默窗口内只触发受控次数的落盘。
#[test]
fn test_writer_coalesces_burst_into_bounded_persists() {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let (persisted_tx, persisted_rx) = std::sync::mpsc::channel();
    let writes = Arc::new(AtomicUsize::new(0));
    let worker_writes = Arc::clone(&writes);
    let worker = std::thread::spawn(move || {
        run_writer(rx, move |_path| {
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
    worker.join().unwrap();
}
