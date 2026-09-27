//! context.rs 单元测试：`Resources::open_with` 显式路径语义。

#[cfg(unix)]
use std::path::PathBuf;

use tempfile::tempdir;

use peri_acp_types::messages::BaseMessage;
#[cfg(unix)]
use peri_acp_types::session_resources::AccessMode;
use peri_acp_types::store::{PersistedPayload, ThreadStore};
use peri_acp_types::thread::ThreadMeta;
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};

use super::*;
use crate::sessions::{CredentialError, SqliteThreadStore};

/// [P0] 显式路径打开成功：数据库文件被创建，且同路径二次打开幂等。
#[tokio::test]
async fn test_open_with_explicit_path_creates_db() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("custom").join("threads.db");
    let first = Resources::open_with(Some(db_path.clone())).await.unwrap();
    assert!(
        tokio::fs::metadata(&db_path).await.is_ok(),
        "数据库文件应已创建: {}",
        db_path.display()
    );
    let second = Resources::open_with(Some(db_path)).await;
    assert!(
        second.is_ok(),
        "同路径二次打开应幂等成功: {:?}",
        second.err()
    );
    drop(first);
}

/// [P0] 显式路径不可用（父级为普通文件）时直接报错，不 fallback 临时目录，错误携带路径。
#[tokio::test]
async fn test_open_with_explicit_path_errors_no_fallback() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, "not a directory").unwrap();
    let db_path = file.join("threads.db");
    let err = match Resources::open_with(Some(db_path)).await {
        Ok(_) => panic!("父级为普通文件时应返回错误"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains(&file.display().to_string()),
        "错误必须携带路径: {err}"
    );
}

/// [P0] 显式路径指向目录时直接报错（sqlite 无法以目录为库），错误携带路径。
#[tokio::test]
async fn test_open_with_explicit_path_is_directory_errs() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("adir");
    std::fs::create_dir(&db_path).unwrap();
    let err = match Resources::open_with(Some(db_path.clone())).await {
        Ok(_) => panic!("指向目录的路径应返回错误"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains(&db_path.display().to_string()),
        "错误必须携带路径: {err}"
    );
}

/// [P0] 库的 `user_version` 本构建不认识时拒绝打开，用户可见的报错要复述实际版本与
/// 本构建上限——这正是「进入时只看到不支持」的那条链路。
#[tokio::test]
async fn test_open_with_explicit_path_reports_unsupported_schema_version() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("PRAGMA user_version = 99")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let err = match Resources::open_with(Some(db_path.clone())).await {
        Ok(_) => panic!("不认识的 schema 版本必须拒绝打开"),
        Err(e) => e,
    };
    let message = err.to_string();
    assert!(
        message.contains(&db_path.display().to_string()),
        "错误必须携带路径: {message}"
    );
    assert!(
        message.contains("version 99"),
        "错误必须复述实际版本: {message}"
    );
    assert!(
        message.contains("newest supported:"),
        "错误必须给出本构建上限: {message}"
    );
}

/// [P1] `open_with(None)` 使用默认数据库且可正常查询。
#[tokio::test]
async fn test_open_with_none_uses_default_store() {
    // 复用生产路径选择逻辑，只注入默认存储位置，禁止测试迁移用户真实数据库。
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("default.db");
    let default = db_path.clone();
    let resources = Resources::open_with_default(None, move || Ok(default))
        .await
        .unwrap();
    assert!(db_path.is_file(), "None 分支必须打开注入的默认存储");
    let page = resources
        .session_resources()
        .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
            scope: peri_acp_types::workspace::ThreadScope::All,
            cursor: None,
            limit: u32::MAX,
        })
        .await;
    assert!(page.is_ok(), "默认存储应可查询: {:?}", page.err());
}

