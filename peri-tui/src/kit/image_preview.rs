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
use std::sync::{Mutex, OnceLock};
use std::thread;

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::crossterm::terminal;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget as _;
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
/// 不走 `from_query_stdio`（不读写 stdin）。拿到真实单元格像素时（`TIOCGWINSZ`
/// 的像素字段）**Kitty 与 Sixel 都用它**构造字体尺寸：编码像素网格必须与终端
/// 实际单元格一致，否则图片会被拉伸（宽高比失真）——这是 Ghostty 下「看起来
/// 不铺满/比例不对」的根因之一。取不到时退回 halfblocks 默认 10×20（仅影响
/// 清晰度；布局由单元格数决定）。
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
    let cell_px = cell_px.filter(|cell| is_valid_cell_size(*cell));
    let mut picker = match cell_px {
        Some(cell) if caps.graphics != GraphicsProtocol::None => picker_with_cell_size(cell),
        _ => Picker::halfblocks(),
    };
    match caps.graphics {
        GraphicsProtocol::Kitty => picker.set_protocol_type(ProtocolType::Kitty),
        GraphicsProtocol::Sixel if cell_px.is_some() => {
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

/// 面积固定的内联图片协议：一次性 resize+encode 后跨帧只写占位符。
///
/// 与 [`AsyncImage`] 的取舍：内联图片的目标单元格由缓存键（cols/rows）固定，
/// 滚动、重排与 hover 都不改变图片面积，因此不需要运行时 resize；在后台线程
/// 编码完成后即可直接绘制。`AsyncImage` 的 resize 完成事件需要额外的唤醒源，
/// 消息区没有逐帧轮询，固定面积下没必要引入。
pub struct InlineImage {
    protocol: Protocol,
    cols: u16,
    rows: u16,
    /// 终端侧保留数据的估算字节（缓存预算用）：编码像素 ≈ 单元格 × 字体像素。
    bytes: usize,
}

impl std::fmt::Debug for InlineImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineImage")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl InlineImage {
    /// 在调用线程完成 resize+encode（只应在后台线程调用，编码可能耗时）。
    ///
    /// 默认 [Resize::Fit] 语义：**不放大**，只按等比缩小到 `cols`×`rows`。
    /// 需要「铺满」时用 [`InlineImageEncoder::encode_scaled`]（目标格数由
    /// `image_view::sizing` 先算好）。
    #[must_use]
    pub fn encode(picker: &Picker, img: DynamicImage, cols: u16, rows: u16) -> Option<Self> {
        Self::encode_with(picker, img, cols, rows, Resize::Fit(None))
    }

    /// 按显式 resize 策略编码：`cols`/`rows` 是调用方算好的目标单元格。
    ///
    /// [Resize::Scale] 允许放大，`Resize::Fit` 不会——「铺满」的调用方必须先经
    /// `image_view::sizing::fit` 得到目标格数再用 `Scale`，否则小图永远停在自然尺寸。
    /// 失败只记日志并返回 `None`（内联路径统一回落到文本降级）。
    #[must_use]
    pub fn encode_with(
        picker: &Picker,
        img: DynamicImage,
        cols: u16,
        rows: u16,
        resize: Resize,
    ) -> Option<Self> {
        if cols == 0 || rows == 0 {
            return None;
        }
        let font = picker.font_size();
        let target = resize.size_for(&img, font, Size::new(cols, rows));
        if target.width == 0 || target.height == 0 {
            return None;
        }
        // `Scale` 的实际格数由传入的目标区域决定（像素按比例 padding）；
        // `Fit` 可能因不放大而更小，取 `size_for` 的结果。
        let (cols, rows) = match resize {
            Resize::Scale(_) => (cols, rows),
            Resize::Fit(_) | Resize::Crop(_) => (target.width, target.height),
        };
        let protocol = match picker.new_protocol(img, Size::new(cols, rows), resize) {
            Ok(protocol) => protocol,
            Err(err) => {
                tracing::warn!(error = %err, "内联图片 resize/encode 失败，回落文本降级");
                return None;
            }
        };
        let bytes = usize::from(cols)
            .saturating_mul(usize::from(rows))
            .saturating_mul(usize::from(font.width.max(1)))
            .saturating_mul(usize::from(font.height.max(1)))
            .saturating_mul(4);
        Some(Self {
            protocol,
            cols,
            rows,
            bytes,
        })
    }

    /// 真实占用的列数（等比适配后的编码宽度）。
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// 真实占用的行数（等比适配后的编码高度）。
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// 终端侧保留数据的估算字节（协议串/占位符），供调用方做缓存预算。
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    /// 在 `area` 内绘制当前帧；面积必须不小于编码尺寸（不足时调用方应先裁剪）。
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        ratatui_image::Image::new(&self.protocol).render(area, buf);
    }
}

/// 内联图片编码器：把能力位 → `Picker` 的构造收在本模块内，调用方只持有
/// 不透明句柄即可在后台线程编码（不泄漏 ratatui-image 类型）。
#[derive(Clone)]
pub struct InlineImageEncoder {
    picker: Picker,
}

