//! Download launch → real HTTP/setup → progress → popup interaction regressions.

use super::{DownloadRequest, THEME_LIST, trigger_download_themes_with};
use crate::kit::atoms::{
    DOWNLOAD_PROGRESS, DownloadProgressPayload, FileDownloadStatus, NOTIFICATION, Notification,
    POPUP_KIND, PopupKind,
};
use crate::kit::popup_overlay::close_popup;
use crate::kit::popups::download_progress::handle_download_progress_event;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers},
    prelude::EventResult,
};
use serial_test::serial;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};

const WAIT: Duration = Duration::from_secs(5);

struct AtomsGuard {
    progress: DownloadProgressPayload,
    popup: Option<PopupKind>,
    notification: Option<Notification>,
    catalog: Option<Vec<String>>,
}

impl AtomsGuard {
    fn capture_and_reset() -> Self {
        let guard = Self {
            progress: DOWNLOAD_PROGRESS.state().read().clone(),
            popup: *POPUP_KIND.state().read(),
            notification: NOTIFICATION.state().write().take(),
            catalog: None,
        };
        *DOWNLOAD_PROGRESS.state().write() = DownloadProgressPayload::default();
        *POPUP_KIND.state().write() = None;
        guard
    }

    fn capture_catalog(&mut self) {
        self.catalog = Some(THEME_LIST.state().read().clone());
    }
}

impl Drop for AtomsGuard {
    fn drop(&mut self) {
        // Tests join every worker and HTTP peer before restoring these atoms.
        *DOWNLOAD_PROGRESS.state().write() = self.progress.clone();
        *POPUP_KIND.state().write() = self.popup;
        *NOTIFICATION.state().write() = self.notification.take();
        if let Some(catalog) = self.catalog.take() {
            *THEME_LIST.state().write() = catalog;
        }
    }
}

struct ListingServer {
    url: String,
    seen: oneshot::Receiver<()>,
    release: Option<oneshot::Sender<()>>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Vec<String>>,
}

impl ListingServer {
    async fn start() -> Self {
        Self::with_bodies(vec![Some("[]".to_string()); 4]).await
    }

    async fn with_bodies(bodies: Vec<Option<String>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/contents", listener.local_addr().unwrap());
        let (seen_tx, seen) = oneshot::channel();
        let (release, mut release_rx) = oneshot::channel();
        let (stop, mut stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut bodies = std::collections::VecDeque::from(bodies);
            let mut seen_tx = Some(seen_tx);
            let mut requests = Vec::new();
            loop {
                let accepted = tokio::select! {
                    _ = &mut stop_rx => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut stream, _) = accepted.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut byte = [0u8; 1];
                    if stream.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                if let Some(seen_tx) = seen_tx.take() {
                    let _ = seen_tx.send(());
                    let _ = (&mut release_rx).await;
                }
                if let Some(body) = bodies.pop_front().flatten() {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    // An aborted client may close before the gate is released.
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
            requests
        });
        Self {
            url,
            seen,
            release: Some(release),
            stop,
            task,
        }
    }

    fn request(&self, home: Result<String, std::env::VarError>) -> DownloadRequest {
        DownloadRequest {
            home,
            contents_url: self.url.clone(),
            raw_prefix: format!("{}/raw/", self.url),
            scan_catalog: Box::new(|| panic!("an empty/failed listing must not scan themes")),
        }
    }

    async fn wait_until_requested(&mut self) -> bool {
        matches!(tokio::time::timeout(WAIT, &mut self.seen).await, Ok(Ok(())))
    }

    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }

    async fn finish(mut self) -> Result<Vec<String>, String> {
        self.release();
        let _ = self.stop.send(());
        match tokio::time::timeout(WAIT, &mut self.task).await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => {
                self.task.abort();
                let _ = self.task.await;
                Err("HTTP peer did not stop".into())
            }
        }
    }
}

