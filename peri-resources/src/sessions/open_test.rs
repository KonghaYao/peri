//! 打开请求的确定性测试：路径/drive/UNC、`env:` 单次解引用、引擎、凭证与访问意图校验。
//!
//! 全部离线：不连接、不读 `.env`、不建文件。涉及环境变量的用例各用独立变量名，
//! 避免与并行测试互相干扰。

use std::path::PathBuf;

use peri_acp_types::session_resources::{AccessMode, DataCapabilities, SessionResourceErrorKind};
use peri_acp_types::session_store::SessionStoreDeployment;

use crate::sessions::{ReadOnlyStoreErrorKind, ReadOnlyThreadStoreError};

use super::open::{
    AccessIntent, LocatorError, ResolvedLocator, SessionStoreOpenRequest, StorageLocator,
};
use super::remote::{CredentialError, EndpointError, RemoteEngine};

const FAKE_HOST: &str = "sentinel-db-sentinel-org.turso.io";
const FAKE_TOKEN_ENV: &str = "SENTINEL_TOKEN_ENVIRONMENT_NAME";

fn request(
    raw: &str,
    engine: Option<RemoteEngine>,
    token_env: Option<&str>,
) -> SessionStoreOpenRequest {
    SessionStoreOpenRequest::from_locator(raw, engine, token_env, AccessIntent::ReadWrite)
        .expect("locator input")
}

/// 部署参数（CLI 层归一后的中性描述），与 `peri-tui` 的构造方式一致。
fn deployment(
    locator: Option<&str>,
    engine: Option<&str>,
    token_env: Option<&str>,
    access: AccessMode,
) -> SessionStoreDeployment {
    let base = match locator {
        Some(raw) => SessionStoreDeployment::from_locator(raw),
        None => SessionStoreDeployment::default_local(),
    };
    let base = match engine {
        Some(engine) => base.with_engine(engine),
        None => base,
    };
    let base = match token_env {
        Some(name) => base.with_credential_env(name),
        None => base,
    };
    base.with_access(access)
}

#[test]
fn default_input_resolves_to_default_local_store() {
    let default = SessionStoreOpenRequest::local(None, AccessIntent::ReadWrite);
    assert!(matches!(
        default.resolve_locator().expect("resolve"),
        ResolvedLocator::Default
    ));

    let explicit = SessionStoreOpenRequest::local(
        Some(PathBuf::from("/tmp/peri-threads.db")),
        AccessIntent::ReadOnly,
    );
    match explicit.resolve_locator().expect("resolve") {
        ResolvedLocator::Local(path) => assert_eq!(path, PathBuf::from("/tmp/peri-threads.db")),
        other => panic!("expected local resolution, got {other:?}"),
    }
    assert_eq!(explicit.intent(), AccessIntent::ReadOnly);
    assert_eq!(explicit.access(), AccessMode::ReadOnly);
}

#[test]
fn windows_drive_and_unc_stay_paths() {
    for raw in [
        "C:\\work\\threads.db",
        "\\\\server\\share\\threads.db",
        "./threads.db",
    ] {
        let resolved = request(raw, None, None).resolve_locator().expect("resolve");
        match resolved {
            ResolvedLocator::Local(path) => assert_eq!(path, PathBuf::from(raw)),
            other => panic!("{raw} must stay a local path, got {other:?}"),
        }
    }
}

#[test]
fn remote_locator_requires_confirmed_engine_and_credential_source() {
    let ambiguous = request(&format!("https://{FAKE_HOST}"), None, Some(FAKE_TOKEN_ENV))
        .resolve_locator()
        .unwrap_err();
    assert_eq!(
        ambiguous,
        LocatorError::Remote(EndpointError::AmbiguousEngine)
    );

    let no_credential = request(&format!("turso://{FAKE_HOST}"), None, None)
        .resolve_locator()
        .unwrap_err();
    assert_eq!(no_credential, LocatorError::MissingCredentialSource);

    let resolved = request(&format!("turso://{FAKE_HOST}"), None, Some(FAKE_TOKEN_ENV))
        .resolve_locator()
        .expect("resolve");
    match resolved {
        ResolvedLocator::Remote(endpoint) => {
            assert_eq!(endpoint.engine(), RemoteEngine::Turso);
            assert_eq!(endpoint.host_class(), "official_turso_cloud_domain");
        }
        other => panic!("expected remote resolution, got {other:?}"),
    }
}

