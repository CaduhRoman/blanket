use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use blanket_core::downloads::{suggested_file_name, unique_destination};
use blanket_core::security::assess_download;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_DISPOSITION, COOKIE, REFERER, USER_AGENT};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{oneshot, watch};
use tracing::{info, warn};

use crate::{DownloadEvent, DownloadRequest, Shared};

/// Minimum interval between progress events, so a fast download does not flood the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

/// Suffix of the file being written; it is renamed once the transfer completes.
const PARTIAL_SUFFIX: &str = ".blanketpart";

/// Run one download to completion and return its final event.
pub(crate) async fn run(
    client: &reqwest::Client,
    request: DownloadRequest,
    mut cancel: watch::Receiver<bool>,
    confirm: oneshot::Receiver<bool>,
    shared: &Shared,
) -> DownloadEvent {
    let id = request.id;
    let mut partial_path = None;
    let result = tokio::select! {
        result = transfer(client, request, confirm, shared, &mut partial_path) => result,
        _ = cancel.wait_for(|cancelled| *cancelled) => Err(Outcome::Cancelled),
    };
    match result {
        Ok(bytes) => DownloadEvent::Completed { id, bytes },
        Err(outcome) => {
            if let Some(path) = partial_path {
                if let Err(error) = fs::remove_file(&path).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        warn!(path = %path.display(), %error, "could not remove partial download");
                    }
                }
            }
            match outcome {
                Outcome::Cancelled => DownloadEvent::Cancelled { id },
                Outcome::Failed(error) => DownloadEvent::Failed { id, error },
            }
        },
    }
}

enum Outcome {
    Cancelled,
    Failed(String),
}

impl From<reqwest::Error> for Outcome {
    fn from(error: reqwest::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<std::io::Error> for Outcome {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

async fn transfer(
    client: &reqwest::Client,
    request: DownloadRequest,
    confirm: oneshot::Receiver<bool>,
    shared: &Shared,
    partial_path: &mut Option<PathBuf>,
) -> Result<u64, Outcome> {
    let id = request.id;
    let mut builder = client.get(request.url.clone()).header(USER_AGENT, &request.user_agent);
    if let Some(cookie) = &request.cookie_header {
        builder = builder.header(COOKIE, cookie);
    }
    if let Some(referrer) = &request.referrer {
        // Never leak a secure referrer to an insecure destination.
        if !(referrer.scheme() == "https" && request.url.scheme() == "http") {
            builder = builder.header(REFERER, referrer.as_str());
        }
    }
    let response = builder.send().await?.error_for_status()?;

    let final_url = response.url().clone();
    let content_disposition = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok());
    let file_name = suggested_file_name(&final_url, content_disposition);
    let risk = assess_download(&final_url, &file_name);
    let total = response.content_length();

    if risk.needs_confirmation(&request.security) {
        shared.emit(DownloadEvent::AwaitingConfirmation {
            id,
            file_name: file_name.clone(),
            risk: risk.clone(),
        });
        // A dropped sender (download list closed) counts as "no".
        if !confirm.await.unwrap_or(false) {
            return Err(Outcome::Cancelled);
        }
    }

    fs::create_dir_all(&request.directory).await?;
    let (destination, partial, file) = reserve_destination(&request.directory, &file_name).await?;
    *partial_path = Some(partial.clone());
    info!(%id, destination = %destination.display(), "download started");
    shared.emit(DownloadEvent::Started {
        id,
        file_name,
        destination: destination.clone(),
        total,
    });

    let mut writer = BufWriter::with_capacity(256 * 1024, file);
    let mut stream = response.bytes_stream();
    let mut received = 0u64;
    let mut last_progress = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        writer.write_all(&chunk).await?;
        received += chunk.len() as u64;
        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            last_progress = Instant::now();
            shared.emit(DownloadEvent::Progress { id, received, total });
        }
    }
    writer.flush().await?;
    writer.get_mut().sync_all().await?;
    drop(writer);

    if let Some(expected) = total {
        if expected != received {
            return Err(Outcome::Failed(format!("connection closed after {received} of {expected} bytes")));
        }
    }

    let destination = if fs::try_exists(&destination).await.unwrap_or(false) {
        unique_destination(&request.directory, &file_name_of(&destination), |path| path.exists())
    } else {
        destination
    };
    fs::rename(&partial, &destination).await?;
    *partial_path = None;
    Ok(received)
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into())
}

/// Choose a destination that does not exist yet and exclusively create its partial file,
/// so two simultaneous downloads of the same name cannot write to the same file.
async fn reserve_destination(directory: &Path, file_name: &str) -> Result<(PathBuf, PathBuf, File), Outcome> {
    for _ in 0..100 {
        let destination = unique_destination(directory, file_name, |path| {
            path.exists() || partial_for(path).exists()
        });
        let partial = partial_for(&destination);
        match OpenOptions::new().write(true).create_new(true).open(&partial).await {
            Ok(file) => return Ok((destination, partial, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Outcome::Failed("could not find a free file name".into()))
}

fn partial_for(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(PARTIAL_SUFFIX);
    PathBuf::from(name)
}
