//! Blanket: a lightweight desktop web browser built on the Servo engine.

// Release builds are GUI applications without a console window; logs go to the profile.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod app_icon;
mod browser;
mod cli;
mod events;
mod keyutils;
mod logging;
#[cfg(windows)]
mod runtime_files;
mod metrics;
mod protocol;
mod single_instance;
mod ui;

use std::process::ExitCode;
use std::time::Instant;

use blanket_core::config::BlanketConfig;
use blanket_core::paths::ProfilePaths;
use clap::Parser;
use tracing::{error, info, warn};
use winit::event_loop::EventLoop;

use crate::app::{App, StartupOptions};
use crate::cli::Cli;
use crate::events::AppEvent;
use crate::metrics::Metrics;

fn main() -> ExitCode {
    let process_start = Instant::now();
    let cli = Cli::parse();

    let paths = match ProfilePaths::resolve(cli.profile_dir.as_deref()) {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("blanket: cannot determine the profile directory: {error}");
            return ExitCode::FAILURE;
        },
    };
    let _log_guard = logging::init(&paths, cli.log_json);
    // One browser per profile: a second launch hands its URLs to the running one.
    let instance = match single_instance::acquire(paths.root(), &cli.urls) {
        single_instance::Instance::Primary(primary) => primary,
        single_instance::Instance::Forwarded => return ExitCode::SUCCESS,
    };
    // ANGLE and (with audio/video) the delay-loaded GStreamer must be findable first.
    #[cfg(windows)]
    if !runtime_files::prepare() {
        return ExitCode::FAILURE;
    }
    info!(version = env!("CARGO_PKG_VERSION"), profile = %paths.root().display(), "starting Blanket");

    // Servo's network stack and our download client both use rustls; install the
    // aws-lc-rs provider once for the whole process.
    if rustls::crypto::aws_lc_rs::default_provider().install_default().is_err() {
        warn!("a rustls crypto provider was already installed");
    }

    let config = match BlanketConfig::load_or_create(&paths.config()) {
        Ok(config) => config,
        Err(error) => {
            warn!(%error, "invalid configuration file, using defaults");
            BlanketConfig::default()
        },
    };

    let metrics = Metrics::new(process_start, cli.metrics_file.as_deref());
    let event_loop = match EventLoop::<AppEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            error!(%error, "failed to create the event loop");
            return ExitCode::FAILURE;
        },
    };

    let options = StartupOptions {
        paths,
        config,
        urls: cli.urls,
        restore_session: !cli.no_restore,
        metrics,
        instance,
    };
    let mut app = App::new(&event_loop, options);
    if let Err(error) = event_loop.run_app(&mut app) {
        error!(%error, "event loop terminated with an error");
        return ExitCode::FAILURE;
    }
    if app.failed() {
        return ExitCode::FAILURE;
    }
    info!("Blanket exited cleanly");
    ExitCode::SUCCESS
}