#[test]
fn credential_source_for_local_store_is_not_ignored() {
    let error = request("/tmp/threads.db", None, Some(FAKE_TOKEN_ENV))
        .resolve_locator()
        .unwrap_err();
    assert_eq!(error, LocatorError::CredentialSourceForLocalStore);
}

#[test]
fn empty_and_unknown_scheme_locators_are_rejected() {
    assert_eq!(
        StorageLocator::parse("   ").unwrap_err(),
        LocatorError::Empty
    );
    assert_eq!(
        request("ftp://example.invalid/db", None, Some(FAKE_TOKEN_ENV))
            .resolve_locator()
            .unwrap_err(),
        LocatorError::Remote(EndpointError::UnsupportedScheme)
    );
}

#[test]
fn env_indirection_resolves_exactly_once() {
    let url_var = "PERI_OPEN_TEST_STORE_URL_4a1";
    std::env::set_var(url_var, format!("turso://{FAKE_HOST}"));

    let resolved = request(&format!("env:{url_var}"), None, Some(FAKE_TOKEN_ENV))
        .resolve_locator()
        .expect("resolve");
    assert!(matches!(resolved, ResolvedLocator::Remote(_)));

    // 指向另一个引用：只解引用一次，不递归。
    std::env::set_var(url_var, "env:PERI_OPEN_TEST_OTHER_4a1");
    assert_eq!(
        request(&format!("env:{url_var}"), None, Some(FAKE_TOKEN_ENV))
            .resolve_locator()
            .unwrap_err(),
        LocatorError::EnvValueIsReference {
            name: url_var.to_owned()
        }
    );

    std::env::set_var(url_var, "");
    assert_eq!(
        request(&format!("env:{url_var}"), None, Some(FAKE_TOKEN_ENV))
            .resolve_locator()
            .unwrap_err(),
        LocatorError::EnvValueEmpty {
            name: url_var.to_owned()
        }
    );

    std::env::remove_var(url_var);
    assert_eq!(
        request(&format!("env:{url_var}"), None, Some(FAKE_TOKEN_ENV))
            .resolve_locator()
            .unwrap_err(),
        LocatorError::EnvValueMissing {
            name: url_var.to_owned()
        }
    );

    // 变量名不合法：与凭证来源同一套校验规则。
    assert_eq!(
        StorageLocator::parse("env:not a name").unwrap_err(),
        LocatorError::InvalidEnvReference
    );
}

#[test]
fn request_debug_preserves_remote_locator() {
    let request = request(
        &format!("turso://{FAKE_HOST}"),
        Some(RemoteEngine::Turso),
        Some(FAKE_TOKEN_ENV),
    );
    let rendered = format!("{request:?}");
    assert!(rendered.contains(FAKE_TOKEN_ENV)); // 变量名本身不是凭证
    assert_eq!(
        request.credential_source().expect("source").name(),
        FAKE_TOKEN_ENV
    );
    assert!(!rendered.contains("sentinel-db"));
    assert!(!rendered.contains("remote locator redacted"));
}

#[test]
fn engine_names_are_a_fixed_set() {
    assert_eq!(RemoteEngine::parse("turso"), Some(RemoteEngine::Turso));
    assert_eq!(RemoteEngine::parse("libsql"), Some(RemoteEngine::LibSql));
    assert_eq!(RemoteEngine::parse("mysql"), None);
}

