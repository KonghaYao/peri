//! 图片浮动预览的状态机与来源身份（image-p0-p1-spec §7.3 T7）。
//!
//! 从 `atoms` 拆出：状态机（`Idle/Loading/Ready/Degraded/Error`）与预览身份
//! （路径 / 待发送附件 / 已提交附件）是预览领域自身的模型，由浮动层、消息区
//! 与测试共用。`atoms` 只保留全局句柄的再导出，避免其继续膨胀。

use image::DynamicImage;
use ratatui_kit::prelude::Atom as AtomStatic;
use std::path::PathBuf;
use std::sync::Arc;

use crate::kit::image_safety::{ImageMeta, PathGrade};

/// 预览对象身份（§7.3）。
///
/// [Why 显式区分来源] 预览状态机、meta 行与绘制都只需要「展示名 + 像素」；
/// `@image <path>` 流用磁盘路径，粘贴流的待发送附件没有路径、也没有磁盘文件
/// （上传式，见 `input_area/paste.rs`）。用 `PathBuf` 冒充附件名会掩盖这一点，
/// 故身份显式化为三种来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewSource {
    /// `@image <path>` 流：磁盘图片路径。
    Path(PathBuf),
    /// 粘贴流：待发送附件（composer 中）的展示名（`图片 N`）与载荷身份。
    Pending { name: String, payload_id: u64 },
    /// 粘贴流：已提交附件（消息区记录行）——载荷取自
    /// [`crate::kit::atoms::RETAINED_ATTACHMENTS`] 有界保留层。
    Submitted { name: String, payload_id: u64 },
}

impl PreviewSource {
    /// 展示名（meta 行用）：路径流取 `file_name`（不暴露绝对路径，§6.2-5）；
    /// 附件流直接取展示名。
    pub fn display_name(&self) -> String {
        match self {
            Self::Path(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            Self::Pending { name, .. } | Self::Submitted { name, .. } => name.clone(),
        }
    }
}

/// 图片预览状态机（image-p0-p1-spec §7.3 T7）：写入边界为事件/effect 与后台
/// 解码线程，渲染 body 只读（TUI-RENDER-001）。默认 `Idle`（无预览）。
#[derive(Debug, Clone, Default)]
pub enum ImagePreviewState {
    #[default]
    Idle,
    /// 后台解码中（路径流已过 T5 全链校验；`grade` 为分级结果）。
    Loading {
        source: PreviewSource,
        grade: PathGrade,
    },
    /// 解码完成：像素数据以 `Arc` 共享给 overlay 渲染（同来源重复请求复用）。
    Ready {
        source: PreviewSource,
        meta: ImageMeta,
        img: Arc<DynamicImage>,
    },
    /// 手工路径/非受管理目录（§6.1 Q6）：仅文本 meta 行降级，不触发解码。
    Degraded {
        source: PreviewSource,
        reason: String,
    },
    /// 校验/解码失败：固定错误文案（安全降级，reason 仅内部诊断，不含路径）。
    Error {
        source: PreviewSource,
        reason: String,
    },
}

pub static IMAGE_PREVIEW_STATE: AtomStatic<ImagePreviewState> =
    AtomStatic::new(ImagePreviewState::default);
