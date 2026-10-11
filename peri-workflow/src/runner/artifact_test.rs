//! artifact 解析、身份/摘要校验与缓存发布的单元测试。
//!
//! 夹具一律使用仓库入库的 bundle 字节（`repository_bundle_bytes`），不复制内嵌内容；
//! 摘要 pin 与仓库 dist 的一致性由 `repository_bundle_matches_pinned_digest` 守卫。

use std::path::{Path, PathBuf};

use super::{
    artifact_id, digest_matches, network_fetch_allowed_from, prepare_workflow_command_with,
    publish_to_cache_in, resolve_artifact, ArtifactOrigin, ArtifactSources, WORKFLOW_BUILD_ID,
    WORKFLOW_ENTRY, WORKFLOW_ENTRY_SHA256, WORKFLOW_NPM_VERSION, WORKFLOW_PACKAGE_NAME,
    WORKFLOW_PROTOCOL_VERSION,
};

/// 入库 bundle 字节：夹具与真实消费共用同一内容。
fn repository_bundle_bytes() -> Vec<u8> {
    let path = super::repository_bundle_path();
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "仓库必须包含已入库的 workflow bundle {}: {error}",
            path.display()
        )
    })
}

/// 在 `parent` 下构造一个身份与摘要均合法的包目录，返回包目录路径。
fn valid_package(parent: &Path, name: &str) -> PathBuf {
    let package = parent.join(name);
    write_package(
        &package,
        &repository_bundle_bytes(),
        WORKFLOW_NPM_VERSION,
        WORKFLOW_ENTRY,
    );
    package
}

/// 按给定元数据与入口字节写入包目录。
fn write_package(package: &Path, entry_bytes: &[u8], version: &str, main: &str) {
    std::fs::create_dir_all(package.join("dist")).unwrap();
    std::fs::write(package.join("dist/peri-workflow.js"), entry_bytes).unwrap();
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

#[test]
fn repository_bundle_matches_pinned_digest() {
    assert!(
        digest_matches(&repository_bundle_bytes(), WORKFLOW_ENTRY_SHA256),
        "入库 dist 与 artifact.rs 的 pin 摘要不一致：重建 dist 后必须同步更新 WORKFLOW_ENTRY_SHA256 与 sidecar 布局检查",
    );
}

#[test]
fn resolve_reports_missing_when_no_source_is_available() {
    let sources = ArtifactSources::default();
    assert!(resolve_artifact(&sources).is_none());
}

#[test]
fn resolve_prefers_override_then_cache_then_sidecar() {
    let tmp = tempfile::TempDir::new().unwrap();
    let override_dir = valid_package(tmp.path(), "override");
    let cache_prefix = tmp.path().join("cache");
    // 缓存包目录 = `<前缀>/<内容身份>`，与 `publish_to_cache` 的发布目标一致。
    let cached = cache_prefix.join(artifact_id());
    write_package(
        &cached,
        &repository_bundle_bytes(),
        WORKFLOW_NPM_VERSION,
        WORKFLOW_ENTRY,
    );
    let sidecar = valid_package(tmp.path(), "sidecar");

    let sources = ArtifactSources {
        override_dir: Some(override_dir.clone()),
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix.clone()),
    };
    let resolved = resolve_artifact(&sources).expect("显式注入应优先命中");
    assert_eq!(resolved.origin, ArtifactOrigin::Override);
    assert_eq!(resolved.package_dir, override_dir);
    assert_eq!(resolved.entry, override_dir.join("dist/peri-workflow.js"));

    let resolved = resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix),
    })
    .expect("缓存应优先于随包 sidecar");
    assert_eq!(resolved.origin, ArtifactOrigin::Cache);
    assert_eq!(resolved.package_dir, cached);

    let resolved = resolve_artifact(&ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: None,
    })
    .expect("随包 sidecar 应可离线解析");
    assert_eq!(resolved.origin, ArtifactOrigin::Sidecar);
    assert_eq!(resolved.package_dir, sidecar);
}

#[test]
fn resolve_rejects_wrong_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let package = tmp.path().join("package");
    // 负例：版本取一个与常量不同的历史值（`main` 用真实入口，隔离版本断言）。
    write_package(
        &package,
        &repository_bundle_bytes(),
        "0.1.0",
        WORKFLOW_ENTRY,
    );

    let sources = ArtifactSources {
        override_dir: Some(package),
        ..ArtifactSources::default()
    };
    assert!(resolve_artifact(&sources).is_none());
}

