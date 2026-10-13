//! `route_fingerprint` 契约测试（契约 §3.2：规范化 endpoint + 长度分帧 SHA-256）与
//! 「禁用 spool 不触碰磁盘」的边界观测（契约 §6）。
//!
//! 只覆盖 P1b 的同步路由派生：正规化的等价类、凭据轮换隔离、以及拒绝 userinfo/query/
//! fragment/非 http(s) 端点。端口本身的行为在 `spool/types_test.rs` 与
//! `spool/cleanup_test.rs` 中测试；native 适配器行为在 `spool/native_test.rs` 中测试。

use super::*;

const ENDPOINT: &str = "https://cloud.langfuse.com";
const PUBLIC_KEY: &str = "pk-lf-11111111-2222-3333-4444-555555555555";
const SECRET_KEY: &str = "sk-lf-99999999-8888-7777-6666-555555555555";

fn fingerprint(endpoint: &str) -> RouteFingerprint {
    route_fingerprint(endpoint, PUBLIC_KEY, SECRET_KEY).expect("endpoint should be accepted")
}

#[test]
fn equivalent_endpoints_share_one_route() {
    let canonical = fingerprint(ENDPOINT);
    for equivalent in [
        "https://cloud.langfuse.com/",
        "https://cloud.langfuse.com///",
        "HTTPS://CLOUD.LANGFUSE.COM",
        "https://cloud.langfuse.com:443",
        "https://cloud.langfuse.com:443/",
    ] {
        assert_eq!(canonical, fingerprint(equivalent), "endpoint: {equivalent}");
    }
}

#[test]
fn path_prefix_is_preserved_and_normalized() {
    let with_path = fingerprint("https://cloud.langfuse.com/v1/otlp");
    assert_eq!(
        with_path,
        fingerprint("https://cloud.langfuse.com/v1/otlp/")
    );
    assert_ne!(with_path, fingerprint("https://cloud.langfuse.com/v1"));
    assert_ne!(with_path, fingerprint(ENDPOINT));
}

#[test]
fn host_and_explicit_port_stay_distinct() {
    assert_ne!(
        fingerprint("http://localhost:3000"),
        fingerprint("http://localhost:3001")
    );
    assert_ne!(
        fingerprint("http://localhost:3000"),
        fingerprint("http://127.0.0.1:3000")
    );
    assert_eq!(
        fingerprint("http://localhost:80"),
        fingerprint("http://localhost"),
        "default port normalization"
    );
}

#[test]
fn credential_rotation_isolates_old_objects() {
    let base = route_fingerprint(ENDPOINT, PUBLIC_KEY, SECRET_KEY).unwrap();
    let rotated_public = route_fingerprint(ENDPOINT, "pk-lf-rotated", SECRET_KEY).unwrap();
    let rotated_secret = route_fingerprint(ENDPOINT, PUBLIC_KEY, "sk-lf-rotated").unwrap();
    assert_ne!(base, rotated_public);
    assert_ne!(base, rotated_secret);
    assert_ne!(rotated_public, rotated_secret);
}

#[test]
fn framing_separates_field_boundaries() {
    // 无长度分帧时 "ab"+"c" 与 "a"+"bc" 会碰撞；分帧必须让它们不同。
    let left = route_fingerprint(ENDPOINT, "ab", "c").unwrap();
    let right = route_fingerprint(ENDPOINT, "a", "bc").unwrap();
    assert_ne!(left, right);
}

#[test]
fn rejected_endpoints_never_echo_credentials() {
    let secret = "sk-lf-should-not-leak";
    for rejected in [
        "https://user:pass@cloud.langfuse.com",
        "https://pk:sk@cloud.langfuse.com",
        "https://cloud.langfuse.com?token=abc",
        "https://cloud.langfuse.com#frag",
        "file:///tmp/spool",
        "not a url",
        "ftp://cloud.langfuse.com",
        "",
    ] {
        let error = route_fingerprint(rejected, PUBLIC_KEY, secret).unwrap_err();
        assert!(
            matches!(error, LangfuseError::Config(_)),
            "unexpected error for {rejected}: {error}"
        );
        let rendered = error.to_string();
        assert!(
            !rendered.contains(secret),
            "error leaked secret: {rendered}"
        );
        assert!(!rendered.contains(rejected) || rejected.is_empty());
    }
}