async fn join_workers(workers: Vec<JoinHandle<()>>) -> Vec<Result<(), String>> {
    let mut outcomes = Vec::new();
    for mut worker in workers {
        outcomes.push(match tokio::time::timeout(WAIT, &mut worker).await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => {
                worker.abort();
                let _ = worker.await;
                Err("download worker did not stop".into())
            }
        });
    }
    outcomes
}

fn home(path: &Path) -> Result<String, std::env::VarError> {
    Ok(path.to_str().unwrap().to_string())
}

fn press_escape() -> EventResult {
    handle_download_progress_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
}

async fn duplicate_during_listing(dismiss: bool) {
    let _atoms = AtomsGuard::capture_and_reset();
    let directory = tempfile::tempdir().unwrap();
    let mut server = ListingServer::start().await;
    let request = server.request(home(directory.path()));
    let first = trigger_download_themes_with(move || request);
    let first_accepted = first.is_some();
    let requested = server.wait_until_requested().await;
    let before = DOWNLOAD_PROGRESS.state().read().clone();
    let popup_opened = *POPUP_KIND.state().read() == Some(PopupKind::Download);
    let local_escape_ignored = matches!(press_escape(), EventResult::Ignored);
    let closed = if dismiss {
        // The root Esc handler delegates to this actual close entry point even
        // when the child popup ignores Esc. Closing must not cancel the worker.
        close_popup()
    } else {
        None
    };
    let request = server.request(home(directory.path()));
    let second = trigger_download_themes_with(move || request);
    let duplicate_accepted = second.is_some();
    server.release();
    let outcomes = join_workers(first.into_iter().chain(second).collect()).await;
    let requests = server.finish().await;
    let completed = DOWNLOAD_PROGRESS.state().read().clone();

    // Every asynchronous writer has stopped before an assertion can unwind.
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    let requests = requests.unwrap();
    assert!(first_accepted && requested && popup_opened);
    assert!(before.items.is_empty() && !before.finished);
    assert!(local_escape_ignored);
    if dismiss {
        assert_eq!(closed, Some(PopupKind::Download));
    }
    assert!(
        completed.finished,
        "in-flight work must finish after dismissal"
    );
    assert!(
        !duplicate_accepted,
        "listing is already in flight; an empty/reset display is not an idle task"
    );
    assert_eq!(requests.len(), 1, "a duplicate download reached HTTP");
}

#[tokio::test]
#[serial]
async fn test_theme_download_rejects_duplicate_while_listing() {
    duplicate_during_listing(false).await;
}

#[tokio::test]
#[serial]
async fn test_theme_download_dismiss_does_not_release_active_owner() {
    duplicate_during_listing(true).await;
}

async fn setup_error_reaches_finished(missing_home: bool) {
    let _atoms = AtomsGuard::capture_and_reset();
    let directory = tempfile::tempdir().unwrap();
    if !missing_home {
        std::fs::write(directory.path().join(".peri"), "not a directory").unwrap();
    }
    let server = ListingServer::start().await;
    let request = server.request(if missing_home {
        Err(std::env::VarError::NotPresent)
    } else {
        home(directory.path())
    });
    let worker = trigger_download_themes_with(move || request);
    let accepted = worker.is_some();
    let outcomes = join_workers(worker.into_iter().collect()).await;
    let requests = server.finish().await;
    let completed = DOWNLOAD_PROGRESS.state().read().clone();
    let escape = press_escape();
    let popup_closed = POPUP_KIND.state().read().is_none();

    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    assert!(
        requests.unwrap().is_empty(),
        "setup failure must not send HTTP"
    );
    assert!(accepted);
    assert!(completed.finished, "setup error left progress unfinished");
    assert_eq!(completed.fail_count, 1);
    assert!(matches!(escape, EventResult::Consumed));
    assert!(popup_closed);
}

#[tokio::test]
#[serial]
async fn test_theme_download_missing_home_reaches_finished() {
    setup_error_reaches_finished(true).await;
}

#[tokio::test]
#[serial]
async fn test_theme_download_directory_error_reaches_finished() {
    setup_error_reaches_finished(false).await;
}

