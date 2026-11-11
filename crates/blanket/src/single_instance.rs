//! One browser process per profile, like Chrome.
//!
//! Two processes on the same profile would restore the same session (duplicating every
//! tab) and write the same history, session, cookie and WebView2 files concurrently. The
//! first process holds an exclusive lock on `instance.lock` in the profile and listens on
//! a loopback port; a later launch forwards its command line (URLs, or nothing for "a new
//! tab") to it and exits, and the running browser comes to the front.
//!
//! The port and a random token are in `instance.json`, next to the lock. The token keeps
//! other local programs from injecting URLs without being able to read the profile.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const LOCK_FILE: &str = "instance.lock";
const INFO_FILE: &str = "instance.json";

#[derive(Serialize, Deserialize)]
struct InstanceInfo {
    port: u16,
    token: String,
}

/// What another launch asked the running browser to open: URLs or search terms (empty
/// means "just a new tab").
pub type OpenRequest = Vec<String>;

pub enum Instance {
    /// This process owns the profile.
    Primary(Primary),
    /// The request went to the browser already running on this profile.
    Forwarded,
}

pub struct Primary {
    lock: Option<File>,
    listener: Option<TcpListener>,
    token: String,
}

impl Primary {
    /// Accept requests from later launches on a background thread. Each one is queued
    /// in `inbox`, then `wake` is called so the UI thread picks it up.
    pub fn serve(self, inbox: Arc<Mutex<Vec<OpenRequest>>>, wake: impl Fn() + Send + 'static) {
        let Self { lock: _lock, listener, token } = self;
        let Some(listener) = listener else {
            // Nothing to serve; keep the lock alive for the process lifetime anyway.
            std::mem::forget(_lock);
            return;
        };
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                // Keep the lock for as long as the process lives.
                let _lock = _lock;
                for stream in listener.incoming().flatten() {
                    match read_request(stream, &token) {
                        Some(request) => {
                            info!(urls = request.len(), "request from another launch");
                            inbox.lock().unwrap_or_else(|poison| poison.into_inner()).push(request);
                            wake();
                        },
                        None => warn!("ignored an invalid single-instance request"),
                    }
                }
            })
            .ok();
    }
}

fn read_request(stream: TcpStream, token: &str) -> Option<OpenRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut reader = BufReader::new(stream.take_limited());
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.trim_end() != token {
        return None;
    }
    line.clear();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim_end()).ok()
}

trait TakeLimited {
    fn take_limited(self) -> std::io::Take<TcpStream>;
}

impl TakeLimited for TcpStream {
    /// Requests are a token and a short list of URLs; refuse to buffer more than 1 MiB.
    fn take_limited(self) -> std::io::Take<TcpStream> {
        std::io::Read::take(self, 1024 * 1024)
    }
}

/// Become the profile's browser process, or hand `urls` to the one already running.
pub fn acquire(profile: &Path, urls: &[String]) -> Instance {
    let _ = std::fs::create_dir_all(profile);
    let lock_path = profile.join(LOCK_FILE);
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path);
    let lock = match lock {
        Ok(file) => file,
        Err(error) => {
            // Without a lock file there is no coordination; run rather than refuse.
            warn!(%error, "cannot open the instance lock; running without single-instance");
            return become_primary(profile, None);
        },
    };
    match lock.try_lock() {
        Ok(()) => become_primary(profile, Some(lock)),
        Err(_) => {
            if forward(profile, urls) {
                Instance::Forwarded
            } else {
                // The owner holds the lock but cannot be reached (still starting, or
                // shutting down). Opening a second browser on the profile is what this
                // module prevents, so stop here.
                warn!("another Blanket owns this profile but did not answer; exiting");
                Instance::Forwarded
            }
        },
    }
}

fn become_primary(profile: &Path, lock: Option<File>) -> Instance {
    let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) else {
        warn!("cannot listen for other launches; later launches will not be forwarded");
        return Instance::Primary(Primary {
            lock,
            listener: None,
            token: String::new(),
        });
    };
    let port = listener.local_addr().map(|address| address.port()).unwrap_or_default();
    let token = random_token();
    let info = InstanceInfo { port, token: token.clone() };
    if let Ok(text) = serde_json::to_string(&info) {
        if let Err(error) = std::fs::write(profile.join(INFO_FILE), text) {
            warn!(%error, "cannot write the instance info");
        }
    }
    Instance::Primary(Primary {
        lock,
        listener: Some(listener),
        token,
    })
}

/// Send `urls` to the running instance. Retries briefly in case it is still starting.
fn forward(profile: &Path, urls: &[String]) -> bool {
    for _ in 0..20 {
        if let Some(info) = std::fs::read_to_string(profile.join(INFO_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<InstanceInfo>(&text).ok())
        {
            allow_foreground();
            if let Ok(mut stream) = TcpStream::connect_timeout(
                &(Ipv4Addr::LOCALHOST, info.port).into(),
                Duration::from_millis(500),
            ) {
                let payload = serde_json::to_string(urls).unwrap_or_else(|_| "[]".into());
                if stream.write_all(format!("{}\n{payload}\n", info.token).as_bytes()).is_ok() {
                    info!("handed the request to the running Blanket");
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

/// Windows only lets the foreground process raise a window; the new launch is in the
/// foreground, so it allows the running browser to come forward.
fn allow_foreground() {
    #[cfg(windows)]
    // SAFETY: plain Win32 call without pointers.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
}

fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut token = String::new();
    for round in 0..4u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(round);
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        hasher.write_u32(std::process::id());
        token.push_str(&format!("{:016x}", hasher.finish()));
    }
    token
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_launch_is_forwarded_to_the_first() {
        let profile = tempfile::tempdir().unwrap();
        let Instance::Primary(primary) = acquire(profile.path(), &[]) else {
            panic!("the first launch owns the profile");
        };
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        primary.serve(inbox.clone(), move || {
            let _ = tx.send(());
        });
        let urls = vec!["https://example.com/".to_owned()];
        assert!(matches!(acquire(profile.path(), &urls), Instance::Forwarded));
        rx.recv_timeout(Duration::from_secs(5)).expect("request delivered");
        assert_eq!(inbox.lock().unwrap().as_slice(), &[urls]);
    }

    #[test]
    fn requests_need_the_token() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        client.write_all(b"wrong\n[\"https://evil.example/\"]\n").unwrap();
        let (server, _) = listener.accept().unwrap();
        assert_eq!(read_request(server, "right"), None);
    }
}
