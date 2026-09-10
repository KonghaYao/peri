use super::*;

#[cfg(unix)]
#[tokio::test]
async fn test_install_cancel_reaps_process_before_returning() {
    let fixture = tempfile::tempdir().unwrap();
    let ready = fixture.path().join("ready");
    let mut command = Command::new("node");
    command.arg("-e").arg(format!(
        "require('fs').writeFileSync({},String(process.pid)); setInterval(()=>{{}},1000);",
        serde_json::to_string(&ready).unwrap()
    ));
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { run_install(command, &cancel).await }
    });
    let pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(&ready).await {
                if let Ok(pid) = value.parse() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("安装进程应已启动并写出pid");
    assert!(pid > 0);
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    // SAFETY: signal 0 only checks existence of the fixture's recorded positive pid.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "返回时安装进程必须已回收"
    );
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}