/// 部署参数携带的访问模式直接映射为访问意图（部署面无拼写输入，因此无解析失败面）。
#[test]
fn deployment_access_mode_maps_to_intent() {
    assert_eq!(
        SessionStoreOpenRequest::from_deployment(&deployment(
            None,
            None,
            None,
            AccessMode::ReadOnly
        ))
        .expect("default local request")
        .intent(),
        AccessIntent::ReadOnly
    );
    assert_eq!(
        SessionStoreOpenRequest::from_deployment(&deployment(
            Some("/tmp/threads.db"),
            None,
            None,
            AccessMode::ReadWrite
        ))
        .expect("local path request")
        .intent(),
        AccessIntent::ReadWrite
    );

    assert_eq!(AccessIntent::ReadOnly.mode(), AccessMode::ReadOnly);
    assert_eq!(AccessIntent::ReadWrite.mode(), AccessMode::ReadWrite);
    assert!(AccessIntent::ReadOnly.is_read_only());
    assert!(!AccessIntent::ReadWrite.is_read_only());
    assert_eq!(format!("{:?}", AccessIntent::ReadWrite), "ReadWrite");
    assert_eq!(
        AccessIntent::from_access_mode(AccessMode::ReadOnly),
        AccessIntent::ReadOnly
    );
}

/// 部署参数到 typed request 的转换是纯解析：互相矛盾的配置在进入任何 I/O 之前失败。
#[tokio::test]
async fn deployment_conflicts_fail_before_any_io() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nested").join("threads.db");

    // 未知引擎名：不猜、不轮流试，直接配置错误。
    for (name, deploy) in [
        (
            "unknown engine",
            deployment(
                Some(missing.to_str().unwrap()),
                Some("mysql"),
                None,
                AccessMode::ReadWrite,
            ),
        ),
        (
            "engine without locator",
            deployment(None, Some("turso"), None, AccessMode::ReadWrite),
        ),
        (
            "credential without locator",
            deployment(None, None, Some(FAKE_TOKEN_ENV), AccessMode::ReadWrite),
        ),
        (
            "credential for local path",
            deployment(
                Some(missing.to_str().unwrap()),
                None,
                Some(FAKE_TOKEN_ENV),
                AccessMode::ReadWrite,
            ),
        ),
        (
            "engine for local path",
            deployment(
                Some(missing.to_str().unwrap()),
                Some("libsql"),
                None,
                AccessMode::ReadWrite,
            ),
        ),
    ] {
        let error = match crate::Resources::open_deployment(&deploy).await {
            Ok(_) => panic!("{name} 必须报配置错误"),
            Err(error) => error,
        };
        assert!(
            error.downcast_ref::<LocatorError>().is_some(),
            "{name} 必须保持类型化: {error}"
        );
        assert!(
            !missing.parent().expect("parent").exists(),
            "{name} 不得进入任何 I/O"
        );
    }
}

/// 环境里存在云 URL/token 变量不会切换后端：默认与本机路径都仍是本机库。
#[tokio::test]
async fn cloud_environment_variables_do_not_switch_the_backend() {
    let url_var = "PERI_OPEN_TEST_CLOUD_URL_7c2";
    let token_var = "PERI_OPEN_TEST_CLOUD_TOKEN_7c2";
    std::env::set_var(url_var, format!("turso://{FAKE_HOST}"));
    std::env::set_var(token_var, "sentinel-credential-222222222222");

    // 请求层：默认路径解析成本机 Default，不因环境里的远程 URL 变成 Remote。
    assert!(matches!(
        SessionStoreOpenRequest::local(None, AccessIntent::ReadWrite)
            .resolve_locator()
            .expect("resolve"),
        ResolvedLocator::Default
    ));

    // 打开层：本机路径仍打开本机 SQLite（不报远程错误、不建远程连接）。
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let resources = crate::Resources::open_with(Some(db_path.clone()))
        .await
        .expect("环境变量存在不得影响本机打开");
    let availability = resources
        .session_resources()
        .inspect_availability(None)
        .await
        .expect("availability");
    assert_eq!(availability.access, AccessMode::ReadWrite);
    assert!(db_path.is_file(), "本机路径请求必须打开本机库");

    // 变量值未被任何分支读取：只有 `env:` 引用才会读环境。
    assert!(std::env::var(url_var).is_ok());
    std::env::remove_var(url_var);
    std::env::remove_var(token_var);
}

