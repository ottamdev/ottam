//! A minimal blocking JSONL JSON-RPC client over any line-based transport.
//!
//! Deliberately independent of the ottam-core test harness's transport
//! layer (which is panic-style and owned by ~15 test files); this one returns
//! `Result`s because a CLI reports failures instead of unwinding.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub struct Connection {
    writer: Box<dyn Write + Send>,
    lines: Receiver<String>,
    /// Messages read past while correlating a response; replayed in order to
    /// the next reader, matching ordinary JSON-RPC interleaving.
    buffered: Vec<Value>,
}

pub enum Incoming {
    Message(Value),
    /// The transport closed (core exited or shut the connection down).
    Eof,
}

impl Connection {
    /// Wraps a transport. The read side is drained by a thread so reads can
    /// time out uniformly across sockets and child pipes.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(reader);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            writer: Box::new(writer),
            lines: rx,
            buffered: Vec::new(),
        }
    }

    pub fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("failed to write to core: {error}"))
    }

    /// The next message (buffered first), or `Eof`; errors on timeout.
    pub fn read(&mut self, timeout: Duration) -> Result<Incoming, String> {
        if !self.buffered.is_empty() {
            return Ok(Incoming::Message(self.buffered.remove(0)));
        }
        self.read_skipping_buffer(timeout)
    }

    fn read_skipping_buffer(&mut self, timeout: Duration) -> Result<Incoming, String> {
        loop {
            match self.lines.recv_timeout(timeout) {
                Ok(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value = serde_json::from_str(&line)
                        .map_err(|error| format!("core sent a non-JSON line: {error}"))?;
                    return Ok(Incoming::Message(value));
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(Incoming::Eof),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "timed out after {}s waiting for the core",
                        timeout.as_secs()
                    ))
                }
            }
        }
    }

    /// Sends `request` and reads until the response with the same `id`
    /// arrives, buffering everything else (notifications) in between.
    pub fn round_trip(&mut self, request: &Value, timeout: Duration) -> Result<Value, String> {
        let id = request
            .get("id")
            .cloned()
            .ok_or("request has no id".to_string())?;
        self.send(request)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    format!(
                        "timed out after {}s waiting for the core",
                        timeout.as_secs()
                    )
                })?;
            match self.read_skipping_buffer(remaining)? {
                Incoming::Eof => {
                    return Err("core closed the connection before responding".to_string())
                }
                Incoming::Message(value) => {
                    if value.get("id") == Some(&id) {
                        return Ok(value);
                    }
                    self.buffered.push(value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scripted read side: yields the given lines, then EOF.
    fn scripted(lines: &[Value]) -> Connection {
        let text = lines
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        Connection::new(std::io::Cursor::new(text), std::io::sink())
    }

    #[test]
    fn round_trip_correlates_by_id_and_buffers_notifications() {
        let mut connection = scripted(&[
            json!({"method": "agent.installEvent", "params": {"phase": "installing"}}),
            json!({"id": "insp/1", "result": {"ok": true}}),
            json!({"method": "later.event", "params": {}}),
        ]);
        let response = connection
            .round_trip(
                &json!({"id": "insp/1", "method": "workspace.list", "params": {}}),
                Duration::from_secs(1),
            )
            .expect("response");
        assert_eq!(response["result"], json!({"ok": true}));

        // The skipped notification replays before later traffic.
        let Incoming::Message(first) = connection.read(Duration::from_secs(1)).expect("read")
        else {
            panic!("expected buffered notification");
        };
        assert_eq!(first["method"], json!("agent.installEvent"));
        let Incoming::Message(second) = connection.read(Duration::from_secs(1)).expect("read")
        else {
            panic!("expected trailing message");
        };
        assert_eq!(second["method"], json!("later.event"));
        assert!(matches!(
            connection.read(Duration::from_secs(1)).expect("read"),
            Incoming::Eof
        ));
    }

    #[test]
    fn eof_before_a_response_is_an_error() {
        let mut connection = scripted(&[]);
        let error = connection
            .round_trip(
                &json!({"id": 1, "method": "workspace.list", "params": {}}),
                Duration::from_secs(1),
            )
            .expect_err("no response");
        assert!(error.contains("closed the connection"), "{error}");
    }
}