/// 会话库被占（schema 锁未释放）：写打开失败不再挡住进入，降级为只读打开——
/// 历史仍可列表读取，写入由只读 store 自己按只读失败。
#[tokio::test]
async fn test_open_with_busy_schema_lock_degrades_to_read_only() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let writable = SqliteThreadStore::new(db_path.clone()).await.unwrap();
    let thread = writable
        .create_thread(ThreadMeta::new("/tmp/read-only-degradation"))
        .await
        .unwrap();
    // 门面列表语义只收已有历史的会话（`message_count > 0`）：裸建的空 thread 不在
    // 列表里，夹具先落一条历史，断言才是在测降级后的可见性。
    writable
        .append_message(&thread, BaseMessage::human("read-only degradation history"))
        .await
        .unwrap();
    writable.close().await;

    // 持住 schema 锁：写打开按「初始化被占」失败，只读打开不受影响。
    let canonical = db_path.canonicalize().unwrap();
    let lock_path = canonical.with_file_name(format!(
        "{}.schema-lock",
        canonical.file_name().unwrap().to_string_lossy()
    ));
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.lock().unwrap();

    let resources = Resources::open_with(Some(db_path.clone())).await.unwrap();
    // 只读降级的「列表仍可读」由门面回答；夹具不需要裸句柄。
    let listed = resources
        .session_resources()
        .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
            scope: peri_acp_types::workspace::ThreadScope::All,
            cursor: None,
            limit: u32::MAX,
        })
        .await
        .unwrap()
        .entries;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].thread.id, thread);
    // 只读句柄用于断言「不得假装可写」：与降级后的门面同库、同为只读连接。
    let store = SqliteThreadStore::open_existing_read_only(&db_path)
        .await
        .unwrap();
    assert!(
        store.delete_thread(&thread).await.is_err(),
        "只读降级不得假装可写：写入必须失败"
    );
    assert!(store.load_meta(&thread).await.is_ok(), "降级后历史仍可读");
    drop(held);
}

fn git_repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        let output = std::process::Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", directory.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(directory.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git fixture failed");
    }
    directory
}

/// [P0] 迁移桥与门面必须共享同一库句柄：桥取得的执行权要能被门面的写入准入承认。
///
/// 这是本阶段的前提条件——ACP 仍经桥取得 owner，Agent 已改走门面写入。两者若各自
/// 建一份连接与 owner 登记，门面会把正在跑的会话判成「无主」而拒绝写入；因此
/// 资源测试入口 `open_store_and_facade_for_tests` 仍提供裸句柄供夹具逐条断言。
#[tokio::test]
async fn test_bridge_lease_is_visible_to_shared_facade() {
    let repo = git_repository();
    let db_dir = tempdir().unwrap();
    let (store, facade) =
        crate::sessions::open_store_and_facade_for_tests(db_dir.path().join("threads.db"))
            .await
            .unwrap();

    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let thread = store
        .create_bound_thread(
            ThreadMeta::new(workspace.cwd.to_string_lossy().into_owned()),
            &workspace,
        )
        .await
        .unwrap();
    let lease = store.acquire_execution_lease(&thread).await.unwrap();

    facade
        .append_history(
            &thread,
            &[PersistedPayload::Message(BaseMessage::human(
                "shared handle",
            ))],
        )
        .await
        .expect("桥取得的 owner 必须被门面写入准入承认");

    let snapshot = facade.load_session_snapshot(&thread).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 1);
    assert_eq!(snapshot.meta.id, thread);
    drop(lease);
}

/// 部署所有权交付：工厂只在这里交出「业务句柄 + 部署关闭权」。
///
/// 断言两件可观察事实：业务句柄在关闭之前完全可用；唯一关闭权被消费之后，同一业务句柄
/// 的新写入被明确拒绝（关闭是真实的，不是形式上的所有权搬运），读取仍然可用。
#[tokio::test]
async fn test_into_parts_hands_out_business_handle_and_deployment_close_owner() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let resources = Resources::open_with(Some(db_path)).await.unwrap();
    let (business, owner) = resources.into_parts();

    // 关闭之前：业务句柄照常回答问题（同一次打开、同一个库句柄）。
    business
        .inspect_availability(None)
        .await
        .expect("an open store answers availability");

    // 唯一关闭权消费一次：确认关闭之后，同一业务句柄不再接受新写入。
    owner
        .shutdown()
        .await
        .expect("no live session: the store must close cleanly");
    let error = business
        .append_history(
            &"closed-session".to_owned(),
            &[PersistedPayload::Message(BaseMessage::human("after close"))],
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::Unavailable { .. }
    ));
}

// ─── 打开失败的分类：凭证来源问题按类型归配置错误 ──────────────────────────────

/// 显式不会存在的凭证变量名：表达「来源已给、变量/值确实没有」。
const ABSENT_CREDENTIAL_ENV: &str = "PERI_CONTEXT_TEST_ABSENT_TOKEN_ENV";
/// 子进程受控 HOME 的守卫变量：只在父用例拉起时出现。
#[cfg(unix)]
const HOME_GUARD_ENV: &str = "PERI_CONTEXT_TEST_REMOTE_CREDENTIAL_HOME";
/// locator 哨兵：断言错误输出不回显它。
#[cfg(unix)]
const LOCATOR_SENTINEL: &str = "turso://sentinel-db-sentinel-org.turso.io";
/// 子进程跑到断言的标记（父用例据此确认证据真的产生了）。
#[cfg(unix)]
const CHILD_REACHED_MARKER: &str = "context-child-verified-no-side-effects";

