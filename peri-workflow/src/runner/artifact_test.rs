//! artifact 解析、身份/摘要校验与缓存发布的单元测试。
//!
//! 夹具**完全自含**：临时目录中的合成包、测试内自算 sha256，经 [`EntryPin`] 注入期望
//! 摘要。任何用例都不读取仓库内文件（dist 不入库，也不读工作树 dist）。

use std::path::{Path, PathBuf};

use crate::runner::artifact::ArtifactOrigin;
use crate::runner::artifact::ArtifactSources;
use crate::runner::artifact::EntryPin;
use crate::runner::artifact::WORKFLOW_BUILD_ID;
use crate::runner::artifact::WORKFLOW_ENTRY;
use crate::runner::artifact::WORKFLOW_ENTRY_SHA256;
use crate::runner::artifact::WORKFLOW_NPM_VERSION;
use crate::runner::artifact::WORKFLOW_PACKAGE_NAME;
use crate::runner::artifact::WORKFLOW_PROTOCOL_VERSION;

/// 合成入口字节：可被 `node` 直接执行的最小脚本（内容任意，摘要由测试内计算）。
fn synthetic_entry(seed: &str) -> Vec<u8> {
    format!("// synthetic engine fixture ({seed})\nprocess.exit(0)\n").into_bytes()
}

/// 按给定元数据与入口字节写入包目录（身份字段取入参）。
fn write_package_with(
    package: &Path,
    entry_bytes: &[u8],
    version: &str,
    build_id: &str,
    main: &str,
) {
    std::fs::create_dir_all(package.join("dist"))
        .expect("the test fixture directory must be creatable");
    std::fs::write(package.join(WORKFLOW_ENTRY), entry_bytes)
        .expect("the test fixture file must be writable");
    std::fs::write(
        package.join("package.json"),
        serde_json::json!({
            "name": WORKFLOW_PACKAGE_NAME,
            "version": version,
            "main": main,
            "periProtocolVersion": WORKFLOW_PROTOCOL_VERSION,
            "periBuildId": build_id,
        })
        .to_string(),
    )
    .expect("the test fixture file must be writable");
}

/// 按随包身份写入包目录（版本/构建标识取编译期常量，仅 `main` 可调）。
fn write_package(package: &Path, entry_bytes: &[u8], version: &str, main: &str) {
    write_package_with(package, entry_bytes, version, WORKFLOW_BUILD_ID, main);
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
        entry_pin: EntryPin(crate::runner::artifact::sha256_hex(&synthetic_entry(seed))),
        ..ArtifactSources::default()
    }
}

/// 摘要前 12 位（内容身份），与生产代码的目录命名约定一致。
fn identity(sha256: &str) -> &str {
    &sha256[..12]
}

#[test]
fn entry_pin_defaults_to_the_compiled_constant() {
    assert_eq!(EntryPin::default().0, WORKFLOW_ENTRY_SHA256);
}

#[test]
fn bundled_build_id_matches_the_compiled_npm_version() {
    assert_eq!(
        WORKFLOW_BUILD_ID,
        crate::runner::artifact::build_id_for(WORKFLOW_NPM_VERSION)
    );
}

#[test]
fn resolve_reports_missing_when_no_source_is_available() {
    let sources = ArtifactSources::default();
    assert!(crate::runner::artifact::resolve_artifact(&sources).is_none());
}

