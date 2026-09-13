//! 请求路径的词法归一（宿主路径单表示）。
//!
//! 本模块是**唯一**允许把手写路径字符串变成「可交给内核的组件序列」的地方，两条不变量：
//!
//! 1. 交给内核的路径一律以**组件向量**表示，且不含 `.`/`..`/空段——`..` 在词法阶段
//!    消解，消解越出授权根即拒绝（绝不把 `..` 交给内核的 `openat`）。
//! 2. 授权判定按**路径组件**逐段比较，禁止字符串前缀比较：`/srv/ws-evil` 不是
//!    `/srv/ws` 下的路径（对应 FC-SBX-01 的前缀碰撞要求）。
//!
//! 从 D-003 起只有**一种**路径观：调用方给出的是宿主路径（相对或绝对），工具与落盘
//! 产物回给调用方的也是宿主路径：D-003 起只有**一种**路径表示，不存在任何表示换算。

use std::path::{Path, PathBuf};

use crate::error::CapabilityError;

use super::{invalid_input, outside_root};

/// 词法归一后的请求路径（不含 `.`/`..`/空段，不区分绝对/相对——两者都以授权根为基准）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedPath {
    raw: String,
    components: Vec<String>,
}

impl RequestedPath {
    /// 词法归一。`root` 只用于构造错误载荷的 `root` 字段（该字段从不进入公开文本）。
    ///
    /// 规则（对齐源实现的 `logical_path_key`，见 `peri-middlewares/.../transaction.rs:129`）：
    /// - 以 `/` 分段，丢弃空段与 `.`；
    /// - `..` 弹出上一段；栈空时弹出即越界，拒绝（源实现在此处的 `pop()` 静默丢弃，
    ///   但那会让 `../../etc/passwd` 变成 `etc/passwd`；本实现在这里必须显式拒绝）。
    /// - 反斜杠是普通文件名字符（源 `logical_path_key` 与 `resolve_path` 均不归一 `\`），
    ///   只有 Glob 的*匹配字符串*会把 `\` 显示为 `/`。
    pub fn parse(raw: &str, root: &Path) -> Result<Self, CapabilityError> {
        if raw.is_empty() {
            return Err(invalid_input("Path must not be empty."));
        }
        if raw.contains('\0') {
            return Err(invalid_input("Path must not contain a NUL byte."));
        }
        if raw.trim().is_empty() {
            return Err(invalid_input("Path must not be blank."));
        }

        let mut components: Vec<String> = Vec::new();
        for segment in raw.split('/') {
            match segment {
                "" | "." => continue,
                ".." => {
                    if components.pop().is_none() {
                        return Err(outside_root(raw, root));
                    }
                }
                other => components.push(other.to_string()),
            }
        }
        Ok(Self {
            raw: raw.to_string(),
            components,
        })
    }

    /// 调用方原始字符串（错误文案逐字回显用）。
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// 由**已验证的**相对组件直接构造（遍历起点、内部换算用）。
    ///
    /// 调用方保证组件不含 `.`/`..`（它们来自 [`RequestedPath::parse`] 或授权根内
    /// 逐组件解析的产物），因此这里不做任何校验。
    pub(crate) fn from_components(components: &[String], _root: &Path) -> Self {
        Self {
            raw: components.join("/"),
            components: components.to_vec(),
        }
    }

    /// 归一后的组件。
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// 是否指向授权根本身（如 `/srv/ws`、`/srv/ws/`、`.`）。
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// 相对根的显示形式（`a/b`；根本身为空串），对齐源实现的 `strip_prefix(cwd)` 语义。
    pub fn relative(&self) -> String {
        self.components.join("/")
    }
}

/// 把相对组件拼成 `PathBuf`（仅用于展示与锁键，不直接交给内核）。
pub fn join_components(components: &[String]) -> PathBuf {
    let mut path = PathBuf::new();
    for component in components {
        path.push(component);
    }
    path
}

/// 组件向量是否以 `prefix` 为前缀（逐组件比较）。
pub fn starts_with_components(components: &[String], prefix: &[String]) -> bool {
    components.len() >= prefix.len() && components[..prefix.len()] == *prefix
}

/// `Path` → 组件向量（丢弃根与 `.`，`..` 弹出）。
///
/// 用于授权根与请求路径的**逐组件**比较（[`starts_with_components`]）：这是前缀碰撞
/// （`/srv/ws-evil` 不是 `/srv/ws` 下的路径）判定的唯一依据，不做任何 FS 访问。
pub fn components_of(path: &Path) -> Vec<String> {
    let mut components: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                components.pop();
            }
            other => components.push(other.as_os_str().to_string_lossy().to_string()),
        }
    }
    components
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/srv/ws")
    }

    #[test]
    fn test_parse_normalizes_dot_and_parent() {
        let parsed = RequestedPath::parse("./src/../lib/./a.rs", &root()).unwrap();
        assert_eq!(
            parsed.components(),
            &["lib".to_string(), "a.rs".to_string()]
        );
        assert_eq!(parsed.relative(), "lib/a.rs");
    }

    #[test]
    fn test_parse_rejects_parent_above_root() {
        let err = RequestedPath::parse("../../etc/passwd", &root()).unwrap_err();
        assert!(matches!(err, CapabilityError::OutsideRoot { .. }));
        assert!(err.public_message().contains("../../etc/passwd"));
        assert!(!err.public_message().contains("/srv/ws"));
    }

    #[test]
    fn test_parse_rejects_inner_escape() {
        let err = RequestedPath::parse("src/../../etc/passwd", &root()).unwrap_err();
        assert!(matches!(err, CapabilityError::OutsideRoot { .. }));
    }

    #[test]
    fn test_parse_rejects_nul_and_blank() {
        assert!(matches!(
            RequestedPath::parse("a\0b", &root()),
            Err(CapabilityError::InvalidInput { .. })
        ));
        assert!(matches!(
            RequestedPath::parse("   ", &root()),
            Err(CapabilityError::InvalidInput { .. })
        ));
    }

    #[test]
    fn test_components_of_normalizes_and_pops_parent() {
        assert_eq!(
            components_of(Path::new("/srv/ws/a/b")),
            ["srv", "ws", "a", "b"]
        );
        assert_eq!(
            components_of(Path::new("/srv/ws/../ws-evil/x")),
            ["srv", "ws-evil", "x"]
        );
        assert_eq!(components_of(Path::new("./a")), ["a"]);
    }

    #[test]
    fn test_starts_with_components_is_component_wise_not_string_prefix() {
        let root = components_of(Path::new("/srv/ws"));
        // 前缀碰撞：字符串前缀真、组件前缀假。
        assert!(!starts_with_components(
            &components_of(Path::new("/srv/ws-evil/x")),
            &root
        ));
        assert!(starts_with_components(
            &components_of(Path::new("/srv/ws/a")),
            &root
        ));
        // 根本身是它自己的前缀（逐组件相等）。
        assert!(starts_with_components(&root, &root));
    }
}
