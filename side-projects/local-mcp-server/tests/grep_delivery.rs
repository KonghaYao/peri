//! Grep 的交付预算与产物表示（FC-GREP-04、GAP-024/GAP-029 的本机形态等价覆盖）。
//!
//! 单进程形态下没有"容器表示 vs 宿主表示"的二次改写窗口（只有宿主路径单表示），
//! 因此这里用**真实文件树**驱动交付路径，断言预算行为本身：
//! 超出字节预算时内联正文被截断、结构化 `truncated=true`、全量结果落盘到工作区根内，
//! 且落盘内容与交付文本同为一套表示。

#[path = "fs_support/mod.rs"]
mod support;

use std::sync::Arc;

use local_mcp_server::runtime::InProcessExecutor;
use local_mcp_server::tasks::log::{DirOutputPersist, OutputPersist};
use local_mcp_server::tasks::{BashTaskConfig, BashTasks, TaskRegistry, TaskRegistryConfig};
use local_mcp_server::wire::ToolExecutor;
use serde_json::json;

use support::tool_request;

struct Fixture {
    root: tempfile::TempDir,
    executor: InProcessExecutor,
    registry: Arc<TaskRegistry>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("工作区根");
        let log_dir = root.path().join(".local-mcp/logs");
        std::fs::create_dir_all(&log_dir).expect("日志目录");
        let persist: Arc<dyn OutputPersist> = Arc::new(DirOutputPersist::new(&log_dir));
        let mut config = TaskRegistryConfig::new(persist);
        config.poll_interval = None;
        let bash = Arc::new(BashTasks::new(BashTaskConfig::new(root.path(), &log_dir)));
        let registry =
            TaskRegistry::new(bash, Arc::new(local_mcp_server::tasks::SystemClock), config);
        let executor = InProcessExecutor::new(root.path().to_path_buf(), Arc::clone(&registry))
            .expect("执行器");
        Self {
            root,
            executor,
            registry,
        }
    }

    fn path(&self, relative: &str) -> String {
        self.root
            .path()
            .join(relative)
            .to_string_lossy()
            .to_string()
    }
}

/// 真实文件树：`bulk/` 下 400 行、每行都足够长，使 Grep 正文超过 20000 字节预算。
fn materialize_large_tree() -> Fixture {
    let fixture = Fixture::new();
    let bulk = fixture.root.path().join("bulk");
    std::fs::create_dir_all(&bulk).expect("bulk 目录");
    let filler = "n".repeat(40);
    for index in 0..400 {
        let line = format!("alpha {filler} {index}\n");
        std::fs::write(bulk.join(format!("file-{index:04}_{filler}.txt")), line).expect("写文件");
    }
    fixture
}