#[test]
fn resolve_prefers_override_then_cache_then_sidecar() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let seed = "precedence";
    let entry_bytes = synthetic_entry(seed);
    let pin = EntryPin(crate::runner::artifact::sha256_hex(&entry_bytes));

    let override_dir = valid_package(tmp.path(), "override", seed);
    // 缓存包目录 = `<前缀>/<内容身份>`，与 `publish_to_cache_in` 的发布目标一致。
    let cache_prefix = tmp.path().join("cache");
    let cached = cache_prefix.join(identity(&pin.0));
    write_package(&cached, &entry_bytes, WORKFLOW_NPM_VERSION, WORKFLOW_ENTRY);
    let sidecar = valid_package(tmp.path(), "sidecar", seed);

    let resolved = crate::runner::artifact::resolve_artifact(&ArtifactSources {
        override_dir: Some(override_dir.clone()),
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: pin.clone(),
    })
    .expect("显式注入应优先命中");
    assert_eq!(resolved.origin, ArtifactOrigin::Override);
    assert_eq!(resolved.package_dir, override_dir);
    assert_eq!(resolved.entry, override_dir.join(WORKFLOW_ENTRY));

    let resolved = crate::runner::artifact::resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix),
        entry_pin: pin.clone(),
    })
    .expect("缓存应优先于随包 sidecar");
    assert_eq!(resolved.origin, ArtifactOrigin::Cache);
    assert_eq!(resolved.package_dir, cached);

    let resolved = crate::runner::artifact::resolve_artifact(&ArtifactSources {
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
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let seed = "wrong-version";
    let entry_bytes = synthetic_entry(seed);
    let package = tmp.path().join("package");
    // 负例：版本取一个与常量不同的历史值（`main`/摘要均合法，隔离版本断言）。
    write_package(&package, &entry_bytes, "0.1.0", WORKFLOW_ENTRY);

    let sources = sources_for(&package, seed);
    assert!(
        crate::runner::artifact::resolve_artifact(&sources).is_none(),
        "版本与期望不符必须拒绝"
    );
}

#[test]
fn resolve_rejects_escaping_entry() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let seed = "escaping";
    let entry_bytes = synthetic_entry(seed);
    let package = tmp.path().join("package");
    std::fs::create_dir_all(&package)
        .expect("the create dir all fixture result is required for this assertion");
    std::fs::write(tmp.path().join("outside.js"), &entry_bytes)
        .expect("the test fixture file must be writable");
    // `main` 逃出包目录；入口内容与 pin 一致，隔离路径越界断言。
    write_package(
        &package,
        b"placeholder",
        WORKFLOW_NPM_VERSION,
        "../../outside.js",
    );

    let sources = sources_for(&package, seed);
    assert!(
        crate::runner::artifact::resolve_artifact(&sources).is_none(),
        "入口路径越界必须拒绝"
    );
}

#[test]
fn resolve_rejects_tampered_entry_with_correct_metadata() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let package = tmp.path().join("package");
    let original = synthetic_entry("tamper-original");
    let mut tampered = synthetic_entry("tamper-original");
    tampered.extend_from_slice(b"\n// tampered payload\n");

    // 元数据、路径、版本全部合法，只有入口内容与期望摘要不符。
    write_package(&package, &tampered, WORKFLOW_NPM_VERSION, WORKFLOW_ENTRY);
    let sources = ArtifactSources {
        entry_pin: EntryPin(crate::runner::artifact::sha256_hex(&original)),
        ..sources_for(&package, "tamper-original")
    };
    assert!(
        crate::runner::artifact::resolve_artifact(&sources).is_none(),
        "入口摘要不匹配必须拒绝，不能让被篡改的 bundle 进入执行路径"
    );

    // 正对照：同一包在 pin 取自身摘要时被接受（证明拒绝来自摘要而非其他校验）。
    let accepted = ArtifactSources {
        entry_pin: EntryPin(crate::runner::artifact::sha256_hex(&tampered)),
        ..sources_for(&package, "tamper-original")
    };
    assert!(crate::runner::artifact::resolve_artifact(&accepted).is_some());
}

#[test]
fn resolve_rejects_synthetic_package_against_production_pin() {
    // 生产口径（默认 pin = 编译期常量）只接受与 pin 一致的发布产物：
    // 合成夹具的摘要必然不同，必须拒绝。
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let package = valid_package(tmp.path(), "package", "not-the-published-bundle");
    let sources = ArtifactSources {
        override_dir: Some(package),
        ..ArtifactSources::default()
    };
    assert!(crate::runner::artifact::resolve_artifact(&sources).is_none());
}

