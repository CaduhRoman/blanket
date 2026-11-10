//! Crash-safe JSON persistence used by history, bookmarks, sessions and permissions.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::warn;

use crate::error::{CoreError, CoreResult};

/// Load a JSON document, falling back to `T::default()` when the file does not exist.
///
/// A file that exists but cannot be parsed is *not* silently overwritten: it is moved
/// aside to `<name>.corrupt` so the user's data can still be recovered by hand.
pub fn load_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> CoreResult<T> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(CoreError::io(path, error)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(value),
        Err(error) => {
            let backup = sibling_with_suffix(path, "corrupt");
            warn!(path = %path.display(), backup = %backup.display(), %error, "corrupt data file, moving aside");
            fs::rename(path, &backup).map_err(|error| CoreError::io(path, error))?;
            Ok(T::default())
        },
    }
}

/// Atomically replace `path` with the JSON serialization of `value`.
///
/// The data is written to a temporary sibling file, flushed to disk and then renamed over
/// the destination, so a crash mid-write never leaves a truncated file behind.
pub fn save_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| CoreError::io(parent, error))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| CoreError::Serialize {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    let temporary = sibling_with_suffix(path, "tmp");
    {
        let mut file = fs::File::create(&temporary).map_err(|error| CoreError::io(&temporary, error))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| CoreError::io(&temporary, error))?;
    }
    fs::rename(&temporary, path).map_err(|error| CoreError::io(path, error))
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Doc {
        items: Vec<String>,
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let doc: Doc = load_json_or_default(&dir.path().join("missing.json")).unwrap();
        assert_eq!(doc, Doc::default());
    }

    #[test]
    fn round_trip_is_atomic_and_lossless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("doc.json");
        let doc = Doc {
            items: vec!["a".into(), "b".into()],
        };
        save_json_atomic(&path, &doc).unwrap();
        assert!(!sibling_with_suffix(&path, "tmp").exists());
        let loaded: Doc = load_json_or_default(&path).unwrap();
        assert_eq!(loaded, doc);
    }

    #[test]
    fn corrupt_file_is_preserved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.json");
        fs::write(&path, b"{ not json").unwrap();
        let loaded: Doc = load_json_or_default(&path).unwrap();
        assert_eq!(loaded, Doc::default());
        assert!(sibling_with_suffix(&path, "corrupt").exists());
    }
}
