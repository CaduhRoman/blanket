//! End-to-end download tests against a local HTTP server.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use blanket_core::config::SecurityConfig;
use blanket_core::ids::DownloadId;
use blanket_net::{DownloadEvent, DownloadRequest, NetService};
use url::Url;

/// Serve `routes` (path -> (headers, body, delay per 16 KiB chunk)) until the test ends.
fn serve(routes: Vec<(&'static str, &'static str, Vec<u8>, Duration)>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let routes = Arc::new(routes);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = routes.clone();
            thread::spawn(move || handle(stream, &routes));
        }
    });
    Url::parse(&format!("http://{address}/")).unwrap()
}

fn handle(mut stream: TcpStream, routes: &[(&str, &str, Vec<u8>, Duration)]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut cookie = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.to_ascii_lowercase().starts_with("cookie:") {
            cookie = line[7..].trim().to_owned();
        }
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let Some((_, headers, body, delay)) = routes.iter().find(|(route, ..)| *route == path) else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    if path == "/needs-cookie" && cookie != "session=abc" {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    for chunk in body.chunks(16 * 1024) {
        if stream.write_all(chunk).is_err() {
            return;
        }
        thread::sleep(*delay);
    }
}

fn request(url: Url, directory: &Path, id: u64) -> DownloadRequest {
    DownloadRequest {
        id: DownloadId(id),
        url,
        directory: directory.to_owned(),
        cookie_header: None,
        referrer: None,
        user_agent: "BlanketTest/1.0".into(),
        security: SecurityConfig::default(),
    }
}

fn wait_for(service: &NetService, id: DownloadId, mut done: impl FnMut(&DownloadEvent) -> bool) -> Vec<DownloadEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut events = Vec::new();
    while Instant::now() < deadline {
        for event in service.poll_events() {
            if event.id() == id {
                let finished = done(&event);
                events.push(event);
                if finished {
                    return events;
                }
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out; events so far: {events:?}");
}

fn is_final(event: &DownloadEvent) -> bool {
    matches!(
        event,
        DownloadEvent::Completed { .. } | DownloadEvent::Failed { .. } | DownloadEvent::Cancelled { .. }
    )
}

#[test]
fn downloads_file_with_server_provided_name() {
    let body: Vec<u8> = (0..200_000u32).map(|value| value as u8).collect();
    let base = serve(vec![(
        "/file",
        "Content-Disposition: attachment; filename=\"report.bin\"\r\n",
        body.clone(),
        Duration::ZERO,
    )]);
    let directory = tempfile::tempdir().unwrap();
    let service = NetService::new(Arc::new(|| {}));
    assert!(!service.is_started(), "runtime is created lazily");
    service.start_download(request(base.join("file").unwrap(), directory.path(), 1));
    let events = wait_for(&service, DownloadId(1), is_final);
    assert!(matches!(events.last(), Some(DownloadEvent::Completed { bytes: 200_000, .. })), "{events:?}");
    let saved = std::fs::read(directory.path().join("report.bin")).unwrap();
    assert_eq!(saved, body);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1, "no partial file left behind");
}

#[test]
fn existing_files_are_not_overwritten() {
    let base = serve(vec![("/a.txt", "", b"new".to_vec(), Duration::ZERO)]);
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a.txt"), b"old").unwrap();
    let service = NetService::new(Arc::new(|| {}));
    service.start_download(request(base.join("a.txt").unwrap(), directory.path(), 2));
    wait_for(&service, DownloadId(2), is_final);
    assert_eq!(std::fs::read(directory.path().join("a.txt")).unwrap(), b"old");
    assert_eq!(std::fs::read(directory.path().join("a (1).txt")).unwrap(), b"new");
}

#[test]
fn cancellation_removes_partial_data() {
    let base = serve(vec![("/slow.bin", "", vec![7; 2 * 1024 * 1024], Duration::from_millis(20))]);
    let directory = tempfile::tempdir().unwrap();
    let service = NetService::new(Arc::new(|| {}));
    let id = DownloadId(3);
    service.start_download(request(base.join("slow.bin").unwrap(), directory.path(), 3));
    wait_for(&service, id, |event| matches!(event, DownloadEvent::Started { .. }));
    service.cancel_download(id);
    let events = wait_for(&service, id, is_final);
    assert!(matches!(events.last(), Some(DownloadEvent::Cancelled { .. })), "{events:?}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn executables_require_confirmation() {
    let base = serve(vec![("/setup.exe", "", b"MZ".to_vec(), Duration::ZERO)]);
    let directory = tempfile::tempdir().unwrap();
    let service = NetService::new(Arc::new(|| {}));

    let denied = DownloadId(4);
    service.start_download(request(base.join("setup.exe").unwrap(), directory.path(), 4));
    wait_for(&service, denied, |event| matches!(event, DownloadEvent::AwaitingConfirmation { .. }));
    service.confirm_download(denied, false);
    let events = wait_for(&service, denied, is_final);
    assert!(matches!(events.last(), Some(DownloadEvent::Cancelled { .. })));
    assert!(!directory.path().join("setup.exe").exists());

    let allowed = DownloadId(5);
    service.start_download(request(base.join("setup.exe").unwrap(), directory.path(), 5));
    wait_for(&service, allowed, |event| matches!(event, DownloadEvent::AwaitingConfirmation { .. }));
    service.confirm_download(allowed, true);
    let events = wait_for(&service, allowed, is_final);
    assert!(matches!(events.last(), Some(DownloadEvent::Completed { bytes: 2, .. })), "{events:?}");
    assert!(directory.path().join("setup.exe").exists());
}

#[test]
fn http_errors_fail_and_cookies_are_forwarded() {
    let base = serve(vec![("/needs-cookie", "", b"secret".to_vec(), Duration::ZERO)]);
    let directory = tempfile::tempdir().unwrap();
    let service = NetService::new(Arc::new(|| {}));

    service.start_download(request(base.join("needs-cookie").unwrap(), directory.path(), 6));
    let events = wait_for(&service, DownloadId(6), is_final);
    assert!(matches!(events.last(), Some(DownloadEvent::Failed { .. })), "{events:?}");

    let mut with_cookie = request(base.join("needs-cookie").unwrap(), directory.path(), 7);
    with_cookie.cookie_header = Some("session=abc".into());
    service.start_download(with_cookie);
    let events = wait_for(&service, DownloadId(7), is_final);
    assert!(matches!(events.last(), Some(DownloadEvent::Completed { bytes: 6, .. })), "{events:?}");
}