#[tokio::test]
async fn offline_first_use_publishes_sidecar_into_cache() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let seed = "publish";
    let sidecar = valid_package(tmp.path(), "sidecar", seed);
    let pin = EntryPin(crate::runner::artifact::sha256_hex(&synthetic_entry(seed)));
    // 缓存前缀等价于 `~/.peri/workflow/<version>`，全程不触网。
    let cache_prefix = tmp
        .path()
        .join(format!("home/.peri/workflow/{WORKFLOW_NPM_VERSION}"));

    let resolved = crate::runner::artifact::resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: pin.clone(),
    })
    .expect("随包 sidecar 应可离线解析");
    assert_eq!(resolved.origin, ArtifactOrigin::Sidecar);

    let published = crate::runner::artifact::publish_to_cache_in(
        &cache_prefix,
        &sidecar,
        &resolved.entry,
        crate::runner::artifact::PackageIdentity::bundled(pin.0.as_str()),
    )
    .await
    .expect("the publish to cache in fixture result is required for this assertion");
    assert_eq!(
        published,
        cache_prefix.join(identity(&pin.0)).join(WORKFLOW_ENTRY)
    );

    // 首次使用后仅凭缓存即可解析（离线复用），且不再依赖 sidecar。
    let cached = crate::runner::artifact::resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: Vec::new(),
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: pin.clone(),
    })
    .expect("发布后的缓存应独立可用");
    assert_eq!(cached.origin, ArtifactOrigin::Cache);
    assert_eq!(cached.entry, published);

    // 重复发布幂等：命中既有缓存，不产生额外内容目录。
    let again = crate::runner::artifact::publish_to_cache_in(
        &cache_prefix,
        &sidecar,
        &resolved.entry,
        crate::runner::artifact::PackageIdentity::bundled(pin.0.as_str()),
    )
    .await
    .expect("the publish to cache in fixture result is required for this assertion");
    assert_eq!(again, published);
    let mut dirs = std::fs::read_dir(&cache_prefix)
        .expect("the read dir fixture result is required for this assertion")
        .map(|entry| {
            entry
                .expect("the entry fixture result is required for this assertion")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    dirs.sort();
    assert_eq!(dirs, vec![identity(&pin.0).to_string()]);
}

/// 离线首次使用（空缓存 + 仅随包 sidecar + 不触网）：解析 → 发布 → 真实执行。
#[tokio::test]
async fn first_use_with_empty_cache_runs_published_sidecar() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let seed = "first-use";
    let sidecar = valid_package(tmp.path(), "sidecar", seed);
    let cache_prefix = tmp
        .path()
        .join(format!(".peri/workflow/{WORKFLOW_NPM_VERSION}"));
    let pin = crate::runner::artifact::sha256_hex(&synthetic_entry(seed));
    let sources = ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar],
        cache_prefix: Some(cache_prefix.clone()),
        entry_pin: EntryPin(pin.clone()),
    };

    let command = crate::runner::artifact::prepare_workflow_command_with(&sources)
        .await
        .expect("首次离线使用必须由随包 sidecar 满足");
    assert_eq!(
        command.args,
        vec![cache_prefix
            .join(identity(&pin))
            .join(WORKFLOW_ENTRY)
            .to_string_lossy()
            .into_owned()],
        "首次使用应先发布到本机缓存并从缓存路径执行"
    );

    let exit_code = crate::cli::run_prepared(&command, &[std::ffi::OsString::from("--help")])
        .await
        .expect("the run prepared fixture result is required for this assertion");
    assert_eq!(
        exit_code, 0,
        "随包 sidecar 必须能在无网络、无仓库 dist 的环境下真实执行"
    );
}

#[test]
fn resolve_ignores_cache_entries_outside_the_bundled_content_identity() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let orphan_bytes = synthetic_entry("orphan-cache");
    let orphan_sha = crate::runner::artifact::sha256_hex(&orphan_bytes);
    let cache_prefix = tmp.path().join("cache");
    // 模拟旧 registry/其它来源留下的合法包：内容身份与随包 pin 不同，不得被复用。
    write_package(
        &cache_prefix.join(identity(&orphan_sha)),
        &orphan_bytes,
        WORKFLOW_NPM_VERSION,
        WORKFLOW_ENTRY,
    );
    let sources = ArtifactSources {
        override_dir: None,
        sidecar_dirs: Vec::new(),
        cache_prefix: Some(cache_prefix),
        entry_pin: EntryPin(crate::runner::artifact::sha256_hex(&synthetic_entry(
            "bundled",
        ))),
    };
    assert!(
        crate::runner::artifact::resolve_artifact(&sources).is_none(),
        "只允许随包内容身份的缓存命中"
    );
}

#[tokio::test]
async fn prepare_fails_closed_without_artifact() {
    let error = crate::runner::artifact::prepare_workflow_command_with(&ArtifactSources::default())
        .await
        .expect_err("无 sidecar、无缓存时必须 fail closed");
    let message = error.to_string();
    assert!(
        message.contains(&format!(
            "peri-resources/workflow/{}",
            crate::runner::artifact::artifact_id()
        )),
        "错误须指出随包 sidecar 位置: {message}"
    );
    assert!(
        message.contains("PERI_WORKFLOW_ARTIFACT_DIR"),
        "错误须指出显式注入入口: {message}"
    );
    assert!(
        !message.contains("PERI_WORKFLOW_ALLOW_NPX_FALLBACK"),
        "错误不得再提示已移除的联网开关: {message}"
    );
}
