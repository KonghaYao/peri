//! capability：授权根内的路径授权与边界判定（WP-002）。
//!
//! ## 职责
//!
//! 本模块把「调用方给的路径字符串」变成「相对已验证目录 fd 的操作」，是七个工具在**本
//! 进程内**的**唯一**文件系统入口。它不执行工具语义（那是 [`crate::tools::fs`] 的事），
//! 也不接触 MCP 传输。
//!
//! ## 安全模型（对应 R-003 / FC-SBX-01 / A-008）
//!
//! | 威胁 | 处理 |
//! | --- | --- |
//! | `..` 穿越 | 词法阶段消解；越出根即拒绝，绝不把 `..` 交给内核 |
//! | 绝对路径越界 | 拒绝（`OutsideRoot`），只回显调用方原始路径 |
//! | 前缀碰撞（`/srv/ws-evil`） | 逐组件比较，禁止字符串前缀 |
//! | 末段/中间段符号链接逃逸 | `openat(O_NOFOLLOW)` 逐段打开；根内链接解析，绝对或越出根的目标拒绝 |
//! | 交换/删除重建竞态（TOCTOU） | 所有操作基于已持有的目录 fd，不用「解析后的路径字符串」再打开 |
//! | 遍历逃逸（Glob/deep_scan） | 逐目录 `openat` 下钻，不跟随符号链接 |
//!
//! **路径表示只有一种**（D-003）：调用方给的与工具回的都是宿主路径，本层直接把请求解析
//! 为宿主绝对路径（[`RootDir::resolve_host_path`]），不存在任何第二种路径表示。
//!
//! **这里不提供隔离**：工作区根是**能力边界，不是安全边界**——进程以当前用户权限在本机
//! 运行，`Bash` 在该根下执行且不额外限制命令（见 README 的诚实声明）。本模块只保证
//! *文件类工具* 的路径解析不越出根，不声称任何越出根之后的遏制能力。

pub mod path;
pub mod root;

use std::io;
use std::path::Path;

use crate::error::CapabilityError;

pub use path::{join_components, starts_with_components, RequestedPath};
pub use root::{
    DirHandle, DirectChild, EntryMeta, OpenedEntry, Parents, RawDirEntry, RootDir, WalkControl,
    WalkEntry,
};

/// capability 层失败分类。
///
/// 工具层需要区分「不存在」（源实现有专门文案，如 `Error: File not found at {path}`）与
/// 「越界/被拒」以及「其它 IO 失败」，但 [`CapabilityError`] 是冻结的共享契约（不能扩展）。
/// 因此本层内部用本枚举，工具层再把它投影成各自的文案。
#[derive(Debug)]
pub enum AccessError {
    /// 目标不存在。
    NotFound {
        /// 调用方请求路径（已归一，可回显）。
        requested: String,
    },
    /// 结构性拒绝：越界、符号链接逃逸、非法输入。
    Rejected(CapabilityError),
    /// 其它 IO 失败（`message` 已脱敏，只含调用方已知路径）。
    Io {
        /// 动作名（`open`/`create directory`/…）。
        action: &'static str,
        /// 调用方请求路径。
        requested: String,
        /// 底层错误。
        source: io::Error,
    },
}

impl AccessError {
    /// 构造「不存在」。
    pub fn not_found(requested: impl Into<String>) -> Self {
        Self::NotFound {
            requested: requested.into(),
        }
    }

    /// 构造结构性拒绝。
    pub fn rejected(error: CapabilityError) -> Self {
        Self::Rejected(error)
    }

    /// 构造 IO 失败。
    pub fn io(action: &'static str, requested: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            action,
            requested: requested.into(),
            source,
        }
    }

    /// 是否「不存在」。
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    /// 底层 `errno`（仅 IO 失败有）。
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            Self::Io { source, .. } => source.raw_os_error(),
            _ => None,
        }
    }

    /// 是否结构性拒绝（越界/逃逸/非法输入）。
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }

    /// 可外发文本（不含宿主真实路径与 secret）。
    ///
    /// `Io` 只回显底层错误文本——源实现的工具在 IO 失败时正是把它原样抛给调用方
    /// （例如 `folder_operations` 的 `create_dir_all(resolved)?`）。
    pub fn message(&self) -> String {
        match self {
            Self::NotFound { requested } => format!("No such file or directory: {requested}"),
            Self::Rejected(error) => error.public_message(),
            Self::Io { source, .. } => source.to_string(),
        }
    }

    /// 源实现形态的 IO 文案：工具把底层错误原样外发时使用
    /// （例如 `folder_operations` 的 `create_dir_all(resolved)?`）。
    pub fn io_text(&self) -> String {
        match self {
            Self::NotFound { .. } => io::Error::from_raw_os_error(libc::ENOENT).to_string(),
            Self::Io { source, .. } => source.to_string(),
            Self::Rejected(error) => error.public_message(),
        }
    }

    /// 投影回共享错误契约（供需要统一类型的调用方使用）。
    pub fn into_capability(self) -> CapabilityError {
        match self {
            Self::Rejected(error) => error,
            Self::NotFound { requested } => CapabilityError::Io {
                message: format!("No such file or directory: {requested}"),
            },
            Self::Io {
                action,
                requested,
                source,
            } => CapabilityError::Io {
                message: format!("{action} failed for {requested}: {source}"),
            },
        }
    }
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for AccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 非法参数（类型/范围/空值）。
pub(crate) fn invalid_input(message: impl Into<String>) -> CapabilityError {
    CapabilityError::InvalidInput {
        message: message.into(),
    }
}

/// 请求路径落在授权根之外。
///
/// `root` 字段按共享契约承载授权根，但**从不**进入任何外发文本
/// （[`CapabilityError::public_message`] 只用 `requested`）：宿主路径不因误序列化而外泄。
pub(crate) fn outside_root(requested: &str, root: &Path) -> CapabilityError {
    CapabilityError::OutsideRoot {
        requested: requested.to_string(),
        root: root.to_path_buf(),
    }
}

/// 符号链接目标逃出授权根。
pub(crate) fn symlink_escape(requested: &str, resolved: &Path) -> CapabilityError {
    CapabilityError::SymlinkEscape {
        requested: requested.to_string(),
        resolved: resolved.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_access_error_messages_are_sanitized() {
        let error = AccessError::rejected(CapabilityError::OutsideRoot {
            requested: "../secret".to_string(),
            root: PathBuf::from("/private/host/ws-root"),
        });
        let message = error.message();
        assert!(message.contains("../secret"));
        assert!(!message.contains("/private/host/ws-root"));
    }

    #[test]
    fn test_access_error_classification() {
        assert!(AccessError::not_found("a").is_not_found());
        assert!(!AccessError::not_found("a").is_rejected());
        assert!(AccessError::rejected(invalid_input("bad")).is_rejected());
        assert_eq!(
            AccessError::io("open", "a", io::Error::from_raw_os_error(libc::EACCES)).raw_os_error(),
            Some(libc::EACCES)
        );
    }
}