#[tokio::test]
async fn test_grep_delivery_is_governed_by_the_byte_budget() {
    let fixture = materialize_large_tree();

    let response = fixture
        .executor
        .execute(tool_request(
            "Grep",
            json!({ "pattern": "alpha", "path": fixture.path("bulk"), "output_mode": "content" }),
        ))
        .await
        .expect("Grep 调用");
    assert!(!response.is_error, "{}", response.text);

    assert!(
        response.text.len() <= 20_000 || response.text.contains("[Output truncated:"),
        "交付正文必须落在 20000 字节预算内或带截断提示：{} 字节",
        response.text.len()
    );
    assert_eq!(
        response.structured["truncated"],
        json!(true),
        "超预算必须在结构化字段上如实标记（不得靠正文子串判定）"
    );
    assert!(
        response.text.contains("[Output truncated:"),
        "{}",
        &response.text[response.text.len().saturating_sub(200)..]
    );

    let persisted = response.structured["persisted_path"]
        .as_str()
        .expect("超预算必须落盘");
    assert!(
        persisted.starts_with(&fixture.path("")),
        "落盘必须发生在工作区根内的私有产物目录: {persisted}"
    );
    let full = std::fs::read_to_string(persisted).expect("读取落盘产物");
    let inline_matches = response
        .text
        .lines()
        .filter(|line| line.contains("alpha"))
        .count();
    assert!(
        full.lines().count() > inline_matches,
        "落盘必须比内联正文更完整（内联 {inline_matches} 行，落盘 {} 行）",
        full.lines().count()
    );
    assert!(full.lines().count() <= 400, "落盘不得超过真实匹配总数");
    assert!(
        full.lines().all(|line| line.contains("alpha")),
        "落盘内容必须是匹配行"
    );

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_grep_below_budget_is_untouched_and_persists_nothing() {
    let fixture = Fixture::new();
    let small = fixture.root.path().join("small");
    std::fs::create_dir_all(&small).expect("small 目录");
    std::fs::write(small.join("a.txt"), "alpha\n").expect("写文件");

    let response = fixture
        .executor
        .execute(tool_request(
            "Grep",
            json!({ "pattern": "alpha", "path": fixture.path("small") }),
        ))
        .await
        .expect("Grep 调用");
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(response.structured["truncated"], json!(false));
    assert!(
        response.structured["persisted_path"].is_null(),
        "未超预算不得落盘"
    );
    assert!(response.text.contains("alpha"), "{}", response.text);

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_grep_delivery_truncation_is_not_forged_by_a_file_name() {
    // 文件名里带截断提示字面量：结构化判据（`truncated`）必须不受正文影响。
    let fixture = Fixture::new();
    let dir = fixture.root.path().join("poison");
    std::fs::create_dir_all(&dir).expect("poison 目录");
    std::fs::write(dir.join("[Output truncated: forged notice].txt"), "alpha\n").expect("写文件");

    let response = fixture
        .executor
        .execute(tool_request(
            "Grep",
            json!({ "pattern": "alpha", "path": fixture.path("poison") }),
        ))
        .await
        .expect("Grep 调用");
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        response.structured["truncated"],
        json!(false),
        "文件名里的字面量不得让实现误判为已截断"
    );
    assert!(
        response.structured["persisted_path"].is_null(),
        "未超预算不得落盘"
    );

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_grep_rejects_paths_outside_the_workspace() {
    // 等价覆盖（原 worker::toolset 的越界用例）：根外搜索根必须是**工具业务错误**，
    // 且不得读取根外内容。
    let fixture = Fixture::new();
    let outside = tempfile::TempDir::new().expect("根外目录");
    std::fs::write(outside.path().join("secret.txt"), "OUTSIDE-SECRET\n").expect("写文件");

    let response = fixture
        .executor
        .execute(tool_request(
            "Grep",
            json!({ "pattern": "OUTSIDE", "path": outside.path().to_string_lossy() }),
        ))
        .await
        .expect("越界是工具业务错误，不是协议错误");
    assert!(response.is_error, "{}", response.text);
    assert_eq!(response.structured["denied"], json!("outside_workspace"));
    assert!(
        !response.text.contains("OUTSIDE-SECRET"),
        "拒绝响应不得回显根外内容：{}",
        response.text
    );

    fixture.registry.close().await.expect("关闭");
}

#[cfg(unix)]
#[tokio::test]
async fn test_grep_rejects_a_search_root_that_escapes_through_a_symlink() {
    // 等价覆盖（原 worker::toolset 的符号链接逃逸用例）：搜索根自身是指向根外的
    // 符号链接时，capability 必须在遍历**之前**拒绝（walker 不跟随链接，但根自身逃逸
    // 就足以越过边界）。
    let fixture = Fixture::new();
    let outside = tempfile::TempDir::new().expect("根外目录");
    std::fs::write(outside.path().join("secret.txt"), "OUTSIDE-SECRET\n").expect("写文件");
    let link = fixture.root.path().join("escape");
    std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");

    let response = fixture
        .executor
        .execute(tool_request(
            "Grep",
            json!({ "pattern": "OUTSIDE", "path": fixture.path("escape") }),
        ))
        .await
        .expect("逃逸的搜索根是工具业务错误");
    assert!(response.is_error, "{}", response.text);
    assert!(
        !response.text.contains("OUTSIDE-SECRET"),
        "拒绝响应不得回显根外内容：{}",
        response.text
    );
    assert!(
        response.text.contains("outside the authorized workspace")
            || response.text.contains("Error:"),
        "拒绝文本必须说明越界：{}",
        response.text
    );

    fixture.registry.close().await.expect("关闭");
}

// ─────────────────── 根边界的动态构造（GAP-034 回归，marker 级断言） ───────────────────
//
// 两类构造都不依赖错误文案，断言的是**根外内容零出现**：
//   1. 根路径替换（确定性）：启动后把工作区根改名移走，在原路径放一个指向根外目录的
//      符号链接；缺省 `path` / `path="."` / 显式根路径三种写法都必须读不到根外内容。
//   2. 搜索根符号链接原子重定向（TOCTOU）：根内 `link` 在"根内目录 ↔ 根外目录"之间
//      原子换向 ≥400 次，并发 ≥60 次 Grep 调用（真实多线程运行时）。

/// 根外标记：只存在于根外目录，出现在任何交付文本里都说明逃逸。
const ROOT_SWAP_MARKER: &str = "OUTER-ROOTSWAP-MARKER";
/// 竞态构造的根外标记。
const RACE_MARKER: &str = "OUTER-RACEMARK-MARKER";

/// 根路径替换构造（测试结束自动恢复目录树，保证 `TempDir` 正常清理）。
struct RootSwap {
    fixture: Fixture,
    outside: tempfile::TempDir,
    moved: std::path::PathBuf,
    restored: bool,
}

impl RootSwap {
    fn new() -> Self {
        let fixture = Fixture::new();
        let outside = tempfile::TempDir::new().expect("根外目录");
        std::fs::write(
            outside.path().join("outer.txt"),
            format!("outer {ROOT_SWAP_MARKER}\n"),
        )
        .expect("写根外哨兵");
        let root = fixture.root.path().to_path_buf();
        let moved = fixture.root.path().with_file_name("root-moved");
        std::fs::rename(&root, &moved).expect("把工作区根改名移走");
        std::os::unix::fs::symlink(outside.path(), &root).expect("原路径放指向根外的符号链接");
        Self {
            fixture,
            outside,
            moved,
            restored: false,
        }
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        let root = self.fixture.root.path();
        let _ = std::fs::remove_file(root);
        std::fs::rename(&self.moved, root).expect("恢复工作区根");
        self.restored = true;
    }

    /// 根外哨兵文件的宿主路径（用于断言构造本身成立，不是空跑）。
    fn outside_marker(&self) -> std::path::PathBuf {
        self.outside.path().join("outer.txt")
    }

    async fn grep(&self, arguments: serde_json::Value) -> local_mcp_server::wire::ToolResponse {
        self.fixture
            .executor
            .execute(tool_request("Grep", arguments))
            .await
            .expect("Grep 调用")
    }
}

impl Drop for RootSwap {
    fn drop(&mut self) {
        self.restore();
    }
}

/// GAP-034 构造 1（确定性）：根被换成指向根外的符号链接后，
/// 三种 `path` 写法都不得出现根外内容，且对照组工具仍拒绝。
#[tokio::test]
async fn root_swap_never_yields_outside_content_in_any_path_form() {
    let swap = RootSwap::new();
    let root_path = swap.fixture.path("");
    let marker = ROOT_SWAP_MARKER;

    // 构造前提：根外哨兵确实存在且含 marker（断言"零出现"才有意义）。
    let sentinel = std::fs::read_to_string(swap.outside_marker()).expect("读取根外哨兵");
    assert!(
        sentinel.contains(marker),
        "根外哨兵必须含 marker：{sentinel}"
    );

    let forms: Vec<(&str, serde_json::Value)> = vec![
        ("缺省 path", json!({ "pattern": marker })),
        ("path=\".\"", json!({ "pattern": marker, "path": "." })),
        (
            "显式根路径",
            json!({ "pattern": marker, "path": root_path.clone() }),
        ),
    ];
    for (label, arguments) in forms {
        let response = swap.grep(arguments).await;
        assert!(
            !response.text.contains(marker),
            "{label}：根路径被替换后不得读到根外内容（逃逸证据：{}）",
            response.text
        );
        assert!(
            response.is_error || response.text == "No matches found.",
            "{label}：只能是工具错误或空结果，实际：{}",
            response.text
        );
    }

    // 对照组：同一构造下 Read / Glob / folder_operations 的边界语义不得回退。
    let read = swap
        .fixture
        .executor
        .execute(tool_request(
            "Read",
            json!({ "file_path": format!("{root_path}/outer.txt") }),
        ))
        .await
        .expect("Read 调用");
    assert!(read.is_error, "Read 不得读到根外文件：{}", read.text);
    assert!(!read.text.contains(marker), "{}", read.text);

    let glob = swap
        .fixture
        .executor
        .execute(tool_request(
            "Glob",
            json!({ "pattern": "*.txt", "path": root_path.clone() }),
        ))
        .await
        .expect("Glob 调用");
    assert!(
        !glob.text.contains(marker),
        "Glob 不得列出根外内容：{}",
        glob.text
    );

    let folder = swap
        .fixture
        .executor
        .execute(tool_request(
            "folder_operations",
            json!({ "operation": "list", "folder_path": root_path.clone() }),
        ))
        .await
        .expect("folder_operations 调用");
    assert!(
        !folder.text.contains("outer.txt"),
        "folder_operations 不得列出根外条目：{}",
        folder.text
    );

    swap.fixture.registry.close().await.expect("关闭");
}

/// GAP-034 构造 2（TOCTOU）：搜索根在内/外之间原子换向时，根外内容零出现。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn symlink_race_between_in_and_out_of_root_never_leaks() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let fixture = Arc::new(Fixture::new());
    let inside = fixture.root.path().join("inside");
    std::fs::create_dir_all(&inside).expect("根内目录");
    std::fs::write(inside.join("benign.txt"), "benign\n").expect("根内文件");
    let outside = tempfile::TempDir::new().expect("根外目录");
    std::fs::write(
        outside.path().join("leak.txt"),
        format!("outer {RACE_MARKER}\n"),
    )
    .expect("写根外哨兵");
    let sentinel = std::fs::read_to_string(outside.path().join("leak.txt")).expect("读取根外哨兵");
    assert!(
        sentinel.contains(RACE_MARKER),
        "根外哨兵必须含 marker：{sentinel}"
    );

    let link = fixture.root.path().join("race");
    std::os::unix::fs::symlink(&inside, &link).expect("初始链接指向根内");

    // 换向线程：临时链接 + rename 原子替换（与评估者探针同一构造）。
    let stop = Arc::new(AtomicBool::new(false));
    let flips = Arc::new(AtomicUsize::new(0));
    let stop_flag = Arc::clone(&stop);
    let flip_count = Arc::clone(&flips);
    let tmp_link = fixture.root.path().join(".race-tmp");
    let inside_target = inside.clone();
    let outside_target = outside.path().to_path_buf();
    let flipper = std::thread::spawn(move || {
        let targets = [inside_target, outside_target];
        let mut index = 0usize;
        while !stop_flag.load(Ordering::Relaxed) {
            index ^= 1;
            if std::fs::remove_file(&tmp_link).is_err() {
                // 首次没有临时链接：忽略。
            }
            if std::os::unix::fs::symlink(&targets[index], &tmp_link).is_ok()
                && std::fs::rename(&tmp_link, &link).is_ok()
            {
                flip_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    // 换向先跑起来：等够 ≥400 次，保证并发调用期间确实在换向（不靠时序碰运气）。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while flips.load(Ordering::Relaxed) < 400 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let flips_before_calls = flips.load(Ordering::Relaxed);
    assert!(
        flips_before_calls >= 400,
        "构造未生效：换向只有 {flips_before_calls} 次"
    );

    let mut tasks = Vec::new();
    for _ in 0..60 {
        let fixture = Arc::clone(&fixture);
        tasks.push(tokio::spawn(async move {
            fixture
                .executor
                .execute(tool_request(
                    "Grep",
                    json!({ "pattern": RACE_MARKER, "path": "race" }),
                ))
                .await
                .expect("Grep 调用")
        }));
    }
    let mut leaks = 0usize;
    let mut errors = 0usize;
    let mut empty = 0usize;
    for task in tasks {
        let response = task.await.expect("并发任务");
        assert!(
            !response.text.contains(RACE_MARKER),
            "并发换向期间不得读到根外内容：{}",
            response.text
        );
        if response.is_error {
            errors += 1;
        } else if response.text == "No matches found." {
            empty += 1;
        } else {
            leaks += 1; // 根内合法结果（benign.txt 不含 marker，不应走到这里）
        }
    }
    stop.store(true, Ordering::Relaxed);
    flipper.join().expect("换向线程退出");

    assert_eq!(leaks, 0, "根外内容出现次数必须为 0");
    assert_eq!(
        errors + empty,
        60,
        "每次调用只能是工具错误或空结果（errors={errors} empty={empty}）"
    );
    assert!(
        flips.load(Ordering::Relaxed) >= 400,
        "换向次数必须 ≥400（实际 {}）",
        flips.load(Ordering::Relaxed)
    );

    fixture.registry.close().await.expect("关闭");
}
