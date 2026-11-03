//! Shared fixtures for `settings::tests`.
//!
//! Compiled only through the parent module's `#[cfg(test)]`; none of this enters
//! production builds. Keeping the fixtures here leaves the test file within the
//! repository size budget without changing any test's module path.

use std::io::Write;
use std::path::PathBuf;

use super::super::ConfigSource;

pub(super) fn revision(source: &ConfigSource) -> crate::ConfigurationRevision {
    source
        .snapshot()
        .map(|snapshot| snapshot.revision())
        .unwrap_or_else(|| {
            crate::ConfigurationSnapshot::resolve(
                crate::ConfigurationScope::new(
                    std::env::temp_dir(),
                    source.global_path().to_owned(),
                )
                .unwrap(),
                Default::default(),
            )
            .unwrap()
            .revision()
        })
}

/// 在临时目录创建 .peri/settings.json
pub(super) fn write_settings(dir: &std::path::Path, content: &str) {
    let peri_dir = dir.join(".peri");
    std::fs::create_dir_all(&peri_dir).unwrap();
    let mut f = std::fs::File::create(peri_dir.join("settings.json")).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

/// RAII guard：记录进入时的全局配置路径，Drop 时恢复原值，
/// 防止断言失败后残留全局态污染其他测试。
///
/// 不能无条件复位为 `None`：若进入前已被重定向，清空会把后续用例带回默认用户路径。
/// 配置数据面只暴露解析后的路径，因此这里保存并恢复解析后的值；未重定向时该值
/// 即默认路径，行为等价。
pub(super) struct ConfigPathGuard {
    previous: PathBuf,
}

impl ConfigPathGuard {
    pub(super) fn new() -> Self {
        Self {
            previous: super::super::config_path(),
        }
    }
}

impl Drop for ConfigPathGuard {
    fn drop(&mut self) {
        super::super::set_global_config_path(Some(self.previous.clone()));
    }
}

/// 子进程用例的唤醒标记：父测试用它确认子进程确实由本测试拉起。
pub(super) const SUBPROCESS_MARKER: &str = "PERI_CONFIG_TEST_SUBPROCESS";

/// 以隔离的 cwd/HOME 运行当前测试二进制中的指定用例。
///
/// 子进程的所有相对路径解析都落在 `workspace`，HOME 落在 `home`；
/// 全局路径重定向是进程内存状态，不会从父进程继承。
pub(super) fn run_isolated_test(
    test_name: &str,
    workspace: &std::path::Path,
    home: &std::path::Path,
    ignored: bool,
) -> std::process::Output {
    let exe = std::env::current_exe().expect("定位当前测试二进制");
    let mut command = std::process::Command::new(exe);
    command
        .arg("--exact")
        .arg(test_name)
        .arg("--test-threads=1")
        .arg("--nocapture")
        .current_dir(workspace)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(SUBPROCESS_MARKER, "1");
    if ignored {
        command.arg("--ignored");
    }
    let output = command.output().expect("启动隔离子进程");
    if output.status.success() {
        // 过滤名写错时 libtest 会以 0 退出但不运行任何用例，必须显式防假绿。
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "隔离子进程未运行目标用例 {test_name}：\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

/// 构造配置源：global 目录 + 可选 workspace 目录（均含 .peri/settings.json）
pub(super) fn make_source(tmp: &tempfile::TempDir, ws_dir: Option<&str>) -> ConfigSource {
    let global_dir = tmp.path().join("global");
    let global_path = global_dir.join(".peri").join("settings.json");
    let cwd = ws_dir
        .map(|d| tmp.path().join(d))
        .unwrap_or_else(|| tmp.path().join("empty-cwd"));
    if ws_dir.is_none() {
        std::fs::create_dir_all(&cwd).unwrap();
    }
    ConfigSource::load_at(&cwd, global_path).unwrap()
}
