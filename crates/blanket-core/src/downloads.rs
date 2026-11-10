//! Download Manager model: download records, file naming and state transitions.
//! The transfer itself is performed by `blanket-net`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CoreResult;
use crate::ids::DownloadId;
use crate::security::sanitize_file_name;
use crate::storage::{load_json_or_default, save_json_atomic};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DownloadState {
    /// Waiting for the user to confirm a risky download.
    AwaitingConfirmation,
    InProgress { received: u64, total: Option<u64> },
    Completed { bytes: u64 },
    Failed { error: String },
    Cancelled,
}

impl DownloadState {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled)
    }

    /// Progress in `0.0..=1.0` when the total size is known.
    pub fn progress(&self) -> Option<f32> {
        match self {
            Self::InProgress {
                received,
                total: Some(total),
            } if *total > 0 => Some((*received as f64 / *total as f64).min(1.0) as f32),
            Self::Completed { .. } => Some(1.0),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id: DownloadId,
    pub url: Url,
    pub file_name: String,
    pub destination: PathBuf,
    pub state: DownloadState,
    pub started: u64,
}

/// Download records, newest first.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DownloadList {
    items: Vec<DownloadItem>,
}

/// Finished records kept in the persisted download history.
const PERSISTED_DOWNLOADS_LIMIT: usize = 200;

impl DownloadList {
    /// Load the download history. Downloads that were running when the browser last
    /// exited are marked as interrupted, and new ids are allocated above loaded ones.
    pub fn load(path: &Path) -> CoreResult<Self> {
        let mut list: Self = load_json_or_default(path)?;
        list.mark_interrupted();
        if let Some(max) = list.items.iter().map(|item| item.id).max() {
            DownloadId::reserve_through(max);
        }
        Ok(list)
    }

    pub fn save(&self, path: &Path) -> CoreResult<()> {
        let kept = Self {
            items: self.items.iter().take(PERSISTED_DOWNLOADS_LIMIT).cloned().collect(),
        };
        save_json_atomic(path, &kept)
    }

    pub fn items(&self) -> &[DownloadItem] {
        &self.items
    }

    /// Record where the transfer is actually being written once the server's response
    /// (and its `Content-Disposition`) is known.
    pub fn set_target(&mut self, id: DownloadId, file_name: String, destination: PathBuf) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.file_name = file_name;
                item.destination = destination;
                true
            },
            None => false,
        }
    }

    /// Forget one finished record (the file itself is kept).
    pub fn remove(&mut self, id: DownloadId) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id || !item.state.is_finished());
        self.items.len() != before
    }

    pub fn get(&self, id: DownloadId) -> Option<&DownloadItem> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn add(&mut self, item: DownloadItem) {
        self.items.insert(0, item);
    }

    pub fn active_count(&self) -> usize {
        self.items.iter().filter(|item| !item.state.is_finished()).count()
    }

    /// Apply a state transition. Finished downloads never change state again, so a late
    /// progress message cannot resurrect a cancelled download.
    pub fn update(&mut self, id: DownloadId, state: DownloadState) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) if !item.state.is_finished() => {
                item.state = state;
                true
            },
            _ => false,
        }
    }

    /// Remove finished downloads from the list (files are kept).
    pub fn clear_finished(&mut self) {
        self.items.retain(|item| !item.state.is_finished());
    }

    /// Downloads interrupted by a previous exit are marked failed on load.
    pub fn mark_interrupted(&mut self) {
        for item in &mut self.items {
            if !item.state.is_finished() {
                item.state = DownloadState::Failed {
                    error: "interrupted when the browser closed".into(),
                };
            }
        }
    }
}

/// Extract the file name from a `Content-Disposition` header value (RFC 6266),
/// preferring the RFC 5987 `filename*` form.
pub fn file_name_from_content_disposition(header: &str) -> Option<String> {
    let mut plain = None;
    for part in header.split(';').map(str::trim) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "filename*" {
            // charset'language'percent-encoded
            let mut pieces = value.splitn(3, '\'');
            let charset = pieces.next().unwrap_or_default();
            let _language = pieces.next();
            if let Some(encoded) = pieces.next() {
                if charset.eq_ignore_ascii_case("utf-8") {
                    let decoded: Vec<u8> = percent_decode(encoded);
                    if let Ok(text) = String::from_utf8(decoded) {
                        return Some(text);
                    }
                }
            }
        } else if key == "filename" {
            plain = Some(value.trim_matches('"').to_owned());
        }
    }
    plain.filter(|name| !name.is_empty())
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                output.push(high << 4 | low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    output
}

