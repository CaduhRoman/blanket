//! Structured logging: human-readable output on stderr plus a daily-rotated file in the
//! profile's `logs` directory. Servo logs through the `log` crate, which
//! `tracing-subscriber` bridges automatically, so engine messages share the same sinks.
//! The filter is configured with the `BLANKET_LOG` environment variable
//! (`EnvFilter` syntax).

use blanket_core::paths::ProfilePaths;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const DEFAULT_FILTER: &str = "warn,blanket=info,blanket_core=info,blanket_net=info";
const KEPT_LOG_FILES: usize = 5;

/// Install the global subscriber. The returned guard flushes the file writer on drop and
/// must be kept alive for the lifetime of the process.
pub fn init(paths: &ProfilePaths, json: bool) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_env("BLANKET_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let (file_writer, guard) = match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("blanket")
        .filename_suffix("log")
        .max_log_files(KEPT_LOG_FILES)
        .build(paths.logs_dir())
    {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (Some(writer), Some(guard))
        },
        Err(error) => {
            eprintln!("blanket: file logging disabled: {error}");
            (None, None)
        },
    };

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_target(true);
    let file_plain = file_writer
        .clone()
        .filter(|_| !json)
        .map(|writer| fmt::layer().with_ansi(false).with_writer(writer));
    let file_json = file_writer
        .filter(|_| json)
        .map(|writer| fmt::layer().json().with_writer(writer));

    if let Err(error) = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_plain)
        .with(file_json)
        .try_init()
    {
        eprintln!("blanket: logging already initialized: {error}");
    }
    guard
}
