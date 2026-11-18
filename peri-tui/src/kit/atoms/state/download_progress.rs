//! 主题下载进度弹窗的全局状态。

use ratatui_kit::prelude::Atom as AtomStatic;

/// 下载进度中的单文件状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDownloadStatus {
    /// 等待下载
    Pending,
    /// 正在下载
    Downloading,
    /// 下载完成
    Done,
    /// 下载失败（包含错误信息）
    Failed(String),
}

/// 下载进度条目
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    pub filename: String,
    pub status: FileDownloadStatus,
}

/// 下载进度弹窗的完整状态
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DownloadProgressPayload {
    pub items: Vec<DownloadItem>,
    /// 下载是否已完成（true 时 Esc 可关闭弹窗）
    pub finished: bool,
    /// 成功下载的文件数量
    pub success_count: usize,
    /// 失败的文件数量
    pub fail_count: usize,
}

pub static DOWNLOAD_PROGRESS: AtomStatic<DownloadProgressPayload> =
    AtomStatic::new(DownloadProgressPayload::default);
