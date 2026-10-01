//! 显式云端端到端回归（默认 `#[ignore]`）：**真实部署入口** `Resources::open_deployment`
//! 的完整生命周期，跨进程、冷恢复。
//!
//! 前面几组云实验分别打的是 adapter 行为、门面行为、故障收敛；这一组打的是**装配入口本身**：
//! 入口配置（`--session-store` + `--session-store-token-env`）→ D 装配（`open_deployment`）
//! → 远程数据面 + 本机执行面 → 全生命周期行为。因此它不听任何内部端口，只用公开门面，
//! 并且每一段都在**独立进程**里跑：进程边界消失之后仍然成立的事实才是 durable 事实。
//!
//! | 阶段 | 进程 | 断言 |
//! | --- | --- | --- |
//! | 写入 | 子进程 A（temp HOME） | 创建 → 追加 → 排空 → compact → fork → child → 标题 A→B→A → close |
//! | 冷恢复 | 子进程 B（同一 HOME，新进程） | 未决收敛 `Recovered` → 新 runtime owner → rewind → 删除 → 只读复核 |
//! | 只读（全新 HOME） | 子进程 C（全新 HOME） | 本机没有执行事实 ⇒ 打开按 `NotFound` 如实失败；HOME 一个文件都不建 |
//! | 只读（沿用 A 的 HOME） | 子进程 D（沿用 A 的 HOME） | 远端历史可读、执行权不可得；写入按 `ReadOnlyStore` 拒绝；本机状态不变 |
//!
//! 父测试只做三件事：拉起子进程、用**新连接**核对云端事实（阶段间与清理后各一次）、用
//! **只读连接**盘点本机执行面库（阶段间各一次，见下）。
//!
//! 本机盘点读的是真实落盘的那个文件（`registry_path(home)`，不是门面自陈）：远端模式下本机
//! 只留被授权的 workspace 证据（`projects` / `workspaces`）；runtime 状态不持久化，
//! 不留任何 store 痕迹，也不留任何会话数据（`threads` / `messages` /
//! `session_bindings` 全为 0）。库里本不该有的东西一旦回来，只核对云端计数是看不见的，这条
//! 盘点就是为它准备的。
//!
//! 期望值全部**派生**，不另抄一份会悄悄过期的名单：真实运行的本机库与同一构建在本机模式下
//! 新建的库逐表比对（用户裁决「远端库完全 = 本地库的模式」），v10 删掉的五张本机远程表名
//! 则从 `sqlite_store/schema.rs` 的 `DROPPED_LOCAL_TABLES` 原文派生（见 [`dropped_local_tables`]）。
//!
//! 阶段 C 的拒绝与本机库的只读打开是同一个判定：workspace 证据只存在本机库里，
//! 只读意图不许创建它，因此缺库时没有可用的执行面——只读打开
//! 一个不存在的库在两种存储模式下都按 `NotFound` 拒绝，而不是把「没有事实」降级成空事实。
//!
//! ## 事实与安全
//!
//! - HOME、workspace、frozen、历史全部是系统临时目录里的合成内容，不含真实历史、项目
//!   或任何仓库文件；本机执行面库因此落在系统 temp，不进仓库。
//! - 配置即用（v10）：指向哪个 store 就直接用哪个，打开之前不需要任何本机登记；子进程 C 与
//!   D 的差别只在**本机有没有库**（全新 HOME 与沿用 A 的 HOME），不在接纳语义。
//! - 凭证：只有测试进程自己解析 `.env`（键名 + 绝对路径选择器，见 [`super::cloud_tests`]），
//!   值只在本测试进程内注入同名环境变量供部署参数按名取用（部署参数按设计只接受来源名），
//!   父进程不设该变量；不打印 URL/token，输出只走 `SafeOut`。
//! - 只操作本轮 `run` 命名空间；结束用正常删除路径清理并复核计数为 0（共享 schema 与其他
//!   run 的收据不动）。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_deployment_
//! ```

use std::path::{Path, PathBuf};

