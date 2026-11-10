//! Reproducible resource measurements for Blanket, and optional side-by-side runs of
//! Chrome and Firefox under the same URLs and tab counts.
//!
//! This tool records numbers. It does not declare that Blanket is faster or lighter than
//! another browser — compare the JSON yourself, on your machine, with the same pages.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tempfile::TempDir;

#[derive(Clone, Copy, Debug, ValueEnum, Serialize, PartialEq, Eq)]
enum BrowserKind {
    Blanket,
    Chrome,
    Firefox,
}

#[derive(Parser, Debug)]
#[command(name = "blanket-bench", version, about = "Measure Blanket, Chrome and Firefox under the same load.")]
struct Cli {
    /// Page opened in every tab.
    #[arg(long, default_value = "https://example.com/")]
    url: String,

    /// Tab counts to measure, as a comma-separated list.
    #[arg(long, default_value = "1,5,10", value_delimiter = ',')]
    tabs: Vec<usize>,

    /// Browsers to launch. Missing installations are skipped, not failed.
    #[arg(long, value_enum, default_value = "blanket,chrome,firefox", value_delimiter = ',')]
    browsers: Vec<BrowserKind>,

    /// Seconds to wait after launch before sampling (page load + compositor settle).
    #[arg(long, default_value_t = 12)]
    settle_secs: u64,

    /// Seconds of idle sampling after settle, used for background CPU.
    #[arg(long, default_value_t = 5)]
    idle_secs: u64,

    /// Path to blanket.exe. Defaults to target/release/blanket.exe next to this workspace.
    #[arg(long)]
    blanket: Option<PathBuf>,

    /// JSON report path. Printed to stdout as well.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Serialize)]
struct Report {
    url: String,
    settle_secs: u64,
    idle_secs: u64,
    host: HostInfo,
    runs: Vec<Run>,
}

#[derive(Serialize)]
struct HostInfo {
    os: String,
    total_memory_bytes: u64,
}

#[derive(Serialize)]
struct Run {
    browser: BrowserKind,
    tabs: usize,
    ok: bool,
    error: Option<String>,
    startup_ms: Option<f64>,
    first_frame_ms: Option<f64>,
    load_complete_ms: Option<f64>,
    rss_bytes: Option<u64>,
    idle_cpu_percent: Option<f32>,
}

fn main() {
    let cli = Cli::parse();
    let mut system = System::new_with_specifics(RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()))
        .unwrap_or_else(|error| panic!("system statistics unavailable: {error}"));
    system.refresh_memory();
    let mut report = Report {
        url: cli.url.clone(),
        settle_secs: cli.settle_secs,
        idle_secs: cli.idle_secs,
        host: HostInfo {
            os: std::env::consts::OS.to_owned(),
            total_memory_bytes: system.total_memory(),
        },
        runs: Vec::new(),
    };

    for browser in &cli.browsers {
        let Some(exe) = locate(*browser, cli.blanket.as_deref()) else {
            eprintln!("skipping {browser:?}: executable not found");
            continue;
        };
        for &tabs in &cli.tabs {
            if tabs == 0 {
                continue;
            }
            eprintln!("measuring {browser:?} · {tabs} tab(s) · {}", cli.url);
            report.runs.push(measure(*browser, &exe, &cli, tabs, &mut system));
        }
    }

    let json = serde_json::to_string_pretty(&report).expect("report serializes");
    if let Some(path) = &cli.output {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, &json).unwrap_or_else(|error| panic!("could not write {}: {error}", path.display()));
        eprintln!("wrote {}", path.display());
    }
    println!("{json}");
}

