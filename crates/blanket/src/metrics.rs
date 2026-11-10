//! Machine-readable timing events for benchmarking (`--metrics-file`).
//!
//! Each line is a JSON object with the milliseconds elapsed since process start (`t_ms`),
//! an `event` name and event-specific fields. Recording is a no-op when no file was
//! requested, so normal sessions pay nothing.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use serde_json::{Map, Value};
use tracing::warn;

pub struct Metrics {
    start: Instant,
    sink: Option<RefCell<BufWriter<File>>>,
}

impl Metrics {
    pub fn new(start: Instant, path: Option<&Path>) -> Self {
        let sink = path.and_then(|path| {
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(file) => Some(RefCell::new(BufWriter::new(file))),
                Err(error) => {
                    warn!(%error, path = %path.display(), "cannot open metrics file");
                    None
                },
            }
        });
        Self { start, sink }
    }

    pub fn enabled(&self) -> bool {
        self.sink.is_some()
    }

    pub fn elapsed_ms(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.start).as_secs_f64() * 1000.0
    }

    /// Record `event` with extra `fields` (must be a JSON object, or `Value::Null`).
    pub fn record(&self, event: &str, fields: Value) {
        let Some(sink) = &self.sink else {
            return;
        };
        let mut object = match fields {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        object.insert("t_ms".into(), Value::from(self.elapsed_ms(Instant::now())));
        object.insert("event".into(), Value::from(event));
        let mut sink = sink.borrow_mut();
        let result = serde_json::to_writer(&mut *sink, &Value::Object(object))
            .map_err(std::io::Error::other)
            .and_then(|()| sink.write_all(b"\n"))
            .and_then(|()| sink.flush());
        if let Err(error) = result {
            warn!(%error, "failed to write metrics event");
        }
    }
}