/// 凭证来源的每一种问题（名字非法、未设置、空值、非 Unicode、注入空值）都按**类型**归
/// 「配置错误」：被 context 包成 source chain 之后仍被认出，分类不解析错误文本。
#[test]
fn test_classify_open_failure_sees_wrapped_credential_errors() {
    let cases = [
        CredentialError::InvalidName,
        CredentialError::Missing {
            name: ABSENT_CREDENTIAL_ENV.to_owned(),
        },
        CredentialError::Empty {
            name: ABSENT_CREDENTIAL_ENV.to_owned(),
        },
        CredentialError::NotUnicode {
            name: ABSENT_CREDENTIAL_ENV.to_owned(),
        },
        CredentialError::EmptyValue,
    ];

    for error in cases {
        let wrapped = anyhow::Error::new(error).context("无法打开远程会话存储");
        assert!(
            wrapped
                .chain()
                .any(|cause| cause.downcast_ref::<CredentialError>().is_some()),
            "凭证问题必须留在 source chain 上：{wrapped:?}"
        );
        assert_eq!(
            classify_open_failure(&wrapped),
            StoreOpenFailure::NotConfigured
        );
    }
}

/// 分类不被「有 context」放大：链上没有凭证问题的失败仍是内部错误。
#[test]
fn test_classify_open_failure_keeps_untyped_failures_internal() {
    let unrelated = anyhow::anyhow!("no typed cause here").context("无法打开远程会话存储");
    assert_eq!(
        classify_open_failure(&unrelated),
        StoreOpenFailure::Internal
    );
}

/// 缺凭证必须在**任何库/登记副作用之前**失败：父用例把 HOME 指到临时目录拉起子进程，
/// 子进程真跑 `Resources::open_deployment`（远程 locator、变量显式不存在、只读意图）。
#[cfg(unix)]
#[tokio::test]
async fn test_missing_remote_credential_fails_before_registry_side_effects() {
    let home = tempdir().unwrap();
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "context::tests::test_missing_remote_credential_child_process",
            "--nocapture",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env(HOME_GUARD_ENV, home.path())
        .env_remove(ABSENT_CREDENTIAL_ENV)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("running 1 test"), "{stdout}");
    assert!(
        stdout.contains(CHILD_REACHED_MARKER),
        "子进程没跑到断言，本用例没有取得证据：{stdout}"
    );
    assert_eq!(
        std::fs::read_dir(home.path()).unwrap().count(),
        0,
        "缺凭证之前不得留下任何文件或目录"
    );
}

/// 只在父用例的受控环境里执行；普通全量跑（无守卫变量）直接返回，不动进程环境。
#[cfg(unix)]
#[tokio::test]
async fn test_missing_remote_credential_child_process() {
    let Some(home) = std::env::var_os(HOME_GUARD_ENV) else {
        return;
    };
    let home = PathBuf::from(home);
    // 受控环境：变量确实不存在——不读 `.env`，也不会有任何真实网络调用。
    std::env::remove_var(ABSENT_CREDENTIAL_ENV);
    assert!(std::env::var_os(ABSENT_CREDENTIAL_ENV).is_none());
    // HOME 控制已生效：默认登记库位置指向临时目录（只解析，不创建）。
    assert_eq!(
        Resources::local_registry_path().unwrap(),
        home.join(".peri").join("threads").join("threads.db")
    );

    let deployment = SessionStoreDeployment::from_locator(LOCATOR_SENTINEL)
        .with_credential_env(ABSENT_CREDENTIAL_ENV)
        .with_access(AccessMode::ReadOnly);
    let error = match Resources::open_deployment(&deployment).await {
        Ok(_) => panic!("缺凭证必须失败"),
        Err(error) => error,
    };
    assert_eq!(
        classify_open_failure(&error),
        StoreOpenFailure::NotConfigured
    );
    // 错误里只允许出现「来源名」（`CredentialError` 的类型里根本没有凭证值字段，
    // `SessionStoreCredential` 也不实现 `Debug`），locator 原文与凭证值一律不得出现。
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains("sentinel-db-sentinel-org"),
        "不回显 locator: {rendered}"
    );
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "缺凭证必须在建目录/开库/写侧车之前失败"
    );
    println!("{CHILD_REACHED_MARKER}");
}