use peri_acp_types::session_store::SessionStoreDeployment;
use sqlx::{sqlite::SqliteConnectOptions, AssertSqlSafe, Connection};

use super::cloud_tests::{check, failure, run_counts, unique_run_label, with_cleanup, CloudTarget};
use super::mutation::StoreAccess;

/// 子进程看到的临时 HOME（本机执行面库的位置）。
pub(super) const HOME_ENV: &str = "PERI_CLOUD_FACADE_HOME";
/// 子进程看到的合成 workspace（git 仓库，父测试建好，不随阶段变化）。
pub(super) const WORKSPACE_ENV: &str = "PERI_CLOUD_FACADE_WORKSPACE";
/// 本轮 run 标签：所有合成 thread id 都由它派生。
pub(super) const RUN_ENV: &str = "PERI_CLOUD_FACADE_RUN";
/// 只读子进程的模式：`fresh`（全新 HOME）或 `registered`（沿用写入期 HOME）。
pub(super) const READ_ONLY_ENV: &str = "PERI_CLOUD_FACADE_READ_ONLY";

/// 写入阶段创建的三个合成会话（`<run>-<后缀>`）：root 树根、它的子会话、fork 出来的独立树。
///
/// 子进程按这三个后缀造 id，两处共用同一组常量，
/// 免得「哪条会话有本机执行事实」这件事在父测试里另写一份镜像。
pub(super) const ROOT_SUFFIX: &str = "root";
pub(super) const CHILD_SUFFIX: &str = "child";
pub(super) const FORK_SUFFIX: &str = "fork";

/// 本机执行面库位置：部署入口只从 HOME 推导它，测试因此只隔离 HOME。
pub(super) fn registry_path(home: &Path) -> PathBuf {
    home.join(".peri").join("threads").join("threads.db")
}

// ─── 本机执行面库：父进程侧的只读盘点 ─────────────────────────────────────────
//
// 逐表相同。
//
// 盘点一律读**真实落盘的那个文件**（只读连接，不建库、不建目录），并且都在子进程退出之后
// 进行：那时没有写者，读到的是稳定状态，也不是任何门面的自陈。

/// 本机执行面库在一个时刻的盘点：版本、表集合与持久 workspace 事实计数。
///
/// 只有结构与计数，没有会话内容、没有路径——因此可以直接进断言消息与 `PROBE` 行。
#[derive(Debug, PartialEq, Eq)]
struct LocalFace {
    version: i64,
    tables: Vec<String>,
    threads: i64,
    messages: i64,
    bindings: i64,
    projects: i64,
    workspaces: i64,
}

/// 只读盘点本机执行面库；缺文件即失败（不会把「本机没有执行事实」降级成「空的执行事实」）。
async fn read_local_face(path: &Path) -> Result<LocalFace, String> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .map_err(|error| format!("the local execution face must open read-only: {error}"))?;
    let face = collect_local_face(&mut connection).await;
    connection
        .close()
        .await
        .map_err(|error| format!("the local execution face must close: {error}"))?;
    face
}

async fn collect_local_face(connection: &mut sqlx::SqliteConnection) -> Result<LocalFace, String> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| format!("local schema version is unreadable: {error}"))?;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("local table set is unreadable: {error}"))?;
    let threads = count_rows(connection, "threads").await?;
    let messages = count_rows(connection, "messages").await?;
    let bindings = count_rows(connection, "session_bindings").await?;
    let projects = count_rows(connection, "projects").await?;
    let workspaces = count_rows(connection, "workspaces").await?;
    Ok(LocalFace {
        version,
        tables,
        threads,
        messages,
        bindings,
        projects,
        workspaces,
    })
}

/// 单表行数；表名来自上方静态清单，未包含外部输入。
async fn count_rows(
    connection: &mut sqlx::SqliteConnection,
    table: &'static str,
) -> Result<i64, String> {
    sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| format!("local {table} count is unreadable: {error}"))
}