/// 带凭证的 URL 在打开入口就被拒绝：错误不回显 locator 原文与凭证值。
#[tokio::test]
async fn url_with_embedded_secret_is_rejected_without_echo() {
    let secret = "sentinel-credential-333333333333";
    let raw = format!("turso://user:{secret}@{FAKE_HOST}");
    // 请求层：带凭证 URL 在解析 locator 时按稳定分类拒绝（词法层不替它做判断）。
    let request = SessionStoreOpenRequest::from_locator(
        &raw,
        None,
        Some(FAKE_TOKEN_ENV),
        AccessIntent::ReadWrite,
    )
    .expect("词法层只做 env:/原文分流");
    assert_eq!(
        request.resolve_locator().unwrap_err(),
        LocatorError::Remote(EndpointError::UserInfoPresent)
    );

    let error = match crate::Resources::open_deployment(&deployment(
        Some(&raw),
        None,
        Some(FAKE_TOKEN_ENV),
        AccessMode::ReadWrite,
    ))
    .await
    {
        Ok(_) => panic!("带凭证 URL 必须被拒绝"),
        Err(error) => error,
    };
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("must not carry credentials"),
        "必须给出稳定分类: {rendered}"
    );
    assert!(!rendered.contains(secret), "错误不得回显凭证: {rendered}");
    assert!(
        !rendered.contains("sentinel-db"),
        "错误不得回显 locator 原文: {rendered}"
    );
}

/// `file://` 不被静默当成本机路径：未确认的 scheme 直接拒绝。
#[test]
fn file_uri_is_not_silently_read_as_a_path() {
    assert_eq!(
        request("file:///tmp/threads.db", None, None)
            .resolve_locator()
            .unwrap_err(),
        LocatorError::Remote(EndpointError::UnsupportedScheme)
    );
}

/// 显式只读不产生副作用：库不存在时按类型化错误失败，不创建父目录与库文件。
#[tokio::test]
async fn explicit_read_only_does_not_create_anything() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nested").join("threads.db");

    let error = match crate::Resources::open_deployment(&deployment(
        Some(missing.to_str().unwrap()),
        None,
        None,
        AccessMode::ReadOnly,
    ))
    .await
    {
        Ok(_) => panic!("库不存在时只读打开必须失败"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains(&missing.display().to_string()),
        "只读失败必须携带路径: {message}"
    );
    // 失败分类留在 source chain：消费侧可按 kind 映射退出码（D-04），不必解析文本。
    assert_eq!(
        error
            .downcast_ref::<ReadOnlyThreadStoreError>()
            .map(ReadOnlyThreadStoreError::kind),
        Some(ReadOnlyStoreErrorKind::DatabaseNotFound)
    );
    assert!(!missing.exists(), "显式只读不得创建库文件");
    assert!(
        !missing.parent().expect("parent").exists(),
        "显式只读不得创建父目录"
    );

    let writable = crate::Resources::open_deployment(&deployment(
        Some(missing.to_str().unwrap()),
        None,
        None,
        AccessMode::ReadWrite,
    ))
    .await
    .expect("写意图按既有语义创建库");
    assert!(missing.is_file(), "写意图必须创建库文件");
    drop(writable);
}