impl InlineImageEncoder {
    /// 协议可用时构造编码器；None/ITerm2/缺单元格尺寸的 Sixel 返回 `None`，
    /// 调用方据此在请求前就不做任何 IO。
    #[must_use]
    pub fn for_caps(caps: &TerminalCaps, cell_px: Option<(u16, u16)>) -> Option<Self> {
        supported_with_cell_size(caps, cell_px).then(|| Self {
            picker: picker_for_with_cell_size(caps, cell_px),
        })
    }

    /// 编码器使用的单元格像素（真实探测值，或 halfblocks 默认 10×20）。
    /// 几何计算在拿不到 `TIOCGWINSZ` 像素字段时用它保持一致性。
    #[must_use]
    pub fn font_size(&self) -> (u16, u16) {
        let font = self.picker.font_size();
        (font.width.max(1), font.height.max(1))
    }

    /// 在调用线程完成 resize+encode；面积预算由调用方给出（`Fit` 语义）。
    #[must_use]
    pub fn encode(&self, img: DynamicImage, cols: u16, rows: u16) -> Option<InlineImage> {
        InlineImage::encode(&self.picker, img, cols, rows)
    }

    /// 按算好的目标格数放大/缩小铺满（`Resize::Scale`）。
    #[must_use]
    pub fn encode_scaled(&self, img: DynamicImage, cols: u16, rows: u16) -> Option<InlineImage> {
        InlineImage::encode_with(&self.picker, img, cols, rows, Resize::Scale(None))
    }
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
    if !protocol_available(caps, cell_px) {
        return false;
    }
    let expected = match caps.graphics {
        GraphicsProtocol::Kitty => ProtocolType::Kitty,
        GraphicsProtocol::Sixel if cell_px.is_some_and(is_valid_cell_size) => ProtocolType::Sixel,
        GraphicsProtocol::Sixel | GraphicsProtocol::ITerm2 | GraphicsProtocol::None => {
            return false;
        }
    };
    picker_for_with_cell_size(caps, cell_px).protocol_type() == expected
}

/// 轻量能力判定：只回答「能力位 + 单元格尺寸是否足以走像素渲染」。
///
/// 与 [`supported_with_cell_size`] 同一门控规则，但不构造 `Picker`——布局预算
/// 每帧都要问一次，`Picker::halfblocks` 在 tmux 下有 spawn 副作用，不能进渲染路径。
pub(crate) fn protocol_available(caps: &TerminalCaps, cell_px: Option<(u16, u16)>) -> bool {
    match caps.graphics {
        GraphicsProtocol::Kitty => true,
        GraphicsProtocol::Sixel => cell_px.is_some_and(is_valid_cell_size),
        GraphicsProtocol::ITerm2 | GraphicsProtocol::None => false,
    }
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
    /// 原始像素尺寸与编码用的单元格像素（供调用方走共享 sizing 计算目标矩形）。
    dims: (u32, u32),
    cell_px: (u16, u16),
}

impl AsyncImage {
    /// 构造：从 picker 取 `StatefulProtocol` 并启动后台 resize/encode 线程。
    /// 图片所有权转移给协议（内部保留原始 `DynamicImage` 供后续重编码）。
    #[must_use]
    pub fn new(picker: &Picker, img: DynamicImage) -> Self {
        let dims = (img.width(), img.height());
        let font = picker.font_size();
        let cell_px = (font.width.max(1), font.height.max(1));
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
            dims,
            cell_px,
        }
    }

    /// 原始像素尺寸（宽, 高）。
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        self.dims
    }

    /// 编码使用的单元格像素（与 Picker 字体尺寸一致）。
    #[must_use]
    pub fn cell_px(&self) -> (u16, u16) {
        self.cell_px
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
    ///
    /// `area` 应是调用方按等比铺满算出的目标矩形（`image_view::sizing`），
    /// 不是整块预留区——`Fit` 不放大，画在更大的区域内会在右侧/下方留白。
    pub fn render_at(&mut self, area: Rect, buf: &mut Buffer) {
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

/// `Picker` 进程级缓存：单元格像素尺寸随终端字体变化；缓存按（能力位, 尺寸）
/// 失效，避免交互式浮层反复构造（tmux 探测有 spawn 副作用）。
///
/// 放在本模块（唯一 `ratatui-image` 边界）而不是 `image_view`：上层只拿不透明
/// 句柄转交 [`AsyncImage`]，不直接依赖第三方类型。
static PICKER: OnceLock<Mutex<Option<CachedPicker>>> = OnceLock::new();

struct CachedPicker {
    caps: TerminalCaps,
    cell_px: Option<(u16, u16)>,
    picker: Picker,
}

/// 按当前能力位与单元格像素取共享 `Picker`。
pub(crate) fn shared_picker(caps: &TerminalCaps, cell_px: Option<(u16, u16)>) -> Picker {
    let cache = PICKER.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = guard.as_ref()
        && cached.cell_px == cell_px
        && cached.caps == *caps
    {
        return cached.picker.clone();
    }
    let picker = picker_for_with_cell_size(caps, cell_px);
    *guard = Some(CachedPicker {
        caps: *caps,
        cell_px,
        picker: picker.clone(),
    });
    picker
}
