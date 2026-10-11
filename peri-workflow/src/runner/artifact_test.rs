//! artifact 解析、身份/摘要校验与缓存发布的单元测试。
//!
//! 夹具**完全自含**：临时目录中的合成包、测试内自算 sha256，经 [`EntryPin`] 注入期望
//! 摘要。任何用例都不读取仓库内文件（dist 不入库，也不读工作树 dist）。

use std::path::{Path, PathBuf};

use super::{
    artifact_id, network_fetch_allowed_from, prepare_workflow_command_with, publish_to_cache_in,
    resolve_artifact, sha256_hex, ArtifactOrigin, ArtifactSources, EntryPin, WORKFLOW_BUILD_ID,
    WORKFLOW_ENTRY, WORKFLOW_ENTRY_SHA256, WORKFLOW_NPM_VERSION, WORKFLOW_PACKAGE_NAME,
    WORKFLOW_PROTOCOL_VERSION,
};

/// 合成入口字节：可被 `node` 直接执行的最小脚本（内容任意，摘要由测试内计算）。
fn synthetic_entry(seed: &str) -> Vec<u8> {
    format!("// synthetic engine fixture ({seed})\nprocess.exit(0)\n").into_bytes()
}

/// 按给定元数据与入口字节写入包目录（身份字段取编译期常量，仅 `version`/`main` 可调）。
fn write_package(package: &Path, entry_bytes: &[u8], version: &str, main: &str) {
    std::fs::create_dir_all(package.join("dist")).unwrap();
    std::fs::write(package.join(WORKFLOW_ENTRY), entry_bytes).unwrap();
    std::fs::write(
        package.join("package.json"),
        serde_json::json!({
            "name": WORKFLOW_PACKAGE_NAME,
            "version": version,
            "main": main,
            "periProtocolVersion": WORKFLOW_PROTOCOL_VERSION,
            "periBuildId": WORKFLOW_BUILD_ID,
        })
        .to_string(),
    )
    .unwrap();
}

/// 身份合法且摘要与 `entry_bytes` 一致的合成包目录。
fn valid_package(parent: &Path, name: &str, seed: &str) -> PathBuf {
    let package = parent.join(name);
    write_package(
        &package,
        &synthetic_entry(seed),
        WORKFLOW_NPM_VERSION,
        WORKFLOW_ENTRY,
    );
    package
}

/// 只含一个显式注入来源（合成包）的解析输入，pin 取该包入口的自算摘要。
fn sources_for(package: &Path, seed: &str) -> ArtifactSources {
    ArtifactSources {
        override_dir: Some(package.to_path_buf()),
        entry_pin: EntryPin(sha256_hex(&synthetic_entry(seed))),
        ..ArtifactSources::default()
    }
}

#[test]
fn entry_pin_defaults_to_the_compiled_constant() {
    assert_eq!(EntryPin::default().0, WORKFLOW_ENTRY_SHA256);
}

#[test]
fn resolve_reports_missing_when_no_source_is_available() {
    let sources = ArtifactSources::default();
    assert!(resolve_artifact(&sources).is_none());
}

