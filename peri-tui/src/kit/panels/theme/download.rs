//! One admitted theme download owns progress until all terminal effects finish.
//! The popup's resettable display is a projection, never task admission state.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use fluent_bundle::FluentValue;
use peri_theme::loader::list_available_themes;
use tokio::task::JoinHandle;

use super::THEME_LIST;
use crate::i18n;

static DOWNLOAD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// A stalled peer must not hold the single download lease forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Both the catalog listing and each theme file are small; the budget keeps an
/// endless or oversized response from holding the lease or filling the disk.
const MAX_BODY_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct DownloadLimits {
    pub(super) request_timeout: Duration,
    pub(super) max_body_bytes: u64,
}

impl Default for DownloadLimits {
    fn default() -> Self {
        Self {
            request_timeout: REQUEST_TIMEOUT,
            max_body_bytes: MAX_BODY_BYTES,
        }
    }
}

pub(super) struct DownloadRequest {
    pub(super) home: Result<String, std::env::VarError>,
    pub(super) contents_url: String,
    pub(super) raw_prefix: String,
    pub(super) scan_catalog: Box<dyn FnOnce() -> Vec<String> + Send>,
    pub(super) limits: DownloadLimits,
}

impl DownloadRequest {
    fn from_environment() -> Self {
        Self {
            home: std::env::var("HOME"),
            contents_url:
                "https://api.github.com/repos/konghayao/perihelion/contents/.peri/theme?ref=main"
                    .to_string(),
            raw_prefix: "https://raw.githubusercontent.com/konghayao/perihelion/main/.peri/theme/"
                .to_string(),
            scan_catalog: Box::new(list_available_themes),
            limits: DownloadLimits::default(),
        }
    }
}

pub(super) fn trigger_download_themes() {
    let _ = trigger_download_themes_with(DownloadRequest::from_environment);
}

pub(super) fn trigger_download_themes_with(
    request: impl FnOnce() -> DownloadRequest + Send + 'static,
) -> Option<JoinHandle<()>> {
    let mut run = DownloadRun::try_claim()?;
    crate::kit::popup_overlay::open_popup(PopupKind::Download);
    Some(tokio::spawn(async move {
        let result = run_download(request(), &mut run).await;
        run.finish(result);
    }))
}

/// Non-Clone lease plus the worker's canonical progress. Dropping the task,
/// even before its first poll, publishes a terminal failure before admission
/// is released. Normal completion marks the lease settled and Drop only unlocks.
struct DownloadRun {
    progress: DownloadProgressPayload,
    settled: bool,
}

impl DownloadRun {
    fn try_claim() -> Option<Self> {
        DOWNLOAD_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        let run = Self {
            progress: DownloadProgressPayload::default(),
            settled: false,
        };
        run.publish();
        Some(run)
    }

    fn publish(&self) {
        *DOWNLOAD_PROGRESS.state().write() = self.progress.clone();
    }

    fn list(&mut self, filenames: Vec<String>) {
        self.progress.items = filenames
            .into_iter()
            .map(|filename| DownloadItem {
                filename,
                status: FileDownloadStatus::Pending,
            })
            .collect();
        self.publish();
    }

    fn downloading(&mut self, index: usize) {
        self.progress.items[index].status = FileDownloadStatus::Downloading;
        self.publish();
    }

    fn record(&mut self, index: usize, result: Result<(), String>) {
        self.progress.items[index].status = match result {
            Ok(()) => {
                self.progress.success_count += 1;
                FileDownloadStatus::Done
            }
            Err(error) => {
                self.progress.fail_count += 1;
                FileDownloadStatus::Failed(error)
            }
        };
        self.publish();
    }

