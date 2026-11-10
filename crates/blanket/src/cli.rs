use std::path::PathBuf;

use clap::Parser;

/// Blanket web browser.
#[derive(Debug, Parser)]
#[command(name = "blanket", version, about)]
pub struct Cli {
    /// URLs or search terms to open, one tab each.
    pub urls: Vec<String>,

    /// Profile directory (configuration, history, bookmarks, session, engine data).
    /// Defaults to $BLANKET_PROFILE_DIR, then %APPDATA%\Blanket.
    #[arg(long, value_name = "DIR")]
    pub profile_dir: Option<PathBuf>,

    /// Do not restore the previous session.
    #[arg(long)]
    pub no_restore: bool,

    /// Append machine-readable timing events (JSON lines) to this file. Used by
    /// blanket-bench.
    #[arg(long, value_name = "FILE")]
    pub metrics_file: Option<PathBuf>,

    /// Write the log file as JSON lines instead of plain text.
    #[arg(long)]
    pub log_json: bool,
}