#[test]
fn resolve_rejects_tampered_entry_with_correct_metadata() {
    let tmp = tempfile::TempDir::new().unwrap();
    let package = tmp.path().join("package");
    // 元数据、路径、版本全部合法，只有入口内容被替换。
    let mut tampered = repository_bundle_bytes();
    tampered.extend_from_slice(b"\n// tampered payload\n");
    write_package(&package, &tampered, WORKFLOW_NPM_VERSION, WORKFLOW_ENTRY);

    let sources = ArtifactSources {
        override_dir: Some(package),
        ..ArtifactSources::default()
    };
    assert!(
        resolve_artifact(&sources).is_none(),
        "入口摘要不匹配必须拒绝，不能让被篡改的 bundle 进入执行路径"
    );
}

#[tokio::test]
async fn offline_first_use_publishes_sidecar_into_cache() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sidecar = valid_package(tmp.path(), "sidecar");
    // 缓存前缀等价于 `~/.peri/workflow/<version>`，全程不触网。
    let cache_prefix = tmp
        .path()
        .join(format!("home/.peri/workflow/{WORKFLOW_NPM_VERSION}"));

    let resolved = resolve_artifact(&ArtifactSources {
        sidecar_dirs: vec![sidecar.clone()],
        cache_prefix: Some(cache_prefix.clone()),
        ..ArtifactSources::default()
    })
    .expect("随包 sidecar 应可离线解析");
    assert_eq!(resolved.origin, ArtifactOrigin::Sidecar);

    let published = publish_to_cache_in(&cache_prefix, &sidecar, &resolved.entry)
        .await
        .unwrap();
    assert_eq!(
        published,
        cache_prefix
            .join(artifact_id())
            .join("dist/peri-workflow.js")
    );

    // 首次使用后仅凭缓存即可解析（离线复用），且不再依赖 sidecar。
    let cached = resolve_artifact(&ArtifactSources {
        sidecar_dirs: Vec::new(),
        cache_prefix: Some(cache_prefix.clone()),
        ..ArtifactSources::default()
    })
    .expect("发布后的缓存应独立可用");
    assert_eq!(cached.origin, ArtifactOrigin::Cache);
    assert_eq!(cached.entry, published);

    // 重复发布幂等：命中既有缓存，不产生额外内容目录。
    let again = publish_to_cache_in(&cache_prefix, &sidecar, &resolved.entry)
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

/// 离线首次使用（空缓存 + 仅随包 sidecar + 不触网）：解析 → 发布 → 真实执行。
#[tokio::test]
async fn first_use_with_empty_cache_runs_published_sidecar() {
    let tmp = tempfile::TempDir::new().unwrap();
    // 随包 sidecar 的目录形状即 npm 包目录；以仓库包目录充当发行内容。
    let sidecar = super::repository_package_dir();
    let cache_prefix = tmp
        .path()
        .join(format!(".peri/workflow/{WORKFLOW_NPM_VERSION}"));
    let sources = ArtifactSources {
        override_dir: None,
        sidecar_dirs: vec![sidecar],
        cache_prefix: Some(cache_prefix.clone()),
    };

    let command = prepare_workflow_command_with(&sources)
        .await
        .expect("首次离线使用必须由随包 sidecar 满足");
    assert_eq!(
        command.args,
        vec![cache_prefix
            .join(artifact_id())
            .join("dist/peri-workflow.js")
            .to_string_lossy()
            .into_owned()],
        "首次使用应先发布到本机缓存并从缓存路径执行"
    );

    let exit_code = crate::cli::run_prepared(&command, &[std::ffi::OsString::from("--help")])
        .await
        .unwrap();
    assert_eq!(exit_code, 0, "随包 sidecar 必须能在无网络环境下真实执行");
}

#[test]
fn resolve_rejects_escaping_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let package = tmp.path().join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(tmp.path().join("outside.js"), repository_bundle_bytes()).unwrap();
    std::fs::write(
        package.join("package.json"),
        serde_json::json!({
            "name": WORKFLOW_PACKAGE_NAME,
            "version": WORKFLOW_NPM_VERSION,
            "main": "../../outside.js",
            "periProtocolVersion": WORKFLOW_PROTOCOL_VERSION,
            "periBuildId": WORKFLOW_BUILD_ID,
        })
        .to_string(),
    )
    .unwrap();

    let sources = ArtifactSources {
        override_dir: Some(package),
        ..ArtifactSources::default()
    };
    assert!(resolve_artifact(&sources).is_none());
}
