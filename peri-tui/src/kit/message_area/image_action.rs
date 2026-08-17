use super::hits::{ImageHoverState, ImageLineHit};

// ── [T4 §4] @image 行交互：hover 目标解析 + 点击 open ────────────────────

/// Moved 事件 hover 目标解析（§4.4）：命中 [`ImageLineHit`] → [`ImageHoverState`]；
/// 未命中或遮挡（`mouse_router::is_occluded`）→ None（恢复默认渲染）。
/// 纯函数（mod_test 直调锁定）——handler 只做「命中集合变化才写」的胶水。
pub(super) fn hover_target_for(
    hits: &[ImageLineHit],
    x: u16,
    y: u16,
    occluded: bool,
) -> Option<ImageHoverState> {
    if occluded {
        return None;
    }
    hits.iter()
        .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
        .map(|h| ImageHoverState {
            row: h.row,
            slot_index: h.slot_index,
            logical_idx: h.logical_idx,
            vm_hash: h.vm_hash,
            path: h.path.clone(),
            size_text: h.size_text.clone(),
        })
}

/// open 命令构建失败原因（错误文本不含路径细节，RUST-ERROR-001）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenImageError {
    /// 非 macOS 平台尚未验证（§4.6：安全降级，仅记录日志不 spawn）。
    UnsupportedPlatform,
    /// T5 校验失败（非常规文件 / 扩展名 / 大小上限等）。
    ValidationFailed,
}

/// 构建打开图片的 open 命令（参数化，**禁止 shell 拼接**，§6.2-6）。
///
/// 平台选型（§4.2）：macOS `open`；Windows `cmd /C start`、Linux `xdg-open`
/// 未验证前返回 [`OpenImageError::UnsupportedPlatform`]（§4.6 安全降级——
/// 不 spawn）。打开前过 T5 校验（常规文件 + 扩展名 + 大小上限；§4.4）。
/// stdout/stderr 重定向 null——detach 不阻塞 TUI、不继承终端。
///
/// [F8 残余窗口] 调用方传 canonical 路径（T4 `ImageLineHit.path`，render.rs
/// 注释），但 `open` 命令本身按路径打开：T5 校验与 spawn 之间文件仍可能被
/// 替换（OS 语义固有，无法经 fd 传递到外部命令）——登记为低危残余窗口。
pub(crate) fn build_open_command(path: &str) -> Result<std::process::Command, OpenImageError> {
    build_open_command_with(path, "open")
}

/// [`build_open_command`] 的二进制名注入版（P2-7：成功 spawn 路径测试
/// 注入 `/bin/echo` 等，不依赖真实 Finder；生产路径恒为 `"open"`）。
pub(super) fn build_open_command_with(
    path: &str,
    open_bin: &str,
) -> Result<std::process::Command, OpenImageError> {
    if !cfg!(target_os = "macos") {
        return Err(OpenImageError::UnsupportedPlatform);
    }
    crate::kit::image_safety::validate_image_file(std::path::Path::new(path))
        .map_err(|_| OpenImageError::ValidationFailed)?;
    let mut cmd = std::process::Command::new(open_bin);
    cmd.arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    Ok(cmd)
}

/// 点击 open 完整链路（§4.4）：T5 校验 → 参数化 Command → detach spawn。
/// 校验失败 → NOTIFICATION 提示（paste-truncated 通知模式）；未支持平台
/// （非 macOS）→ 仅记录日志不 spawn（§4.6 安全降级）。返回是否成功 spawn。
pub(crate) fn try_open_image(path: &str) -> bool {
    match build_open_command(path) {
        Ok(mut cmd) => match cmd.spawn() {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(error = %e, "image open: spawn failed");
                false
            }
        },
        Err(OpenImageError::UnsupportedPlatform) => {
            tracing::warn!("image open: platform not supported (macOS verified only)");
            false
        }
        Err(OpenImageError::ValidationFailed) => {
            show_open_failed_notification();
            false
        }
    }
}

/// 打开失败状态栏提示（参照 submit_blocked 的 paste-truncated 通知模式）。
fn show_open_failed_notification() {
    *crate::kit::atoms::NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
        message: crate::i18n::tr("user-image-open-failed"),
        until: std::time::Instant::now() + std::time::Duration::from_secs(3),
    });
    crate::kit::atoms::RENDER_HEARTBEAT
        .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
}
