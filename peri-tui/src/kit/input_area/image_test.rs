use super::*;

#[test]
fn paste_gate_rejects_overlap_and_releases_on_return() {
    let gate = PasteGate::default();
    let permit = gate.try_acquire().unwrap();
    assert!(gate.clone().try_acquire().is_none());
    drop(permit);
    assert!(gate.try_acquire().is_some());
}

#[test]
fn paste_gate_releases_on_panic() {
    let gate = PasteGate::default();
    let worker_gate = gate.clone();
    let result = std::thread::spawn(move || {
        let _permit = worker_gate.try_acquire().unwrap();
        panic!("simulated clipboard failure");
    })
    .join();
    assert!(result.is_err());
    assert!(gate.try_acquire().is_some());
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use image::ImageEncoder;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
    use objc2_foundation::{NSData, NSString};

    // 命名剪贴板与用户的 generalPasteboard 隔离；退出时清空测试数据。
    struct TestPasteboard(objc2::rc::Retained<NSPasteboard>);

    impl TestPasteboard {
        fn new(bytes: Option<&[u8]>) -> Self {
            // 显式生成跨线程/进程的名字，不依赖 AppKit 的隐式命名状态。
            let name = NSString::from_str(&format!("peri.clipboard-test.{}", uuid::Uuid::new_v4()));
            let board = NSPasteboard::pasteboardWithName(&name);
            board.clearContents();
            if let Some(bytes) = bytes {
                let data = NSData::with_bytes(bytes);
                assert!(unsafe { board.setData_forType(Some(&data), NSPasteboardTypePNG) });
            }
            Self(board)
        }
    }

    impl Drop for TestPasteboard {
        fn drop(&mut self) {
            self.0.clearContents();
        }
    }

    #[test]
    fn clipboard_png_is_saved_byte_for_byte_without_pixel_conversion() {
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.png");
            png_encode(&[17, 33, 65, 128], 1, 1, &source).unwrap();
            let bytes = std::fs::read(source).unwrap();
            let board = TestPasteboard::new(Some(&bytes));
            let output = save_pasteboard_png(&board.0, dir.path()).unwrap().unwrap();
            assert_eq!(std::fs::read(output).unwrap(), bytes);
        });
    }

    #[test]
    fn concurrent_clipboards_keep_independent_names_and_png_bytes() {
        const WORKERS: usize = 16;
        let barrier = std::sync::Barrier::new(WORKERS);
        let names = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..WORKERS)
                .map(|index| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        objc2::rc::autoreleasepool(|_| {
                            let dir = tempfile::tempdir().unwrap();
                            let source = dir.path().join("source.png");
                            png_encode(&[index as u8, 33, 65, 128], 1, 1, &source).unwrap();
                            let bytes = std::fs::read(source).unwrap();
                            barrier.wait();
                            let board = TestPasteboard::new(None);
                            let name = board.0.name().to_string();
                            barrier.wait();
                            let data = NSData::with_bytes(&bytes);
                            let written = unsafe {
                                board.0.setData_forType(Some(&data), NSPasteboardTypePNG)
                            };
                            // 所有写入结束后再读取，确保能揭示不同 fixture 共用剪贴板。
                            barrier.wait();
                            let output = save_pasteboard_png(&board.0, dir.path());
                            let saved = output
                                .as_ref()
                                .ok()
                                .and_then(|path| path.as_ref())
                                .map(std::fs::read);
                            // 读取全部完成前不 Drop，避免清理干扰其他 worker 的证据。
                            barrier.wait();
                            assert!(written);
                            assert_eq!(saved.unwrap().unwrap(), bytes, "剪贴板 {name}");
                            name
                        })
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(
            unique.len(),
            WORKERS,
            "每个 fixture 必须拥有独立剪贴板: {names:?}"
        );
    }

    #[test]
    fn clipboard_without_png_allows_legacy_fallback() {
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let board = TestPasteboard::new(None);
            assert!(save_pasteboard_png(&board.0, dir.path()).unwrap().is_none());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        });
    }

    #[test]
    fn clipboard_invalid_png_is_error_not_legacy_fallback() {
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let board = TestPasteboard::new(Some(b"not a PNG"));
            assert!(save_pasteboard_png(&board.0, dir.path()).is_err());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        });
    }

    #[test]
    fn clipboard_oversized_png_is_rejected_before_writing() {
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let bytes = vec![0; crate::kit::image_safety::MAX_IMAGE_BYTES as usize + 1];
            let board = TestPasteboard::new(Some(&bytes));
            assert!(save_pasteboard_png(&board.0, dir.path()).is_err());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        });
    }

    // 手动性能实验：合成 4K 桌面图，对比 arboard 的 TIFF→RGBA→PNG 链路。
    // 不设耗时阈值，避免机器负载影响常规回归测试。
    #[test]
    #[ignore = "manual clipboard performance comparison"]
    fn clipboard_png_performance_comparison() {
        let subscriber = tracing_subscriber::fmt().with_test_writer().finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let (width, height) = (3840usize, 2160usize);
            let pixels: Vec<u8> = (0..width * height)
                .flat_map(|i| {
                    let (x, y) = (i % width, i / width);
                    [(x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8, 255]
                })
                .collect();
            let mut tiff = std::io::Cursor::new(Vec::new());
            image::codecs::tiff::TiffEncoder::new(&mut tiff)
                .write_image(
                    &pixels,
                    width as u32,
                    height as u32,
                    image::ExtendedColorType::Rgba8,
                )
                .unwrap();
            let source = dir.path().join("source.png");
            png_encode(&pixels, width, height, &source).unwrap();
            let png = std::fs::read(source).unwrap();
            let board = TestPasteboard::new(Some(&png));
            let start = std::time::Instant::now();
            let decoded =
                image::load_from_memory_with_format(tiff.get_ref(), image::ImageFormat::Tiff)
                    .unwrap()
                    .into_rgba8();
            png_encode(
                decoded.as_raw(),
                width,
                height,
                &dir.path().join("legacy.png"),
            )
            .unwrap();
            let legacy = start.elapsed();
            let start = std::time::Instant::now();
            let output = save_pasteboard_png(&board.0, dir.path()).unwrap().unwrap();
            let native = start.elapsed();
            assert_eq!(std::fs::read(output).unwrap(), png);
            tracing::info!(
                ?legacy,
                ?native,
                rgba_bytes = pixels.len(),
                png_bytes = png.len(),
                "clipboard performance comparison"
            );
        });
    }

    #[test]
    fn clipboard_png_write_failure_is_error_not_legacy_fallback() {
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.png");
            png_encode(&[0; 4], 1, 1, &source).unwrap();
            let bytes = std::fs::read(&source).unwrap();
            let board = TestPasteboard::new(Some(&bytes));
            assert!(save_pasteboard_png(&board.0, &source).is_err());
            assert_eq!(std::fs::read(source).unwrap(), bytes);
        });
    }
}

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
