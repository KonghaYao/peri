//! test-child 二进制路径解析等测试辅助函数

/// 返回 test-child 二进制的路径。
/// 利用 cargo 的 CARGO_BIN_EXE_test-child 环境变量。
pub fn test_child_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_test-child"))
}