/// 显式只读打开已存在的库与普通打开共享同一后端选择点：选出的只读后端不写任何文件、
/// 不登记本机执行身份，写入在副作用前按只读拒绝。
#[tokio::test]
async fn explicit_read_only_shares_selection_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let locator = db_path.to_str().unwrap();
    drop(
        crate::Resources::open_deployment(&deployment(
            Some(locator),
            None,
            None,
            AccessMode::ReadWrite,
        ))
        .await
        .expect("写意图创建库"),
    );

    let before = listing(dir.path());
    let read_only = crate::Resources::open_deployment(&deployment(
        Some(locator),
        None,
        None,
        AccessMode::ReadOnly,
    ))
    .await
    .expect("已存在的库可显式只读打开");
    let availability = read_only
        .session_resources()
        .inspect_availability(None)
        .await
        .expect("availability");
    assert_eq!(availability.access, AccessMode::ReadOnly);
    assert_eq!(availability.capabilities, DataCapabilities::HistoryReadOnly);
    assert_eq!(before, listing(dir.path()), "显式只读不得新增任何文件");
    // 写入在副作用前拒绝：不假装可写。
    let error = read_only
        .session_resources()
        .delete_session_tree(&"no-such-session".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
}

fn listing(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn remote_open_is_not_silently_downgraded_to_local() {
    // 远程组合已接线（`sessions::remote::composition`）。这里断言的是**配置不完整时
    // 在任何 I/O 之前失败**：凭证来源缺失、引擎名不认识，都不连接远端、不开本机库，
    // 也绝不静默回落到本机存储。
    let error = match crate::Resources::open_deployment(&deployment(
        Some(&format!("turso://{FAKE_HOST}")),
        None,
        Some(FAKE_TOKEN_ENV),
        AccessMode::ReadOnly,
    ))
    .await
    {
        Ok(_) => panic!("remote store must not open as a local store"),
        Err(error) => error,
    };
    // 凭证来源没有设置：配置不完整必须在**任何 I/O 之前**失败（不连接远端、不打开或
    // 创建本机登记库、不建锁文件），并且保留类型化分类供消费侧映射退出码。
    // 组合层里凭证解析是第一步，这里断言失败确实停在它上面（不是被压平的字符串）。
    assert!(
        error.downcast_ref::<CredentialError>().is_some(),
        "缺凭证必须是类型化配置错误: {error}"
    );
    assert!(
        format!("{error:#}").contains("SENTINEL_TOKEN_ENVIRONMENT_NAME"),
        "错误链里只应出现变量名: {error:#}"
    );

    // 未知引擎名在解析期失败，不进入任何 I/O。
    let error = match crate::Resources::open_deployment(&deployment(
        Some(&format!("turso://{FAKE_HOST}")),
        Some("mysql"),
        Some(FAKE_TOKEN_ENV),
        AccessMode::ReadWrite,
    ))
    .await
    {
        Ok(_) => panic!("unknown engine must be a config error"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("unknown session store engine"),
        "未知引擎名必须报配置错误: {error}"
    );
    assert!(
        error.downcast_ref::<LocatorError>().is_some(),
        "配置错误必须保持类型化: {error}"
    );
}

/// 各部署入口等价：既有 `--db-path` 兼容入口（`open_with`）与部署参数入口
/// （`open_deployment`，本机路径 / 显式 locator）只共享一个后端选择点——相同的库、
/// 相同的访问模式与能力，不是各自解释出另一份存储。
#[tokio::test]
async fn deployment_entries_agree_on_one_selection_point() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let raw = db_path.to_str().unwrap().to_owned();

    // 1) 既有 `--db-path` 兼容入口。
    let legacy = crate::Resources::open_with(Some(db_path.clone()))
        .await
        .expect("legacy --db-path entry");
    let legacy_availability = legacy
        .session_resources()
        .inspect_availability(None)
        .await
        .expect("availability");
    drop(legacy);

    // 2) `--session-store <本机路径>` 部署参数。
    let via_locator = crate::Resources::open_deployment(&deployment(
        Some(&raw),
        None,
        None,
        AccessMode::ReadWrite,
    ))
    .await
    .expect("--session-store entry");
    let locator_availability = via_locator
        .session_resources()
        .inspect_availability(None)
        .await
        .expect("availability");
    drop(via_locator);

    // 3) `--db-path` 归一后的部署参数（同一份 path 语义）。
    let via_path =
        crate::Resources::open_deployment(&SessionStoreDeployment::local_path(db_path.clone()))
            .await
            .expect("deployment local path entry");
    let path_availability = via_path
        .session_resources()
        .inspect_availability(None)
        .await
        .expect("availability");
    drop(via_path);

    assert_eq!(legacy_availability.access, AccessMode::ReadWrite);
    for availability in [locator_availability, path_availability] {
        assert_eq!(availability.access, legacy_availability.access);
        assert_eq!(availability.capabilities, legacy_availability.capabilities);
    }
    assert!(db_path.is_file(), "三个入口必须落在同一个库文件");
}

// ─── `--db-path` 归一出的是已确认本机路径，不进入 locator 解析 ──────────────

/// 原缺陷回归：`--db-path` 归一路径曾被写进 locator 字符串、再按 `env:` 解引用，
/// 名为 `env:<名字>` 的合法文件因而打不开（报「环境变量未设置」）。
#[tokio::test]
async fn db_path_file_named_like_an_env_reference_opens_as_a_file() {
    let name = "PERI_OPEN_TEST_UNSET_9d1";
    assert!(
        std::env::var_os(name).is_none(),
        "用例要求 {name} 未被设置：按引用解释必然失败"
    );
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(format!("env:{name}"));

    // 请求层：不解引用，原样是路径。
    let request = SessionStoreOpenRequest::from_deployment(&SessionStoreDeployment::local_path(
        db_path.clone(),
    ))
    .expect("--db-path deployment");
    match request.resolve_locator().expect("resolve") {
        ResolvedLocator::Local(path) => assert_eq!(path, db_path),
        other => panic!("--db-path 必须保持本机路径，得到 {other:?}"),
    }

    // 打开层：真的把该文件当库打开（写打开会创建它），不报配置错误。
    let resources =
        crate::Resources::open_deployment(&SessionStoreDeployment::local_path(db_path.clone()))
            .await
            .expect("open（--db-path 形态）");
    drop(resources);
    assert!(db_path.is_file(), "必须是本机文件，而不是环境变量引用");
}

/// Unix 非 UTF-8 文件名：字节原样到达打开层，且**如实失败**。两种平台的失败原因都在
/// 原始字节路径上（实跑）：macOS 的 syscall 直接拒绝（EILSEQ，os error 92）；Linux 的
/// syscall 接受，由打开层的 sqlx 拒绝——它要求 SQLite 文件名是合法 UTF-8
/// （`EstablishParams::from_options`，无平台分支）。旧实现经 `to_string_lossy` 改写后会在
/// U+FFFD 变体上另建一个库，静默打开错误的库，因此失败必须落在原始字节路径上。
#[cfg(unix)]
#[tokio::test]
async fn db_path_keeps_non_utf8_bytes_end_to_end() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(OsStr::from_bytes(b"threads-\xff\xfe.db"));
    assert!(db_path.to_str().is_none(), "用例本身要求非 UTF-8 路径");

    let request = SessionStoreOpenRequest::from_deployment(&SessionStoreDeployment::local_path(
        db_path.clone(),
    ))
    .expect("--db-path deployment");
    match request.resolve_locator().expect("resolve") {
        ResolvedLocator::Local(path) => {
            assert_eq!(path.as_os_str().as_bytes(), db_path.as_os_str().as_bytes());
        }
        other => panic!("非 UTF-8 路径必须保持本机路径，得到 {other:?}"),
    }

    let opened =
        crate::Resources::open_deployment(&SessionStoreDeployment::local_path(db_path.clone()))
            .await;
    // 如实失败，不改写成另一个路径后"成功"：macOS 在 syscall 层拒绝（EILSEQ，
    // os error 92），Linux 的 syscall 接受、由打开层 sqlx 的 UTF-8 文件名要求拒绝。
    let error = match opened {
        Ok(_) => panic!("非 UTF-8 路径必须如实失败：不得改写成 lossy 路径后成功"),
        Err(error) => error,
    };
    // 具体原因按平台不同，因此只断言失败出在「打开这个库」这一步：不得是部署解析
    // （locator / 引擎名 / 凭证）之类的另一条路径，也不把某平台的措辞写死。
    assert!(
        error.to_string().contains("无法打开指定 SQLite 数据库"),
        "必须是打开层的失败，得到 {error}"
    );

    let lossy_variant = dir
        .path()
        .join(String::from_utf8_lossy(b"threads-\xff\xfe.db").as_ref());
    assert!(
        !lossy_variant.exists(),
        "不得在 to_string_lossy 变体上建库：那等于静默打开另一个库"
    );
}

