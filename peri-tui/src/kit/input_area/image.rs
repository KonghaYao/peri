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
    let file = std::fs::File::create(output_path)?;
    let mut w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut w, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba_bytes)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_reference_ends_before_following_text() {
        let mut state = TextAreaState::default();

        insert_image_reference(&mut state, std::path::Path::new("/tmp/a.png"));
        state.insert_str(" 继续描述");

        assert_eq!(state.text, "@image /tmp/a.png\n 继续描述");
    }
}
