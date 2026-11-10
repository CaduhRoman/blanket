//! Download Manager integration.
//!
//! Servo 0.8 has no download API: a navigation to a non-renderable resource is simply
//! not displayed. Blanket therefore starts downloads itself, from three sources:
//!
//! * main-frame navigations to URLs whose path names a typical download file type,
//!   intercepted in [`super::delegate`] (the page stays where it was);
//! * the context menu ("Download link", "Save image");
//! * the browser menu ("Save page").
//!
//! The transfer runs in `blanket-net`, carrying the engine's cookies for the URL so
//! that downloads behind a login keep working.

use std::path::Path;

use blanket_core::downloads::{DownloadItem, DownloadState, suggested_file_name};
use blanket_core::ids::DownloadId;
use blanket_core::paths::default_download_dir;
use blanket_core::time::unix_now;
use blanket_net::{DownloadEvent, DownloadRequest};
use servo::{CookieSource, UserAgentPlatform};
use tracing::{info, warn};
use url::Url;

use super::Browser;
use super::maintenance::Task;
use crate::ui::dialogs::Dialog;

/// File types that Servo cannot display and that users expect to be saved.
const DOWNLOAD_EXTENSIONS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "exe", "msi", "msix", "appx", "dmg", "pkg", "deb",
    "rpm", "apk", "appimage", "iso", "img", "bin", "torrent", "jar", "whl", "nupkg", "pdf", "doc", "docx", "xls",
    "xlsx", "ppt", "pptx", "odt", "ods", "odp", "epub",
];

/// Media types are only playable when Blanket is built with GStreamer support.
#[cfg(not(feature = "media"))]
const MEDIA_EXTENSIONS: &[&str] = &["mp4", "m4v", "mkv", "mov", "avi", "wmv", "mp3", "m4a", "flac", "wav", "aac"];
#[cfg(feature = "media")]
const MEDIA_EXTENSIONS: &[&str] = &[];

pub fn looks_like_download(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(extension) = Path::new(url.path()).extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    DOWNLOAD_EXTENSIONS.contains(&extension.as_str()) || MEDIA_EXTENSIONS.contains(&extension.as_str())
}

impl Browser {
    pub(super) fn start_download(&mut self, url: Url, referrer: Option<Url>) {
        if !matches!(url.scheme(), "http" | "https") {
            self.gui_mut().toast(format!("Cannot download {} links", url.scheme()));
            return;
        }
        let cookie_header = self.servo().map(|servo| {
            servo
                .site_data_manager()
                .cookies_for_url(url.clone(), CookieSource::HTTP)
                .iter()
                .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
                .collect::<Vec<_>>()
                .join("; ")
        });
        let id = DownloadId::next();
        let file_name = suggested_file_name(&url, None);
        info!(%id, %url, "starting download");
        self.downloads.add(DownloadItem {
            id,
            url: url.clone(),
            file_name: file_name.clone(),
            destination: Default::default(),
            state: DownloadState::InProgress { received: 0, total: None },
            started: unix_now(),
        });
        self.net.start_download(DownloadRequest {
            id,
            url,
            directory: default_download_dir(),
            cookie_header: cookie_header.filter(|header| !header.is_empty()),
            referrer,
            user_agent: UserAgentPlatform::default().to_user_agent_string(),
            security: self.config.security.clone(),
        });
        self.gui_mut().toast(format!("Downloading {file_name}"));
        self.request_redraw();
    }

    pub(super) fn process_download_events(&mut self) {
        let events: Vec<DownloadEvent> = self.net.poll_events().collect();
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                DownloadEvent::AwaitingConfirmation { id, file_name, risk } => {
                    self.downloads.set_target(id, file_name.clone(), Default::default());
                    self.downloads.update(id, DownloadState::AwaitingConfirmation);
                    let source = self.downloads.get(id).map(|item| item.url.clone());
                    if let Some(source) = source {
                        self.gui_mut().dialogs.push_global(Dialog::download_confirmation(id, file_name, source, risk));
                    }
                },
                DownloadEvent::Started { id, file_name, destination, total } => {
                    self.downloads.set_target(id, file_name, destination);
                    self.downloads.update(id, DownloadState::InProgress { received: 0, total });
                },
                DownloadEvent::Progress { id, received, total } => {
                    self.downloads.update(id, DownloadState::InProgress { received, total });
                },
                DownloadEvent::Completed { id, bytes } => {
                    self.downloads.update(id, DownloadState::Completed { bytes });
                    if let Some(item) = self.downloads.get(id) {
                        let message = format!("Downloaded {}", item.file_name);
                        self.gui_mut().toast(message);
                    }
                    self.schedule_soon(Task::SaveDownloads);
                },
                DownloadEvent::Failed { id, error } => {
                    warn!(%id, %error, "download failed");
                    self.downloads.update(id, DownloadState::Failed { error: error.clone() });
                    self.gui_mut().toast(format!("Download failed: {error}"));
                    self.schedule_soon(Task::SaveDownloads);
                },
                DownloadEvent::Cancelled { id } => {
                    self.downloads.update(id, DownloadState::Cancelled);
                    self.schedule_soon(Task::SaveDownloads);
                },
            }
        }
        self.request_redraw();
    }

    /// Open Explorer with the downloaded file selected. Files are never executed by the
    /// browser itself.
    pub(super) fn show_download_in_folder(&mut self, id: DownloadId) {
        let Some(item) = self.downloads.get(id) else {
            return;
        };
        if item.destination.as_os_str().is_empty() {
            return;
        }
        reveal_in_file_manager(&item.destination);
    }
}

#[cfg(windows)]
fn reveal_in_file_manager(path: &Path) {
    let mut argument = std::ffi::OsString::from("/select,");
    argument.push(path.as_os_str());
    if let Err(error) = std::process::Command::new("explorer.exe").arg(argument).spawn() {
        warn!(%error, "could not open Explorer");
    }
}

#[cfg(not(windows))]
fn reveal_in_file_manager(path: &Path) {
    let Some(directory) = path.parent() else {
        return;
    };
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(error) = std::process::Command::new(opener).arg(directory).spawn() {
        warn!(%error, "could not open the file manager");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_detection_uses_the_path_extension() {
        assert!(looks_like_download(&Url::parse("https://a.com/files/setup.EXE").unwrap()));
        assert!(looks_like_download(&Url::parse("https://a.com/r/report.pdf?x=1").unwrap()));
        assert!(!looks_like_download(&Url::parse("https://a.com/page.html").unwrap()));
        assert!(!looks_like_download(&Url::parse("https://a.com/zip").unwrap()));
        assert!(!looks_like_download(&Url::parse("file:///C:/a.zip").unwrap()));
    }
}
