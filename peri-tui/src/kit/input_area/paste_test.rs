use super::*;
use crate::kit::atoms::PENDING_ATTACHMENTS;

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

/// 上传式附件载荷：base64 解码后必须与剪贴板字节逐字节一致（不经像素转换）。
#[test]
fn attach_image_stores_base64_payload_without_reencoding() {
    use base64::Engine;
    crate::kit::atoms::init_atoms();
    PENDING_ATTACHMENTS.state().write().clear();

    attach_image(vec![0x89, 0x50, 0x4E, 0x47], "image/png");

    let state = PENDING_ATTACHMENTS.state();
    let pending = state.read();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].media_type, "image/png");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&pending[0].base64_data)
            .unwrap(),
        vec![0x89, 0x50, 0x4E, 0x47]
    );
    drop(pending);
    state.write().clear();
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

    fn png_bytes(width: usize, height: usize) -> Vec<u8> {
        let mut seed = 0x1234_5678_u32;
        let pixels: Vec<u8> = (0..width * height * 4)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        crate::kit::input_area::image::png_encode_bytes(&pixels, width, height, usize::MAX).unwrap()
    }

    #[test]
    fn clipboard_png_reads_byte_for_byte_without_pixel_conversion() {
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.png");
            let pixels = [17u8, 33, 65, 128];
            let encoder =
                image::codecs::png::PngEncoder::new(std::fs::File::create(&source).unwrap());
            encoder
                .write_image(&pixels, 1, 1, image::ExtendedColorType::Rgba8)
                .unwrap();
            let bytes = std::fs::read(source).unwrap();
            let board = TestPasteboard::new(Some(&bytes));
            let read = pasteboard_png_bytes(&board.0).unwrap().unwrap();
            assert_eq!(read, bytes, "剪贴板 PNG 必须原样上传（不重编码）");
        });
    }

    #[test]
    fn clipboard_without_png_allows_legacy_fallback() {
        objc2::rc::autoreleasepool(|_| {
            let board = TestPasteboard::new(None);
            assert!(pasteboard_png_bytes(&board.0).unwrap().is_none());
        });
    }

    /// 无 PNG 数据时返回 None；有数据但不是 PNG（无 IHDR）时是错误，不是回落。
    #[test]
    fn clipboard_invalid_png_is_error_not_legacy_fallback() {
        objc2::rc::autoreleasepool(|_| {
            let board = TestPasteboard::new(Some(b"not a PNG"));
            assert!(pasteboard_png_bytes(&board.0).is_err());
        });
    }

    #[test]
    fn clipboard_oversized_png_is_rejected() {
        objc2::rc::autoreleasepool(|_| {
            let bytes = vec![0; crate::kit::image_safety::MAX_IMAGE_BYTES as usize + 1];
            let board = TestPasteboard::new(Some(&bytes));
            assert!(pasteboard_png_bytes(&board.0).is_err());
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
                            let bytes = png_bytes(8 + index, 8);
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
                            let read = pasteboard_png_bytes(&board.0);
                            // 读取全部完成前不 Drop，避免清理干扰其他 worker 的证据。
                            barrier.wait();
                            assert!(written);
                            assert_eq!(read.unwrap().unwrap(), bytes, "剪贴板 {name}");
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

    // 手动性能实验：合成 4K 桌面图，对比 arboard 的 TIFF→RGBA→PNG 链路。
    // 不设耗时阈值，避免机器负载影响常规回归测试。
    #[test]
    #[ignore = "manual clipboard performance comparison"]
    fn clipboard_png_performance_comparison() {
        let subscriber = tracing_subscriber::fmt().with_test_writer().finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        objc2::rc::autoreleasepool(|_| {
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
            let png =
                crate::kit::input_area::image::png_encode_bytes(&pixels, width, height, usize::MAX)
                    .unwrap();
            let board = TestPasteboard::new(Some(&png));
            let start = std::time::Instant::now();
            let decoded =
                image::load_from_memory_with_format(tiff.get_ref(), image::ImageFormat::Tiff)
                    .unwrap()
                    .into_rgba8();
            let legacy_png = crate::kit::input_area::image::png_encode_bytes(
                decoded.as_raw(),
                width,
                height,
                usize::MAX,
            )
            .unwrap();
            let legacy = start.elapsed();
            let start = std::time::Instant::now();
            let native = pasteboard_png_bytes(&board.0).unwrap().unwrap();
            let native_elapsed = start.elapsed();
            assert_eq!(native, png);
            assert!(!legacy_png.is_empty());
            tracing::info!(
                ?legacy,
                ?native_elapsed,
                rgba_bytes = pixels.len(),
                png_bytes = png.len(),
                "clipboard performance comparison"
            );
        });
    }
}