#[test]
fn resolve_prefers_override_then_cache_then_sidecar() {
    let tmp = tempfile::TempDir::new().unwrap();
    let seed = "precedence";
    let entry_bytes = synthetic_entry(seed);
    let pin = EntryPin(sha256_hex(&entry_bytes));

    let override_dir = valid_package(tmp.path(), "override", seed);
    // 缓存包目录 = `<前缀>/<内容身份>`，与 `publish_to_cache_in` 的发布目标一致。
    let cache_prefix = tmp.path().join("cache");
    let cached = cache_prefix.join(artifact_id());
    write_package(&cached, &entry_bytes, WORKFLOW_NPM_VERSION, WORKFLOW_ENTRY);
    let sidecar = valid_package(tmp.path(), "sidecar", seed);

    let resolved = resolve_artifact(&ArtifactSources {
        override_dir: Some(override_dir.clone()),
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: pin.clone(),
    })
    .expect("显式注入应优先命中");
    assert_eq!(resolved.origin, ArtifactOrigin::Override);
    assert_eq!(resolved.package_dir, override_dir);
    assert_eq!(resolved.entry, override_dir.join(WORKFLOW_ENTRY));

    let resolved = resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix),
        entry_pin: pin.clone(),
    })
    .expect("缓存应优先于随包 sidecar");
    assert_eq!(resolved.origin, ArtifactOrigin::Cache);
    assert_eq!(resolved.package_dir, cached);

    let resolved = resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: None,
        entry_pin: pin,
    })
    .expect("随包 sidecar 应可离线解析");
    assert_eq!(resolved.origin, ArtifactOrigin::Sidecar);
    assert_eq!(resolved.package_dir, sidecar);
}

#[test]
fn resolve_rejects_wrong_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let seed = "wrong-version";
    let entry_bytes = synthetic_entry(seed);
    let package = tmp.path().join("package");
    // 负例：版本取一个与常量不同的历史值（`main`/摘要均合法，隔离版本断言）。
    write_package(&package, &entry_bytes, "0.1.0", WORKFLOW_ENTRY);

    let sources = sources_for(&package, seed);
    assert!(
        resolve_artifact(&sources).is_none(),
        "版本与期望不符必须拒绝"
    );
}

#[test]
fn resolve_rejects_escaping_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let seed = "escaping";
    let entry_bytes = synthetic_entry(seed);
    let package = tmp.path().join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(tmp.path().join("outside.js"), &entry_bytes).unwrap();
    // `main` 逃出包目录；入口内容与 pin 一致，隔离路径越界断言。
    write_package(
        &package,
        b"placeholder",
        WORKFLOW_NPM_VERSION,
        "../../outside.js",
    );

    let sources = sources_for(&package, seed);
    assert!(resolve_artifact(&sources).is_none(), "入口路径越界必须拒绝");
}

#[test]
fn resolve_rejects_tampered_entry_with_correct_metadata() {
    let tmp = tempfile::TempDir::new().unwrap();
    let package = tmp.path().join("package");
    let original = synthetic_entry("tamper-original");
    let mut tampered = synthetic_entry("tamper-original");
    tampered.extend_from_slice(b"\n// tampered payload\n");

    // 元数据、路径、版本全部合法，只有入口内容与期望摘要不符。
    write_package(&package, &tampered, WORKFLOW_NPM_VERSION, WORKFLOW_ENTRY);
    let sources = ArtifactSources {
        entry_pin: EntryPin(sha256_hex(&original)),
        ..sources_for(&package, "tamper-original")
    };
    assert!(
        resolve_artifact(&sources).is_none(),
        "入口摘要不匹配必须拒绝，不能让被篡改的 bundle 进入执行路径"
    );

    // 正对照：同一包在 pin 取自身摘要时被接受（证明拒绝来自摘要而非其他校验）。
    let accepted = ArtifactSources {
        entry_pin: EntryPin(sha256_hex(&tampered)),
        ..sources_for(&package, "tamper-original")
    };
    assert!(resolve_artifact(&accepted).is_some());
}

#[test]
fn resolve_rejects_synthetic_package_against_production_pin() {
    // 生产口径（默认 pin = 编译期常量）只接受与 pin 一致的发布产物：
    // 合成夹具的摘要必然不同，必须拒绝。
    let tmp = tempfile::TempDir::new().unwrap();
    let package = valid_package(tmp.path(), "package", "not-the-published-bundle");
    let sources = ArtifactSources {
        override_dir: Some(package),
        ..ArtifactSources::default()
    };
    assert!(resolve_artifact(&sources).is_none());
}

