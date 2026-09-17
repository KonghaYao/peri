use crate::components::textarea::TextAreaState;

// 在启动线程前获取许可；释放覆盖正常返回、错误和 panic，避免重复按键叠加整图分配。
#[derive(Default, Clone)]
pub(super) struct PasteGate(std::sync::Arc<std::sync::atomic::AtomicBool>);

pub(super) struct PastePermit(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl PasteGate {
    pub(super) fn try_acquire(&self) -> Option<PastePermit> {
        self.0
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()
            .map(|_| PastePermit(self.0.clone()))
    }
}

impl Drop for PastePermit {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
pub(super) fn save_native_clipboard_png() -> anyhow::Result<Option<std::path::PathBuf>> {
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        save_pasteboard_png(
            &pasteboard,
            &crate::kit::image_safety::managed_images_root(),
        )
    })
}

#[cfg(target_os = "macos")]
fn save_pasteboard_png(
    pasteboard: &objc2_app_kit::NSPasteboard,
    directory: &std::path::Path,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    // PNG 缺席才允许回退 TIFF；超限或读取/落盘失败不能触发更昂贵的解码。
    let Some(data) = (unsafe { pasteboard.dataForType(objc2_app_kit::NSPasteboardTypePNG) }) else {
        return Ok(None);
    };
    anyhow::ensure!(
        data.len() as u64 <= crate::kit::image_safety::MAX_IMAGE_BYTES,
        "clipboard PNG exceeds byte limit"
    );
    // SAFETY: 保持 NSData 存活且不修改内容，借用仅在同步校验和写盘期间有效。
    let bytes = unsafe { data.as_bytes_unchecked() };
    // 只读 IHDR，不分配像素缓冲；完整解码仍由使用图片的受限入口负责。
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.read_header_info()?;
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{}.png", uuid::Uuid::now_v7()));
    std::fs::write(&path, bytes)?;
    Ok(Some(path))
}

/// 在当前光标处插入独占一行的 `@image <path>` 引用。
///
/// 图片路径按行尾结束；前后补换行可避免用户粘贴图片后继续输入的文本被解析为路径。
pub(crate) fn insert_image_reference(state: &mut TextAreaState, output_path: &std::path::Path) {
    state.delete_selection();
    let previous = state
        .cursor
        .checked_sub(1)
        .and_then(|index| state.text.chars().nth(index));
    let next = state.text.chars().nth(state.cursor);
    let mut reference = format!("@image {}", output_path.display());

    if previous.is_some_and(|ch| ch != '\n') {
        reference.insert(0, '\n');
    }
    if next != Some('\n') {
        reference.push('\n');
    }

    state.insert_str(&reference);
    if next == Some('\n') {
        state.cursor_right();
    }
}

/// 将 RGBA 字节数组编码为 PNG 文件
pub(crate) fn png_encode(
    rgba_bytes: &[u8],
    width: usize,
    height: usize,
    output_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Error as IoError, ErrorKind, Write};

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, "image dimensions overflow"))?;
    if rgba_bytes.len() != expected_len {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            format!(
                "RGBA buffer length {} does not match expected {}",
                rgba_bytes.len(),
                expected_len
            ),
        )
        .into());
    }
    let width = u32::try_from(width)
        .map_err(|_| IoError::new(ErrorKind::InvalidInput, "image width exceeds PNG limit"))?;
    let height = u32::try_from(height)
        .map_err(|_| IoError::new(ErrorKind::InvalidInput, "image height exceeds PNG limit"))?;

    let file = std::fs::File::create(output_path)?;
    let mut w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // Stream the compressed IDAT chunks. `write_image_data` first builds the
    // complete compressed image in memory, which defeats the ownership win
    // above for large clipboard images.
    let mut png_writer = encoder.write_header()?;
    {
        let mut stream = png_writer.stream_writer()?;
        stream.write_all(rgba_bytes)?;
        stream.finish()?;
    }
    // Writer::finish writes IEND and flushes its underlying writer. Keeping
    // this explicit ensures errors are propagated instead of being swallowed
    // by Drop.
    png_writer.finish()?;
    w.flush()?;
    Ok(())
}

#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