/// 同一构建在**本机模式**下新建的库：本机执行面的期望形状（版本 + 表集合）。
///
/// 期望值不另抄一份，而是让构建自己造一个库再读回来。用户裁决「远端库完全 = 本地库的模式，
/// 两个存储模式一致」在这里是同一个断言的另一半：远端模式下本机库的形状，必须与同一构建在
/// 本机模式下建出来的库逐表相同——schema 版本或表集合一旦分叉，这里先响。
async fn local_mode_baseline() -> Result<LocalFace, String> {
    let directory = tempfile::tempdir().map_err(|error| format!("baseline temp home: {error}"))?;
    let path = directory.path().join("threads.db");
    let resources =
        crate::Resources::open_deployment(&SessionStoreDeployment::local_path(path.clone()))
            .await
            .map_err(|error| format!("the local-mode baseline must open: {error:#}"))?;
    resources
        .into_concrete_for_test()
        .close()
        .await
        .map_err(|error| format!("the local-mode baseline must close: {error}"))?;
    read_local_face(&path).await
}

/// v10 从本机库删掉的表名，**从 schema 源码派生**（`sqlite_store/schema.rs` 的
/// `DROPPED_LOCAL_TABLES`）。
///
/// 为什么不直接 `use` 那个常量：它在本机存储模块里是模块私有的 `const`（可见性止于
/// `sqlite_store`），`sessions::remote` 读不到它，而本测试被授权的改动范围只在 remote 的两个
/// 测试文件里。于是改为读它的**定义原文**：清单增删时这里跟着变，不会留下第二份会悄悄过期的
/// 名单——这正是「另抄一份字面量」做不到的。
///
/// 声明找不到、解析不出名字、或结果不像表名时**失败**（不 `#[allow]`、不静默通过）：这条
/// 检查不许退化成「没有需要缺席的表」。
fn dropped_local_tables() -> Result<Vec<String>, String> {
    let source = include_str!("../sqlite_store/schema.rs");
    let declaration = source
        .split_once("const DROPPED_LOCAL_TABLES")
        .map(|(_, rest)| rest)
        .ok_or_else(|| {
            "the v10 dropped-table list is gone from sqlite_store/schema.rs; re-anchor this check \
             instead of letting it pass vacuously"
                .to_owned()
        })?;
    let names = array_element_names(declaration);
    if names.is_empty() || !names.iter().all(|name| is_table_name(name)) {
        return Err(format!(
            "the v10 dropped-table list could not be derived from sqlite_store/schema.rs: {names:?}"
        ));
    }
    Ok(names)
}

/// 数组字面量里**第一层**的字符串字面量：外层 `&[ ... ]` 的元素是表名，列名在更深的
/// `&[ ... ]` 里，因此按方括号深度筛选（行注释不参与深度计数，其中的引号也不是字面量）。
fn array_element_names(declaration: &str) -> Vec<String> {
    let Some((_, initializer)) = declaration.split_once('=') else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut value = String::new();
    let mut in_string = false;
    let mut chars = initializer.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            match ch {
                '"' => {
                    in_string = false;
                    if depth == 1 {
                        names.push(std::mem::take(&mut value));
                    }
                }
                _ => value.push(ch),
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                value.clear();
            }
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '[' => depth += 1,
            ']' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    names
}