/// 契约 §6：`spool=false` 不构造 native，因此不扫描/恢复/清理旧 spool、不创建任何新磁盘文件。
///
/// 观测用真实临时根 + 真实 native：启用路径先落下布局并留下一次崩溃 writer 的
/// `tmp/<id>.part` 残留，禁用开关必须一个字节都不动；随后同一根上的启用路径做对照，
/// 证明该观测确实能发现扫描/恢复造成的目录变化（不是恒真断言）。
///
/// 开关的**生产**构造门控属 P3c 的 session 接线（本阶段不接线），此处按冻结规则复核
/// 「禁用即不进入 native 构造」这一配置面语义。
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
#[test]
fn disabled_spool_never_opens_native_and_leaves_the_spool_directory_untouched() {
    use crate::config::TelemetryConfig;
    use crate::spool::native::{hex, NativeSpool};

    const RESIDUE: &str = "tmp/00112233445566778899aabbccddeeff.part";
    let temp = tempfile::tempdir().expect("临时根");
    let route = RouteFingerprint::from_bytes([0x5a; 32]);
    let quota = 8 * 1024 * 1024;

    // 启用路径先落下真实布局，再留下崩溃 writer 的临时残留（启动恢复必须回收它）。
    let enabled =
        NativeSpool::open_at(temp.path().to_path_buf(), route, quota).expect("打开 spool");
    let route_dir = temp
        .path()
        .join("peri/telemetry-spool/v1")
        .join(hex(route.as_bytes()));
    let residue = route_dir.join(RESIDUE);
    std::fs::write(&residue, b"crashed writer body").expect("写入残留");
    drop(enabled);

    let before = spool_tree(temp.path());
    assert!(
        before.iter().any(|(path, _, _)| path.ends_with(RESIDUE)),
        "前置条件：残留必须已被真实 native 落盘"
    );

    // 禁用分支：唯一 native 构造入口不得被调用。
    let config = TelemetryConfig {
        spool_enabled: false,
        ..Default::default()
    };
    let disabled: Option<NativeSpool> = if config.spool_enabled {
        Some(NativeSpool::open_at(temp.path().to_path_buf(), route, quota).expect("打开 spool"))
    } else {
        None
    };
    assert!(disabled.is_none(), "禁用 spool 不得构造 native");
    assert_eq!(
        spool_tree(temp.path()),
        before,
        "禁用 spool 不得扫描/恢复/清理或新建任何文件"
    );
    assert!(residue.exists(), "禁用 spool 不得清扫旧 writer 残留");

    // 对照：同一根上的启用路径会回收残留，使上面的观测具备检测力。
    drop(NativeSpool::open_at(temp.path().to_path_buf(), route, quota).expect("打开 spool"));
    assert!(!residue.exists(), "启用路径的启动恢复必须回收残留");
    assert_ne!(spool_tree(temp.path()), before);
}

/// 目录观测：相对路径集合 + 文件长度与字节内容；不比较 mtime，故只声称「集合与内容」不变。
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
fn spool_tree(root: &std::path::Path) -> Vec<(std::path::PathBuf, u64, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("读取 spool 目录") {
            let entry = entry.expect("目录项");
            let path = entry.path();
            let metadata = entry.metadata().expect("目录项元数据");
            if metadata.is_dir() {
                pending.push(path);
            } else {
                entries.push((
                    path.strip_prefix(root).expect("相对路径").to_path_buf(),
                    metadata.len(),
                    std::fs::read(&path).expect("读取 spool 文件"),
                ));
            }
        }
    }
    entries.sort();
    entries
}
