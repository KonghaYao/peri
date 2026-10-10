use super::*;

/// 充足上限（与应用同源）：用于「与上限无关」的编码用例。
const MAX_ENCODE_BYTES: usize = crate::kit::image_safety::MAX_IMAGE_BYTES as usize;

fn rgba_pattern(width: usize, height: usize) -> Vec<u8> {
    let mut seed = 0x1234_5678_u32;
    (0..width * height * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect()
}

#[test]
fn png_encode_round_trips_rgba_pixels() {
    let width = 64;
    let height = 64;
    let pixels = rgba_pattern(width, height);

    let encoded = png_encode_bytes(&pixels, width, height, MAX_ENCODE_BYTES).unwrap();
    assert!(
        encoded.len() > 8192,
        "test must exercise multiple IDAT chunks"
    );
    let decoded = image::load_from_memory(&encoded).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (width as u32, height as u32));
    assert_eq!(decoded.as_raw(), &pixels);
}

#[test]
fn png_encode_rejects_incomplete_pixel_data() {
    let error = png_encode_bytes(&[0, 1, 2, 3], 2, 1, MAX_ENCODE_BYTES).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn png_encode_rejects_excess_pixel_data() {
    let error = png_encode_bytes(&[0, 1, 2, 3, 4, 5, 6, 7, 8], 2, 1, MAX_ENCODE_BYTES).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

    let error = png_encode_bytes(&[0; 16], 2, 1, MAX_ENCODE_BYTES).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn png_encode_rejects_dimension_overflow() {
    let error = png_encode_bytes(&[], usize::MAX, 2, MAX_ENCODE_BYTES).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

/// 上传上限：编码结果超过 limit 时立即失败（不产出超限载荷）。
///
/// 上传式附件不再落盘，超限必须在编码期就被拦住——否则整张图会白白编码成
/// base64 再被服务端拒绝。
#[test]
fn png_encode_rejects_output_over_limit() {
    let width = 64;
    let height = 64;
    let pixels = rgba_pattern(width, height);

    let error = png_encode_bytes(&pixels, width, height, 16).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    // 同一输入在充足上限下可正常编码——失败确由上限而非数据本身引起。
    assert!(png_encode_bytes(&pixels, width, height, MAX_ENCODE_BYTES).is_ok());
}