    fn finish(&mut self, result: anyhow::Result<()>) {
        if let Err(error) = result {
            tracing::error!("download themes: {error:#}");
            let previous_failures = self.progress.fail_count;
            for item in &mut self.progress.items {
                if matches!(
                    item.status,
                    FileDownloadStatus::Pending | FileDownloadStatus::Downloading
                ) {
                    item.status = FileDownloadStatus::Failed(error.to_string());
                    self.progress.fail_count += 1;
                }
            }
            if self.progress.fail_count == previous_failures {
                self.progress.fail_count += 1;
            }
        }
        self.progress.finished = true;
        self.publish();
        *NOTIFICATION.state().write() = Some(Notification {
            message: i18n::tr_args(
                "popup-download-finished-notify",
                &[
                    (
                        "total".to_string(),
                        FluentValue::from(self.progress.items.len() as i64),
                    ),
                    (
                        "success".to_string(),
                        FluentValue::from(self.progress.success_count as i64),
                    ),
                    (
                        "failed".to_string(),
                        FluentValue::from(self.progress.fail_count as i64),
                    ),
                ],
            )
            .to_string(),
            until: peri_time::monotonic_now() + Duration::from_secs(3),
        });
        self.settled = true;
    }
}

impl Drop for DownloadRun {
    fn drop(&mut self) {
        if !self.settled {
            self.finish(Err(anyhow::anyhow!(
                "download task stopped before completion"
            )));
        }
        // There are no progress/catalog/notification writes after this release.
        DOWNLOAD_ACTIVE.store(false, Ordering::Release);
    }
}

async fn run_download(request: DownloadRequest, run: &mut DownloadRun) -> anyhow::Result<()> {
    let home = request.home.context("HOME not set")?;
    let directory = Path::new(&home).join(".peri").join("themes");
    std::fs::create_dir_all(&directory).context("failed to create theme directory")?;
    let client = reqwest::Client::builder()
        .user_agent("peri-tui/0.1")
        .timeout(request.limits.request_timeout)
        .build()
        .context("failed to create HTTP client")?;
    let response = client
        .get(&request.contents_url)
        .send()
        .await
        .context("GitHub API request failed")?;
    let body = read_body(response, request.limits.max_body_bytes, "GitHub API")
        .await
        .map_err(anyhow::Error::msg)?;
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&body).context("failed to parse GitHub API response")?;
    run.list(
        entries
            .iter()
            .filter_map(|entry| entry.get("name")?.as_str())
            .filter(|name| name.ends_with(".json"))
            .map(str::to_string)
            .collect(),
    );

    for index in 0..run.progress.items.len() {
        let filename = run.progress.items[index].filename.clone();
        run.downloading(index);
        let raw_url = format!("{}{filename}", request.raw_prefix);
        let result = download_file(
            &client,
            &raw_url,
            &directory.join(&filename),
            request.limits.max_body_bytes,
        )
        .await;
        match &result {
            Ok(()) => tracing::info!(
                "download themes: downloaded {} ({}/{})",
                filename,
                index + 1,
                run.progress.items.len()
            ),
            Err(error) => tracing::warn!("download themes: failed to download {filename}: {error}"),
        }
        run.record(index, result);
    }
    if run.progress.success_count > 0 {
        let theme_catalog = THEME_LIST.state();
        let mut catalog = theme_catalog.write();
        super::refresh_theme_catalog_after_download(
            &mut catalog,
            run.progress.success_count,
            request.scan_catalog,
        );
    }
    Ok(())
}

/// Status handling and the write target stay unchanged; the bounded read keeps a
/// stalled or oversized peer from holding the lease or filling the disk.
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    max_body_bytes: u64,
) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("HTTP error: {error}"))?;
    let body = read_body(response, max_body_bytes, "theme").await?;
    std::fs::write(path, body).map_err(|error| format!("write error: {error}"))
}

/// `Content-Length` is only a fast-fail hint: it may be absent or untrusted, so
/// the streamed total is authoritative.
async fn read_body(
    mut response: reqwest::Response,
    max_bytes: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if let Some(length) = response.content_length()
        && length > max_bytes
    {
        return Err(format!(
            "{label} response exceeds {max_bytes} byte limit (Content-Length: {length})"
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("{label} read error: {error}"))?
    {
        if body.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(format!("{label} response exceeds {max_bytes} byte limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

use crate::kit::atoms::DOWNLOAD_PROGRESS;

use crate::kit::atoms::DownloadItem;

use crate::kit::atoms::DownloadProgressPayload;

use crate::kit::atoms::FileDownloadStatus;

use crate::kit::atoms::NOTIFICATION;

use crate::kit::atoms::Notification;

use crate::kit::atoms::PopupKind;
