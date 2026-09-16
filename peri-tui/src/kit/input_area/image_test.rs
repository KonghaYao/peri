use super::*;

#[test]
fn image_reference_ends_before_following_text() {
    let mut state = TextAreaState::default();

    insert_image_reference(&mut state, std::path::Path::new("/tmp/a.png"));
    state.insert_str(" 继续描述");

    assert_eq!(state.text, "@image /tmp/a.png\n 继续描述");
}

#[test]
fn png_encode_round_trips_rgba_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    let width = 64;
    let height = 64;
    let mut seed = 0x1234_5678_u32;
    let pixels: Vec<u8> = (0..width * height * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();

    png_encode(&pixels, width, height, &path).unwrap();
    let encoded = std::fs::read(&path).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");

    let error = png_encode(&[0, 1, 2, 3], 2, 1, &path).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn png_encode_rejects_excess_pixel_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");

    let error = png_encode(&[0, 1, 2, 3, 4, 5, 6, 7, 8], 2, 1, &path).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

    let error = png_encode(&[0; 16], 2, 1, &path).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn png_encode_rejects_dimension_overflow_before_creating_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");

    let error = png_encode(&[], usize::MAX, 2, &path).unwrap_err();
    let error = error.downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!path.exists());
}
