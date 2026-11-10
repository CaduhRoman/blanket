//! Locations of Blanket's on-disk profile.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, CoreResult};

/// Environment variable that overrides the profile directory. Benchmarks use it to start
/// every run from a clean profile, exactly like they do for Chrome and Firefox.
pub const PROFILE_DIR_ENV: &str = "BLANKET_PROFILE_DIR";

/// All files that make up a user profile.
#[derive(Clone, Debug)]
pub struct ProfilePaths {
    root: PathBuf,
}

impl ProfilePaths {
    /// Resolve the profile directory: explicit argument, then `BLANKET_PROFILE_DIR`, then
    /// the platform data directory (`%APPDATA%\Blanket` on Windows).
    pub fn resolve(explicit: Option<&Path>) -> CoreResult<Self> {
        if let Some(path) = explicit {
            return Ok(Self::at(path));
        }
        if let Some(path) = std::env::var_os(PROFILE_DIR_ENV) {
            return Ok(Self::at(PathBuf::from(path)));
        }
        let base = dirs::data_dir().ok_or(CoreError::NoDataDir)?;
        Ok(Self::at(base.join("Blanket")))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("blanket.toml")
    }

    pub fn history(&self) -> PathBuf {
        self.root.join("history.json")
    }

    pub fn bookmarks(&self) -> PathBuf {
        self.root.join("bookmarks.json")
    }

    pub fn session(&self) -> PathBuf {
        self.root.join("session.json")
    }

    pub fn permissions(&self) -> PathBuf {
        self.root.join("permissions.json")
    }

    pub fn downloads_log(&self) -> PathBuf {
        self.root.join("downloads.json")
    }

    /// Directory handed to Servo for its own persistent state (cookies, local storage).
    pub fn engine_config_dir(&self) -> PathBuf {
        self.root.join("engine")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
}

/// Default directory for completed downloads.
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}