/// 表名形态：小写标识符。解析结果不像表名时宁可失败，也不把垃圾当名字去找。
fn is_table_name(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 远端模式下本机执行面库必须成立的共同不变量（每个阶段的盘点之后都查一遍）。
fn check_local_face(
    phase: &str,
    face: &LocalFace,
    baseline: &LocalFace,
    dropped: &[String],
) -> Result<(), String> {
    check(
        face.version == baseline.version,
        &format!(
            "{phase}: the local execution face must carry the schema version this build writes \
             in local mode ({}), got {}",
            baseline.version, face.version
        ),
    )?;
    check(
        face.tables == baseline.tables,
        &format!(
            "{phase}: the local execution face must have exactly the local-mode table set {:?}, \
             got {:?}",
            baseline.tables, face.tables
        ),
    )?;
    check(
        dropped.iter().all(|table| !face.tables.contains(table)),
        &format!(
            "{phase}: the tables v10 dropped from the local store must not come back ({dropped:?}), \
             got {:?}",
            face.tables
        ),
    )?;
    check(
        face.threads == 0 && face.messages == 0 && face.bindings == 0,
        &format!(
            "{phase}: a remote store must leave no session data in the local face: \
             threads={} messages={} session_bindings={}",
            face.threads, face.messages, face.bindings
        ),
    )?;
    check(
        face.projects > 0 && face.workspaces > 0,
        &format!(
            "{phase}: workspace evidence is an authorized local fact and must be present: \
             projects={} workspaces={}",
            face.projects, face.workspaces
        ),
    )?;
    Ok(())
}

// ─── 父测试：拉起子进程 + 新连接核对 ──────────────────────────────────────────

const WRITE_CHILD: &str = "sessions::remote::cloud_deployment_child_tests::cloud_deployment_child_writes_the_synthetic_tree";
const RECOVER_CHILD: &str = "sessions::remote::cloud_deployment_child_tests::cloud_deployment_child_recovers_cold_then_rewinds_and_deletes";
const READ_ONLY_CHILD: &str = "sessions::remote::cloud_deployment_child_tests::cloud_deployment_child_read_only_leaves_no_trace";

/// 端到端：真实部署入口在真引擎上的完整生命周期。
///
/// 父进程不碰门面：它只提供合成环境、拉起子进程，并用**新连接**在阶段之间核对云端事实
/// （写入落库、删除生效、只读零副作用），最后按正常删除路径清理本轮命名空间。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_deployment_entry_point_full_lifecycle() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-facade");
    let home = tempfile::tempdir().expect("temp home");
    let read_only_home = tempfile::tempdir().expect("temp home for the read-only phase");
    let workspace = synthetic_workspace();
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    let result = with_cleanup(&target, &run, || async {
        let lines = lifecycle_flow(
            &target,
            &run,
            home.path(),
            read_only_home.path(),
            workspace.path(),
        )
        .await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("deployment_lifecycle=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn lifecycle_flow(
    target: &CloudTarget,
    run: &str,
    home: &Path,
    read_only_home: &Path,
    workspace: &Path,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    // 期望值先派生出来：同一构建在本机模式下新建的库（形状）+ schema 源码里的 v10 删除清单。
    let baseline = local_mode_baseline().await?;
    let dropped = dropped_local_tables()?;

    // ① 写入：新进程 + temp HOME + 合成 workspace/frozen，全部经真实部署入口。
    lines.extend(run_child(WRITE_CHILD, home, workspace, run, None)?);
    let written = counts_now(target, run).await?;
    check(
        written.sessions == 3 && written.messages == 6,
        &format!(
            "the write phase must land root + child + fork on the remote store: {}/{}",
            written.sessions, written.messages
        ),
    )?;
    check(
        registry_path(home).is_file(),
        "the deployment must put local facts under the home it was given",
    )?;
    lines.push(format!(
        "after_write sessions={} messages={} ledger={}",
        written.sessions, written.messages, written.ledger
    ));
    let local_written = read_local_face(&registry_path(home)).await?;
    check_local_face("after_write", &local_written, &baseline, &dropped)?;
    lines.push(format!(
        "local_after_write version={} tables={} sessions_rows={}/{}/{} v10_dropped={}",
        local_written.version,
        local_written.tables.len(),
        local_written.threads,
        local_written.messages,
        local_written.bindings,
        dropped.join(",")
    ));

    lines.extend(run_child(RECOVER_CHILD, home, workspace, run, None)?);
    let recovered = counts_now(target, run).await?;
    check(
        recovered.sessions == 1 && recovered.messages == 3,
        &format!(
            "deleting the root tree must leave exactly the fork tree: {}/{}",
            recovered.sessions, recovered.messages
        ),
    )?;
    lines.push(format!(
        "after_recovery sessions={} messages={} ledger={}",
        recovered.sessions, recovered.messages, recovered.ledger
    ));
    let local_recovered = read_local_face(&registry_path(home)).await?;
    check_local_face("after_recovery", &local_recovered, &baseline, &dropped)?;
    lines.push(format!(
        "local_after_recovery sessions_rows={}/{}/{}",
        local_recovered.threads, local_recovered.messages, local_recovered.bindings
    ));

    // ③ 显式只读：全新 HOME（本机无库）与沿用写入期 HOME 两种本机状态。
    lines.extend(run_child(
        READ_ONLY_CHILD,
        read_only_home,
        workspace,
        run,
        Some("fresh"),
    )?);
    lines.extend(run_child(
        READ_ONLY_CHILD,
        home,
        workspace,
        run,
        Some("registered"),
    )?);
    let after_read_only = counts_now(target, run).await?;
    check(
        after_read_only.sessions == recovered.sessions
            && after_read_only.messages == recovered.messages
            && after_read_only.ledger == recovered.ledger,
        &format!(
            "read-only opens must not write rows or ledger receipts: {}/{}/{} -> {}/{}/{}",
            recovered.sessions,
            recovered.messages,
            recovered.ledger,
            after_read_only.sessions,
            after_read_only.messages,
            after_read_only.ledger
        ),
    )?;
    lines.push(format!(
        "after_read_only sessions={} messages={} ledger={}",
        after_read_only.sessions, after_read_only.messages, after_read_only.ledger
    ));
    let local_read_only = read_local_face(&registry_path(home)).await?;
    check_local_face("after_read_only", &local_read_only, &baseline, &dropped)?;
    check(
        local_read_only == local_recovered,
        &format!(
            "read-only opens must not change a single local fact: {:?} -> {:?}",
            local_recovered, local_read_only
        ),
    )?;
    lines.push(format!(
        "local_after_read_only sessions_rows={}/{}/{}",
        local_read_only.threads, local_read_only.messages, local_read_only.bindings
    ));
    Ok(lines)
}

/// 一次云端计数：每次都用**新连接**读，读完即关（不把父进程的连接留在事实之间）。
async fn counts_now(
    target: &CloudTarget,
    run: &str,
) -> Result<super::cloud_tests::RunCounts, String> {
    let store = target.store(StoreAccess::ReadOnly).await.map_err(failure)?;
    let counts = run_counts(&store, run).await?;
    store.close().await.map_err(failure)?;
    Ok(counts)
}

/// 合成 workspace：系统临时目录里的空 git 仓库（与本地生命周期夹具同一形态）。
///
/// 它只提供「工作区发现」需要的稳定证据（根目录与 `.git` 对象身份），不含任何真实项目
/// 文件，也不在仓库内。
pub(super) fn synthetic_workspace() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temp workspace");
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=synthetic",
            "-c",
            "user.email=synthetic@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "synthetic base",
        ],
    );
    directory
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git fixture could not start");
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 拉起一个子进程阶段：进程边界消失之后仍然成立的事实才是 durable 事实。
///
/// 子进程的输出只回传 `PROBE` 行，并再经一次凭证字面量校验；断言失败会原样带上子进程
/// 输出（子进程的失败信息本身已按同一规则脱敏）。
pub(super) fn run_child(
    test: &str,
    home: &Path,
    workspace: &Path,
    run: &str,
    read_only: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut command = std::process::Command::new(
        std::env::current_exe()
            .map_err(|error| format!("test binary path is unavailable: {error}"))?,
    );
    command
        .args(["--exact", test, "--nocapture"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(HOME_ENV, home)
        .env(WORKSPACE_ENV, workspace)
        .env(RUN_ENV, run);
    if let Some(mode) = read_only {
        command.env(READ_ONLY_ENV, mode);
    }
    let output = command
        .output()
        .map_err(|error| format!("child process could not start: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let phase = read_only
        .map(|mode| format!("{test} ({mode})"))
        .unwrap_or_else(|| test.to_owned());
    if !output.status.success() || !stdout.contains("test result: ok") {
        return Err(format!(
            "child phase failed: {phase}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ));
    }
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("PROBE "))
        .map(str::to_owned)
        .collect())
}