fn locate(kind: BrowserKind, blanket: Option<&Path>) -> Option<PathBuf> {
    match kind {
        BrowserKind::Blanket => {
            if let Some(path) = blanket {
                return path.exists().then(|| path.to_path_buf());
            }
            let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            let release = workspace.join("target/release/blanket.exe");
            if release.exists() {
                return Some(release);
            }
            let debug = workspace.join("target/debug/blanket.exe");
            debug.exists().then_some(debug)
        },
        BrowserKind::Chrome => first_existing(&[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ]),
        BrowserKind::Firefox => first_existing(&[
            r"C:\Program Files\Mozilla Firefox\firefox.exe",
            r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
        ]),
    }
}

fn first_existing(paths: &[&str]) -> Option<PathBuf> {
    paths.iter().map(PathBuf::from).find(|path| path.exists())
}

fn measure(kind: BrowserKind, exe: &Path, cli: &Cli, tabs: usize, system: &mut System) -> Run {
    let profile = match TempDir::new() {
        Ok(dir) => dir,
        Err(error) => {
            return Run::failed(kind, tabs, error.to_string());
        },
    };
    let metrics_path = profile.path().join("metrics.jsonl");
    let urls = vec![cli.url.clone(); tabs];
    let mut command = Command::new(exe);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match kind {
        BrowserKind::Blanket => {
            command
                .arg("--profile-dir")
                .arg(profile.path())
                .arg("--no-restore")
                .arg("--metrics-file")
                .arg(&metrics_path)
                .args(&urls);
        },
        BrowserKind::Chrome => {
            command
                .arg(format!("--user-data-dir={}", profile.path().display()))
                .arg("--no-first-run")
                .arg("--no-default-browser-check")
                .arg("--disable-sync")
                .arg("--disable-extensions")
                .args(&urls);
        },
        BrowserKind::Firefox => {
            command.arg("-profile").arg(profile.path()).arg("-no-remote").args(&urls);
        },
    }

    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Run::failed(kind, tabs, error.to_string()),
    };
    let pid = Pid::from_u32(child.id());
    thread::sleep(Duration::from_secs(cli.settle_secs));

    let rss = sample_rss(system, pid);
    let cpu_first = sample_cpu(system, pid);
    thread::sleep(Duration::from_secs(cli.idle_secs.max(1)));
    let cpu_second = sample_cpu(system, pid);
    let idle_cpu = cpu_second.or(cpu_first);

    let (first_frame_ms, load_complete_ms) = if kind == BrowserKind::Blanket {
        parse_blanket_metrics(&metrics_path)
    } else {
        (None, None)
    };

    stop(&mut child);
    drop(profile);

    Run {
        browser: kind,
        tabs,
        ok: true,
        error: None,
        startup_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
        first_frame_ms,
        load_complete_ms,
        rss_bytes: rss,
        idle_cpu_percent: idle_cpu,
    }
}

impl Run {
    fn failed(browser: BrowserKind, tabs: usize, error: String) -> Self {
        Self {
            browser,
            tabs,
            ok: false,
            error: Some(error),
            startup_ms: None,
            first_frame_ms: None,
            load_complete_ms: None,
            rss_bytes: None,
            idle_cpu_percent: None,
        }
    }
}

fn sample_rss(system: &mut System, pid: Pid) -> Option<u64> {
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_memory());
    system.process(pid).map(|process| process.memory())
}

fn sample_cpu(system: &mut System, pid: Pid) -> Option<f32> {
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_cpu());
    system.process(pid).map(|process| process.cpu_usage())
}

fn parse_blanket_metrics(path: &Path) -> (Option<f64>, Option<f64>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (None, None);
    };
    let mut first_frame = None;
    let mut load = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let event = value.get("event").and_then(|event| event.as_str());
        let t_ms = value.get("t_ms").and_then(|value| value.as_f64());
        match (event, t_ms) {
            (Some("first_frame"), Some(ms)) => first_frame = Some(ms),
            (Some("load_complete"), Some(ms)) => {
                if let Some(load_ms) = value.get("load_ms").and_then(|value| value.as_f64()) {
                    load = Some(load_ms);
                } else {
                    load = Some(ms);
                }
            },
            _ => {},
        }
    }
    (first_frame, load)
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
