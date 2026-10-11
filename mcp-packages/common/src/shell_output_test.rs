use super::ShellOutputCapture;

#[tokio::test]
async fn capture_preserves_unicode_and_large_tail() {
    let mut capture = ShellOutputCapture::new("shell-output-test");
    let stdout_path = capture.stdout_path().expect("stdout file");
    let stderr_path = capture.stderr_path().expect("stderr file");
    let mut stdout = capture.stdout_writer();
    let mut stderr = capture.stderr_writer();
    let body = "前缀🙂".repeat(1024 * 512);
    stdout.write_chunk(body.as_bytes()).await;
    stderr.write_chunk("stderr-尾部\n".as_bytes()).await;
    stdout.finish().await;
    stderr.finish().await;

    let evidence = capture.finish(Some(7));
    assert!(evidence.complete);
    assert_eq!(evidence.exit_code, Some(7));
    assert_eq!(
        std::fs::read(&stdout_path).expect("stdout contents"),
        body.as_bytes()
    );
    assert_eq!(
        std::fs::read_to_string(&stderr_path).expect("stderr contents"),
        "stderr-尾部\n"
    );
    capture.cleanup().await;
}

#[tokio::test]
async fn finish_before_eof_is_incomplete() {
    let mut capture = ShellOutputCapture::new("shell-output-early-finish");
    let mut stdout = capture.stdout_writer();
    let mut stderr = capture.stderr_writer();
    stdout.write_chunk(b"partial").await;
    let early = capture.finish(None);
    assert!(!early.complete);
    assert!(early
        .error
        .as_deref()
        .is_some_and(|error| error.contains("not finalized")));
    stdout.finish().await;
    stderr.finish().await;
    assert!(capture.finish(None).complete);
    capture.cleanup().await;
}

#[tokio::test]
async fn discarded_writer_is_reported_incomplete() {
    let mut capture = ShellOutputCapture::new("shell-output-discarded");
    let stdout = capture.stdout_writer();
    drop(stdout);
    let mut stderr = capture.stderr_writer();
    stderr.finish().await;
    let evidence = capture.finish(None);
    assert!(!evidence.complete);
    assert!(evidence
        .error
        .as_deref()
        .is_some_and(|error| error.contains("stdout output stream was not finalized")));
    capture.cleanup().await;
}

#[test]
fn creation_failure_is_explicit() {
    let prefix = format!("missing-output-parent-{}/stream", uuid::Uuid::new_v4());
    let capture = ShellOutputCapture::new(&prefix);
    let evidence = capture.finish(None);
    assert!(!evidence.complete);
    assert!(evidence.error.is_some());
    assert!(evidence.stdout_path.is_none());
    assert!(evidence.stderr_path.is_none());
}

#[tokio::test]
async fn write_failure_is_explicit() {
    let mut capture = ShellOutputCapture::new("shell-output-write-failure");
    let mut stdout = capture.stdout_writer();
    let file = std::fs::File::open(capture.stdout_path().unwrap()).expect("open read-only file");
    stdout.0.as_mut().expect("stdout writer").file = tokio::fs::File::from_std(file);
    stdout
        .write_chunk(b"cannot write to a read-only file")
        .await;
    // Tokio may report the blocking write error from the next flush.
    stdout.finish().await;
    let mut stderr = capture.stderr_writer();
    stderr.finish().await;
    let evidence = capture.finish(None);
    assert!(!evidence.complete);
    assert!(evidence
        .error
        .as_deref()
        .is_some_and(|error| error.contains("stdout output file")));
    capture.cleanup().await;
}

/// [回归测试] 取消前台执行后，未发布的输出文件应随后台 pipe reader 一起回收。
#[tokio::test]
async fn test_unpublished_capture_is_removed_after_last_writer() {
    let mut capture = ShellOutputCapture::new("shell-output-drop");
    let stdout_path = capture.stdout_path().unwrap();
    let stderr_path = capture.stderr_path().unwrap();
    let mut stdout = capture.stdout_writer();
    let stderr = capture.stderr_writer();
    stdout.write_chunk("取消前的输出".as_bytes()).await;
    stdout.finish().await;
    drop(capture);
    assert!(
        std::path::Path::new(&stdout_path).exists(),
        "writer 仍持有文件"
    );
    drop(stdout);
    drop(stderr);
    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while std::path::Path::new(&stdout_path).exists()
            || std::path::Path::new(&stderr_path).exists()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    // 失败时也回收测试产物。
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);
    assert!(cleanup.is_ok(), "最后一个 owner 释放后不应残留未发布文件");
}

#[test]
fn test_unpublished_capture_is_removed_during_runtime_shutdown() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
        let capture = ShellOutputCapture::new("shell-output-runtime-drop");
        tx.send([
            capture.stdout_path().unwrap(),
            capture.stderr_path().unwrap(),
        ])
        .unwrap();
        std::future::pending::<()>().await;
        drop(capture);
    });
    let paths = runtime.block_on(rx).unwrap();
    drop(runtime);
    let remaining: Vec<_> = paths
        .iter()
        .filter(|path| std::path::Path::new(path).exists())
        .collect();
    for path in &remaining {
        let _ = std::fs::remove_file(path);
    }
    assert!(
        remaining.is_empty(),
        "runtime shutdown must reclaim unpublished files"
    );
}

#[test]
fn test_unpublished_capture_is_removed_with_closed_runtime_handle() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = runtime.handle().clone();
    let capture =
        runtime.block_on(async { ShellOutputCapture::new("shell-output-closed-runtime") });
    let paths = [
        capture.stdout_path().unwrap(),
        capture.stderr_path().unwrap(),
    ];
    drop(runtime);
    let _entered = handle.enter();
    drop(capture);
    let remaining: Vec<_> = paths
        .iter()
        .filter(|path| std::path::Path::new(path).exists())
        .collect();
    for path in &remaining {
        let _ = std::fs::remove_file(path);
    }
    assert!(
        remaining.is_empty(),
        "a rejected cleanup task must still own file removal"
    );
}
