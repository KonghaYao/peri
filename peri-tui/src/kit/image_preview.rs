//! 图片像素渲染窄接口（image-p0-p1-spec §6 T6）。
//!
//! 唯一允许与 `ratatui-image` 交互的模块：封装协议选择、Image widget 构造与
//! 跨帧状态持有，把第三方 API 隔离在模块内（S2 §10 窄接口约定；「不对外
//! 泄漏 ratatui-image 类型」指模块内 `use` 不外传，`pub` 面仅本文件签名）。
//!
//! 门控：全部经 [`TerminalCaps::graphics`]（T1 能力位）+ [`supported`] 二次判定；
//! 无协议 / ITerm2（检测但 disabled，§6.3）/ 禁用时恒降级文本（§0.2 三层降级）。
//!
//! 集成边界（S2 §1）：
//! - 不走 `Picker::from_query_stdio` 的 stdin 探测（与决策文档 §3.4「query 必须
//!   在 fullscreen 前」的时序约束解耦），协议由 `caps.graphics` 品牌映射决定；
//! - Sixel 直接按像素绘制，目标像素必须由真实单元格尺寸换算。本模块用
//!   `TIOCGWINSZ` 的像素字段（crossterm `window_size`）取尺寸；取不到时退回
//!   文本降级，宁可 false negative 也不让像素溢出预留区；
//! - `is_tmux` 无法外部注入（picker.rs `pub(crate)` 字段，S2 §1 边界 2）——
//!   tmux 内降级/乱码风险接受，T1 矩阵（TMUX 存在 → None）与 `PERI_IMAGE=off`
//!   逃生位已覆盖；
//! - WezTerm/Konsole 黑名单（S2 §1 边界 1）：本模块不触碰黑名单逻辑（那属于
//!   stdin 探测路径），兼容矩阵由 [`supported`] 二次判定兜底。
//!
//! resize 阻塞（S2 §6 官方明示）：动态面积走 [`AsyncImage`]（后台线程
//! resize+encode，渲染线程非阻塞）；面积固定用 [`static_protocol`]。
//! 无显式 placement 删除 API（S2 §7）：占位符从 buffer 消失即 kitty 自动清理，
//! alt screen 退出整体丢弃——T7 的隐藏/退出清理依赖此机制，不维护 id 表。

use std::sync::mpsc::{self, Receiver};
use std::thread;

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::crossterm::terminal;
use ratatui::layout::{Rect, Size};
use ratatui_image::FontSize;
use ratatui_image::errors::Errors;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::{Protocol, StatefulProtocol};
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{Resize, ResizeEncodeRender};

use super::terminal_caps::{GraphicsProtocol, TerminalCaps};

/// 终端单元格像素尺寸；终端不报告（非 TTY / 平台未实现）时返回 `None`。
///
/// Sixel 按像素直接落屏，只有拿到真实单元格尺寸才能把编码像素限制在预留
/// 单元格内；取不到时 [`supported`] 保持文本降级。
pub(crate) fn cell_pixel_size() -> Option<(u16, u16)> {
    let size = terminal::window_size().ok()?;
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return None;
    }
    let cell = (size.width / size.columns, size.height / size.rows);
    is_valid_cell_size(cell).then_some(cell)
}

fn is_valid_cell_size((width, height): (u16, u16)) -> bool {
    width > 0 && height > 0
}

/// 从 [`TerminalCaps`] 品牌映射构造 `Picker`。
///
/// 不走 `from_query_stdio`（不读写 stdin）。[`GraphicsProtocol::Kitty`] 强制
/// 对应 `ProtocolType`；[`GraphicsProtocol::Sixel`] 仅在拿到真实单元格尺寸时
/// 切换（编码像素与预留区一致），否则保持 halfblocks 安全态。ITerm2（检测但
/// disabled）与 None 同样保持 halfblocks，像素渲染由 [`supported`] 统一门控。
/// 注意：`Picker` 构造时按环境探测 tmux（TERM 以 `tmux` 开头或
/// `TERM_PROGRAM == "tmux"` 时 spawn `tmux set -p allow-passthrough on`）；
/// T1 探测矩阵保证自动探测下 `graphics` 非 None 时 TMUX 环境变量不存在，仅
/// `PERI_IMAGE=kitty|sixel` 强制场景可能触发（用户显式强制，S2 已接受风险）。
pub fn picker_for(caps: &TerminalCaps) -> Picker {
    picker_for_with_cell_size(caps, cell_pixel_size())
}

pub(crate) fn picker_for_with_cell_size(
    caps: &TerminalCaps,
    cell_px: Option<(u16, u16)>,
) -> Picker {
    let mut picker = match cell_px {
        Some(cell) if caps.graphics == GraphicsProtocol::Sixel && is_valid_cell_size(cell) => {
            picker_with_cell_size(cell)
        }
        _ => Picker::halfblocks(),
    };
    match caps.graphics {
        GraphicsProtocol::Kitty => picker.set_protocol_type(ProtocolType::Kitty),
        GraphicsProtocol::Sixel if cell_px.is_some_and(is_valid_cell_size) => {
            picker.set_protocol_type(ProtocolType::Sixel);
        }
        GraphicsProtocol::ITerm2 | GraphicsProtocol::Sixel | GraphicsProtocol::None => {}
    }
    picker
}

/// ratatui-image 9.0 起把固定 font size 的构造标为 deprecated（推荐 stdin
/// 查询）；本模块按 S2 边界不读 stdin，改用 ioctl 实测尺寸。
#[allow(deprecated)]
fn picker_with_cell_size((width, height): (u16, u16)) -> Picker {
    Picker::from_fontsize(FontSize::new(width, height))
}