#[tokio::test]
async fn offline_first_use_publishes_sidecar_into_cache() {
    let tmp = tempfile::TempDir::new().unwrap();
    let seed = "publish";
    let sidecar = valid_package(tmp.path(), "sidecar", seed);
    let pin = EntryPin(sha256_hex(&synthetic_entry(seed)));
    // 缓存前缀等价于 `~/.peri/workflow/<version>`，全程不触网。
    let cache_prefix = tmp
        .path()
        .join(format!("home/.peri/workflow/{WORKFLOW_NPM_VERSION}"));

    let resolved = resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: pin.clone(),
    })
    .expect("随包 sidecar 应可离线解析");
    assert_eq!(resolved.origin, ArtifactOrigin::Sidecar);

    let published = publish_to_cache_in(&cache_prefix, &sidecar, &resolved.entry, pin.0.as_str())
        .await
        .unwrap();
    assert_eq!(
        published,
        cache_prefix.join(artifact_id()).join(WORKFLOW_ENTRY)
    );

    // 首次使用后仅凭缓存即可解析（离线复用），且不再依赖 sidecar。
    let cached = resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: Vec::new(),
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: pin.clone(),
    })
    .expect("发布后的缓存应独立可用");
    assert_eq!(cached.origin, ArtifactOrigin::Cache);
    assert_eq!(cached.entry, published);

    // 重复发布幂等：命中既有缓存，不产生额外内容目录。
    let again = publish_to_cache_in(&cache_prefix, &sidecar, &resolved.entry, pin.0.as_str())
        .await
        .unwrap();
    assert_eq!(again, published);
    let mut dirs = std::fs::read_dir(&cache_prefix)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    dirs.sort();
    assert_eq!(dirs, vec![artifact_id().to_string()]);
}

/// 离线首次使用（空缓存 + 仅随包 sidecar + 不触网）：解析 → 发布 → 真实执行。
#[tokio::test]
async fn first_use_with_empty_cache_runs_published_sidecar() {
    let tmp = tempfile::TempDir::new().unwrap();
    let seed = "first-use";
    let sidecar = valid_package(tmp.path(), "sidecar", seed);
    let cache_prefix = tmp
        .path()
        .join(format!(".peri/workflow/{WORKFLOW_NPM_VERSION}"));
    let sources = ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar],
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: EntryPin(sha256_hex(&synthetic_entry(seed))),
    };

    let command = prepare_workflow_command_with(&sources)
        .await
        .expect("首次离线使用必须由随包 sidecar 满足");
    assert_eq!(
        command.args,
        vec![cache_prefix
            .join(artifact_id())
            .join(WORKFLOW_ENTRY)
            .to_string_lossy()
            .into_owned()],
        "首次使用应先发布到本机缓存并从缓存路径执行"
    );

    let exit_code = crate::cli::run_prepared(&command, &[std::ffi::OsString::from("--help")])
        .await
        .unwrap();
    assert_eq!(
        exit_code, 0,
        "随包 sidecar 必须能在无网络、无仓库 dist 的环境下真实执行"
    );
}

#[tokio::test]
async fn prepare_fails_closed_without_artifact_and_without_opt_in() {
    let error = prepare_workflow_command_with(&ArtifactSources::default())
        .await
        .expect_err("无 sidecar、无缓存且未 opt-in 时必须 fail closed");
    let message = error.to_string();
    assert!(
        message.contains(&format!("peri-resources/workflow/{}", artifact_id())),
        "错误须指出随包 sidecar 位置: {message}"
    );
    assert!(
        message.contains("PERI_WORKFLOW_ALLOW_NPX_FALLBACK"),
        "错误须指出显式 opt-in 的联网开关: {message}"
    );
}

#[test]
fn network_fetch_requires_explicit_opt_in_value() {
    use std::ffi::OsStr;
    assert!(!network_fetch_allowed_from(None));
    assert!(!network_fetch_allowed_from(Some(OsStr::new("0"))));
    assert!(!network_fetch_allowed_from(Some(OsStr::new("true"))));
    assert!(network_fetch_allowed_from(Some(OsStr::new("1"))));
}