#[tokio::test]
#[serial]
async fn test_theme_download_empty_listing_finishes_and_releases_owner() {
    let _atoms = AtomsGuard::capture_and_reset();
    let directory = tempfile::tempdir().unwrap();
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut server = ListingServer::start().await;
        let request = server.request(home(directory.path()));
        let worker = trigger_download_themes_with(move || request);
        let accepted = worker.is_some();
        let requested = server.wait_until_requested().await;
        server.release();
        let outcomes = join_workers(worker.into_iter().collect()).await;
        let requests = server.finish().await;
        let completed = DOWNLOAD_PROGRESS.state().read().clone();
        let escape = press_escape();
        let popup_closed = POPUP_KIND.state().read().is_none();
        runs.push((
            accepted,
            requested,
            outcomes,
            requests,
            completed,
            escape,
            popup_closed,
        ));
    }
    for (accepted, requested, outcomes, requests, completed, escape, popup_closed) in runs {
        assert!(accepted && requested);
        assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
        let requests = requests.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /contents HTTP/1.1\r\n"));
        assert!(completed.finished && completed.items.is_empty());
        assert_eq!((completed.success_count, completed.fail_count), (0, 0));
        assert!(matches!(escape, EventResult::Consumed));
        assert!(popup_closed);
    }
}

#[tokio::test]
#[serial]
async fn test_theme_download_success_writes_original_body_and_refreshes_catalog() {
    let mut atoms = AtomsGuard::capture_and_reset();
    atoms.capture_catalog();
    *THEME_LIST.state().write() = vec!["stale-theme".to_string()];
    let directory = tempfile::tempdir().unwrap();
    let theme_directory = directory.path().join(".peri/themes");
    let body = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peri-theme/themes/dark.json"
    ));
    let mut server = ListingServer::with_bodies(vec![
        Some(r#"[{"name":"downloaded-theme.json"},{"name":"README.md"}]"#.to_string()),
        Some(body.to_string()),
    ])
    .await;
    let mut request = server.request(home(directory.path()));
    let scan_directory = theme_directory.clone();
    request.scan_catalog = Box::new(move || {
        std::fs::read_dir(scan_directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| path.file_stem().unwrap().to_string_lossy().to_string())
            .collect()
    });
    let worker = trigger_download_themes_with(move || request);
    let accepted = worker.is_some();
    let requested = server.wait_until_requested().await;
    server.release();
    let outcomes = join_workers(worker.into_iter().collect()).await;
    let requests = server.finish().await;
    let completed = DOWNLOAD_PROGRESS.state().read().clone();
    let catalog = THEME_LIST.state().read().clone();
    let stored = std::fs::read_to_string(theme_directory.join("downloaded-theme.json"));
    let notification_published = NOTIFICATION.state().read().is_some();
    let escape = press_escape();
    let retry_succeeded = complete_empty_retry(directory.path()).await;

    assert!(accepted && requested);
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    let requests = requests.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /contents HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("GET /contents/raw/downloaded-theme.json HTTP/1.1\r\n"));
    assert!(requests.iter().all(|request| {
        request
            .to_ascii_lowercase()
            .contains("user-agent: peri-tui/0.1\r\n")
    }));
    assert_eq!(stored.unwrap(), body);
    assert!(!theme_directory.join("README.md").exists());
    assert_eq!(catalog, vec!["downloaded-theme"]);
    assert!(completed.finished);
    assert_eq!((completed.success_count, completed.fail_count), (1, 0));
    assert_eq!(completed.items.len(), 1);
    assert!(matches!(
        completed.items[0].status,
        FileDownloadStatus::Done
    ));
    assert!(notification_published);
    assert!(matches!(escape, EventResult::Consumed));
    assert!(retry_succeeded);
}