/// `--db-path` 的 Windows drive/UNC 形状不进入 URL 判定。本机不必是 Windows：这里
/// 只验证类型分派与字节保留，不断言这些路径在 macOS/Linux 上可打开。
#[test]
fn db_path_windows_shapes_skip_locator_parsing() {
    for raw in ["C:\\work\\threads.db", "\\\\server\\share\\threads.db"] {
        let request = SessionStoreOpenRequest::from_deployment(
            &SessionStoreDeployment::local_path(PathBuf::from(raw)),
        )
        .expect("--db-path deployment");
        match request.resolve_locator().expect("resolve") {
            ResolvedLocator::Local(path) => assert_eq!(path, PathBuf::from(raw)),
            other => panic!("{raw} 必须保持本机路径，得到 {other:?}"),
        }
    }
}

/// 同一段 `env:` 字面量：`--db-path` 当文件名，`--session-store` 当环境引用，两种语义
/// 不互相渗透。
#[test]
fn env_colon_literal_is_a_file_name_for_db_path_but_a_reference_for_session_store() {
    let name = "PERI_OPEN_TEST_ENV_LITERAL_6f4";
    assert!(std::env::var_os(name).is_none(), "用例要求 {name} 未被设置");

    let as_path = SessionStoreOpenRequest::from_deployment(&SessionStoreDeployment::local_path(
        PathBuf::from(format!("env:{name}")),
    ))
    .expect("--db-path deployment");
    match as_path.resolve_locator().expect("resolve") {
        ResolvedLocator::Local(path) => assert_eq!(path, PathBuf::from(format!("env:{name}"))),
        other => panic!("--db-path 的字面名必须保持路径，得到 {other:?}"),
    }

    let as_reference = SessionStoreOpenRequest::from_deployment(
        &SessionStoreDeployment::from_locator(format!("env:{name}")),
    )
    .expect("--session-store deployment");
    assert_eq!(
        as_reference.resolve_locator().unwrap_err(),
        LocatorError::EnvValueMissing {
            name: name.to_owned()
        }
    );
}

/// 已确认本机路径不接受远程专用参数：给出即配置错误，不静默忽略。
#[test]
fn remote_only_parameters_on_confirmed_local_path_fail_early() {
    let base = SessionStoreDeployment::local_path(PathBuf::from("/tmp/threads.db"));

    let with_engine = base.clone().with_engine("turso");
    assert_eq!(
        SessionStoreOpenRequest::from_deployment(&with_engine).unwrap_err(),
        LocatorError::EngineForLocalStore
    );

    let with_credential = base.with_credential_env(FAKE_TOKEN_ENV);
    assert_eq!(
        SessionStoreOpenRequest::from_deployment(&with_credential).unwrap_err(),
        LocatorError::CredentialSourceForLocalStore
    );
}