fn hex_value(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

/// Derive a safe file name from response metadata and the URL.
pub fn suggested_file_name(url: &Url, content_disposition: Option<&str>) -> String {
    let raw = content_disposition
        .and_then(file_name_from_content_disposition)
        .or_else(|| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(str::to_owned))
                .filter(|segment| !segment.is_empty())
                .map(|segment| String::from_utf8_lossy(&percent_decode(&segment)).into_owned())
        })
        .or_else(|| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    sanitize_file_name(&raw)
}

/// Pick a path in `directory` that does not exist yet: `name.ext`, `name (1).ext`, ...
pub fn unique_destination(directory: &Path, file_name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let candidate = directory.join(file_name);
    if !exists(&candidate) {
        return candidate;
    }
    let path = Path::new(file_name);
    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or(file_name);
    let extension = path.extension().and_then(|extension| extension.to_str());
    for counter in 1..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({counter}).{extension}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = directory.join(name);
        if !exists(&candidate) {
            return candidate;
        }
    }
    directory.join(format!("{stem}-{}", crate::time::unix_now()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn content_disposition_variants() {
        assert_eq!(
            file_name_from_content_disposition("attachment; filename=\"report.pdf\""),
            Some("report.pdf".into())
        );
        assert_eq!(
            file_name_from_content_disposition("attachment; filename=plain.txt"),
            Some("plain.txt".into())
        );
        assert_eq!(
            file_name_from_content_disposition("attachment; filename=\"fallback.txt\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"),
            Some("résumé.pdf".into())
        );
        assert_eq!(file_name_from_content_disposition("inline"), None);
    }

    #[test]
    fn suggested_names_are_safe() {
        assert_eq!(suggested_file_name(&url("https://a.com/files/My%20Doc.pdf"), None), "My Doc.pdf");
        assert_eq!(suggested_file_name(&url("https://a.com/"), None), "a.com");
        assert_eq!(
            suggested_file_name(&url("https://a.com/x"), Some("attachment; filename=\"..\\..\\evil.exe\"")),
            "evil.exe"
        );
    }

    #[test]
    fn destinations_never_overwrite() {
        let existing: HashSet<PathBuf> = [PathBuf::from("d/a.zip"), PathBuf::from("d/a (1).zip")].into_iter().collect();
        let path = unique_destination(Path::new("d"), "a.zip", |path| existing.contains(path));
        assert_eq!(path, PathBuf::from("d/a (2).zip"));
        let path = unique_destination(Path::new("d"), "b", |path| existing.contains(path));
        assert_eq!(path, PathBuf::from("d/b"));
    }

    #[test]
    fn finished_downloads_are_final() {
        let mut list = DownloadList::default();
        let id = DownloadId(1);
        list.add(DownloadItem {
            id,
            url: url("https://a.com/f"),
            file_name: "f".into(),
            destination: PathBuf::from("f"),
            state: DownloadState::InProgress { received: 0, total: Some(10) },
            started: 0,
        });
        assert!(list.update(id, DownloadState::InProgress { received: 5, total: Some(10) }));
        assert_eq!(list.get(id).unwrap().state.progress(), Some(0.5));
        assert!(list.update(id, DownloadState::Cancelled));
        assert!(!list.update(id, DownloadState::InProgress { received: 6, total: Some(10) }));
        assert_eq!(list.get(id).unwrap().state, DownloadState::Cancelled);
        assert_eq!(list.active_count(), 0);
    }

    #[test]
    fn interrupted_downloads_are_failed_on_load() {
        let mut list = DownloadList::default();
        list.add(DownloadItem {
            id: DownloadId(2),
            url: url("https://a.com/f"),
            file_name: "f".into(),
            destination: PathBuf::from("f"),
            state: DownloadState::InProgress { received: 1, total: None },
            started: 0,
        });
        list.mark_interrupted();
        assert!(matches!(list.items()[0].state, DownloadState::Failed { .. }));
    }

    #[test]
    fn persisted_list_round_trips_and_reserves_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("downloads.json");
        let mut list = DownloadList::default();
        let id = DownloadId(5_000);
        list.add(DownloadItem {
            id,
            url: url("https://a.com/f.zip"),
            file_name: "f.zip".into(),
            destination: PathBuf::new(),
            state: DownloadState::InProgress { received: 1, total: None },
            started: 0,
        });
        assert!(list.set_target(id, "g.zip".into(), PathBuf::from("d/g.zip")));
        list.save(&path).unwrap();

        let loaded = DownloadList::load(&path).unwrap();
        let item = loaded.get(id).unwrap();
        assert_eq!(item.file_name, "g.zip");
        assert_eq!(item.destination, PathBuf::from("d/g.zip"));
        assert!(matches!(item.state, DownloadState::Failed { .. }));
        assert!(DownloadId::next().0 > 5_000);

        let mut loaded = loaded;
        assert!(loaded.remove(id));
        assert!(loaded.items().is_empty());
    }
}