#[tokio::test]
#[serial]
async fn test_theme_download_transport_failure_finishes_and_releases_owner() {
    let _atoms = AtomsGuard::capture_and_reset();
    let directory = tempfile::tempdir().unwrap();
    // No HTTP response: fail the real reqwest send rather than injecting an
    // outcome after the transport boundary. Any retry also gets a closed peer.
    let mut server = ListingServer::with_bodies(Vec::new()).await;
    let request = server.request(home(directory.path()));
    let worker = trigger_download_themes_with(move || request);
    let accepted = worker.is_some();
    let requested = server.wait_until_requested().await;
    server.release();
    let outcomes = join_workers(worker.into_iter().collect()).await;
    let requests = server.finish().await;
    let completed = DOWNLOAD_PROGRESS.state().read().clone();
    let escape = press_escape();
    let retry_succeeded = complete_empty_retry(directory.path()).await;

    assert!(accepted && requested);
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    assert!(!requests.unwrap().is_empty());
    assert!(completed.finished && completed.items.is_empty());
    assert_eq!((completed.success_count, completed.fail_count), (0, 1));
    assert!(matches!(escape, EventResult::Consumed));
    assert!(retry_succeeded);
}

async fn abort_and_join(worker: Option<JoinHandle<()>>) -> bool {
    let Some(mut worker) = worker else {
        return false;
    };
    worker.abort();
    match tokio::time::timeout(WAIT, &mut worker).await {
        Ok(Err(error)) => error.is_cancelled(),
        Ok(Ok(())) => false,
        Err(_) => {
            worker.abort();
            let _ = worker.await;
            false
        }
    }
}

async fn complete_empty_retry(directory: &Path) -> bool {
    let mut server = ListingServer::start().await;
    let request = server.request(home(directory));
    let worker = trigger_download_themes_with(move || request);
    let accepted = worker.is_some();
    let requested = server.wait_until_requested().await;
    server.release();
    let outcomes = join_workers(worker.into_iter().collect()).await;
    let requests = server.finish().await;
    let completed = DOWNLOAD_PROGRESS.state().read().clone();
    accepted
        && requested
        && outcomes.iter().all(Result::is_ok)
        && requests.is_ok_and(|requests| requests.len() == 1)
        && completed.finished
        && completed.success_count == 0
        && completed.fail_count == 0
}

#[tokio::test]
#[serial]
async fn test_theme_download_abort_before_first_poll_releases_owner() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let _atoms = AtomsGuard::capture_and_reset();
    let directory = tempfile::tempdir().unwrap();
    let server = ListingServer::start().await;
    let request = server.request(home(directory.path()));
    let polled = Arc::new(AtomicBool::new(false));
    let polled_in_task = polled.clone();
    let worker = trigger_download_themes_with(move || {
        polled_in_task.store(true, Ordering::Relaxed);
        request
    });
    // Current-thread runtime: abort occurs before the first yield, so the
    // request factory has not run and the future itself must already own lease.
    let cancelled = abort_and_join(worker).await;
    let requests = server.finish().await;
    let aborted = DOWNLOAD_PROGRESS.state().read().clone();
    let retry_succeeded = complete_empty_retry(directory.path()).await;

    assert!(cancelled);
    assert!(!polled.load(Ordering::Relaxed));
    assert!(requests.unwrap().is_empty());
    assert!(aborted.finished);
    assert_eq!(aborted.fail_count, 1);
    assert!(retry_succeeded);
}

#[tokio::test]
#[serial]
async fn test_theme_download_abort_in_flight_releases_owner() {
    let _atoms = AtomsGuard::capture_and_reset();
    let directory = tempfile::tempdir().unwrap();
    let mut server = ListingServer::start().await;
    let request = server.request(home(directory.path()));
    let worker = trigger_download_themes_with(move || request);
    let requested = server.wait_until_requested().await;
    let cancelled = abort_and_join(worker).await;
    let requests = server.finish().await;
    let aborted = DOWNLOAD_PROGRESS.state().read().clone();
    let retry_succeeded = complete_empty_retry(directory.path()).await;

    assert!(requested && cancelled);
    assert_eq!(requests.unwrap().len(), 1);
    assert!(aborted.finished);
    assert_eq!(aborted.fail_count, 1);
    assert!(retry_succeeded);
}