/// 静态预览协议（面积固定）：渲染线程跨帧持有同一 `&Protocol`，
/// 首帧 transmit 一次（AtomicBool 机制，S2 §4），后续帧只写占位符。
///
/// 适合面积固定的 overlay 预览；面积会变化时用 [`stateful_protocol`] /
/// [`AsyncImage`]（resize 阻塞，S2 §6）。
pub fn static_protocol(
    picker: &Picker,
    img: DynamicImage,
    size: Size,
    resize: Resize,
) -> Result<Protocol, Errors> {
    picker.new_protocol(img, size, resize)
}

/// 动态 overlay 协议（跟随光标/缩放）：调用方持有 `StatefulProtocol`，
/// 配合 `ratatui_image::StatefulImage` 按 area 自适应。
///
/// resize+encode 在渲染线程**同步执行**（阻塞，S2 §6 官方明示）——
/// 交互路径应优先 [`AsyncImage`]。
pub fn stateful_protocol(picker: &Picker, img: DynamicImage) -> StatefulProtocol {
    picker.new_resize_protocol(img)
}

/// 降级判定：Kitty 恒定可用；Sixel 仅在拿到真实单元格尺寸时可用
/// （编码像素与预留区一致，防止溢出）；ITerm2 / None 恒 false
/// （决策文档 §8.2「可恢复降级」，宁可 false negative）。
///
/// 二次判定 `picker_for` 的协议类型（目前恒真：本模块强制设置、不触碰 stdin
/// 探测；保留判定点以覆盖未来版本内置黑名单降级的可能）。
pub fn supported(caps: &TerminalCaps) -> bool {
    supported_with_cell_size(caps, cell_pixel_size())
}

pub(crate) fn supported_with_cell_size(caps: &TerminalCaps, cell_px: Option<(u16, u16)>) -> bool {
    let expected = match caps.graphics {
        GraphicsProtocol::Kitty => ProtocolType::Kitty,
        GraphicsProtocol::Sixel if cell_px.is_some_and(is_valid_cell_size) => ProtocolType::Sixel,
        GraphicsProtocol::Sixel | GraphicsProtocol::ITerm2 | GraphicsProtocol::None => {
            return false;
        }
    };
    picker_for_with_cell_size(caps, cell_px).protocol_type() == expected
}

/// 后台 resize/encode 卸载的动态协议（S2 §6 非阻塞要求）。
///
/// 包装 `thread::ThreadProtocol`：`render` 只做「必要时发出 resize 请求 +
/// 渲染当前帧」，resize+encode 由后台线程完成；完成结果经 [`AsyncImage::poll_completed`]
/// 在事件/effect 边界取回并应用。
///
/// 帧语义：面积变化帧的 `render` 可能暂无可用帧（请求已发出、编码中），
/// 下一帧经 `poll_completed` 应用结果后出现——T7 用 `RENDER_HEARTBEAT` 补帧。
/// 后台线程 spawn 失败（资源不足）时降级为恒空帧：请求无人消费，不 panic。
pub struct AsyncImage {
    thread: ThreadProtocol,
    completed: Receiver<Result<ResizeResponse, Errors>>,
}

impl AsyncImage {
    /// 构造：从 picker 取 `StatefulProtocol` 并启动后台 resize/encode 线程。
    /// 图片所有权转移给协议（内部保留原始 `DynamicImage` 供后续重编码）。
    #[must_use]
    pub fn new(picker: &Picker, img: DynamicImage) -> Self {
        let (tx_request, rx_request) = mpsc::channel::<ResizeRequest>();
        let (tx_completed, rx_completed) = mpsc::channel();
        // 后台线程：消费 resize 请求并回传结果。自然退出：请求 channel 的
        // 所有 sender 断开（本对象 drop）后 `recv` 返回 Err。
        let _ = thread::Builder::new()
            .name("peri-image-resize".to_string())
            .spawn(move || {
                while let Ok(request) = rx_request.recv() {
                    // UI 线程已 drop 时 send 失败，忽略并继续（直到 channel 断开）。
                    let _ = tx_completed.send(request.resize_encode());
                }
            });
        Self {
            thread: ThreadProtocol::new(tx_request, Some(picker.new_resize_protocol(img))),
            completed: rx_completed,
        }
    }

    /// 取回后台完成的 resize/encode 结果；返回是否应用了更新（尺寸变化帧
    /// 据此补渲染）。应在事件/effect 边界调用，不在 render body 调用
    /// （TUI-RENDER-001）。编码失败（如解码结果异常）记录并丢弃，恒降级。
    pub fn poll_completed(&mut self) -> bool {
        let mut updated = false;
        while let Ok(completed) = self.completed.try_recv() {
            match completed {
                Ok(response) => {
                    // id 不匹配（过期请求）返回 false，丢弃。
                    updated = updated || self.thread.update_resized_protocol(response);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "图片 resize/encode 失败，跳过该帧");
                }
            }
        }
        updated
    }

    /// 当前已编码帧相对给定面积是否需要 resize；`Some(area)` 表示需要
    /// （调用方可用作事件边界的重编码触发判定）。
    pub fn needs_resize(&self, resize: &Resize, size: Size) -> Option<Size> {
        self.thread.needs_resize(resize, size)
    }

    /// 渲染当前帧（`Resize::Fit(None)` 语义）；必要时**非阻塞**发出
    /// resize 请求（S2 §6：不阻塞 UI 线程）。
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let resize = Resize::Fit(None);
        if let Some(rect) = self.thread.needs_resize(&resize, area.into()) {
            self.thread.resize_encode(&resize, rect);
        }
        self.thread.render(area, buf);
    }
}

#[cfg(test)]
#[path = "image_preview_test.rs"]
mod tests;
