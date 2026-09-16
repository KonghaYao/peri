use crate::components::textarea::TextAreaState;

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
