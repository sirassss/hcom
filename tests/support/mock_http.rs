//! Tool-agnostic localhost HTTP transport for the real-tool integration tests.
//!
//! The real Codex/Claude binaries have no fake-response mode, so each is driven
//! by pointing it at a localhost provider and scripting every turn as SSE. This
//! module owns only the transport: bind `127.0.0.1:0`, parse a complete HTTP
//! request (method, path, headers, body), hand it to a responder, and write the
//! framed reply. The wire codecs (OpenAI Responses vs Anthropic Messages) and
//! turn classification live in the per-tool `*_mock.rs` modules so a single
//! transport serves every tool — Claude needs path routing (`/v1/messages` vs
//! `/v1/messages/count_tokens`) that a body-only mock cannot express.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// One fully parsed HTTP request the responder classifies into a scripted turn.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl RecordedRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Parse the body as JSON, or `None` if it is not valid JSON.
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// What the mock should answer for one request.
pub enum Reply {
    /// A fully framed SSE event-stream body (see each tool's `sse` helper).
    Sse(Vec<u8>),
    /// A `200 OK` `application/json` body (e.g. Anthropic `count_tokens`).
    Json(String),
    /// A `200 OK` body with an explicit content type, for non-JSON wire formats
    /// (e.g. Cursor's `application/proto` Connect unary responses).
    Raw {
        content_type: &'static str,
        body: Vec<u8>,
    },
    /// A benign empty-body status that is NOT flagged unexpected — for legitimate
    /// auxiliary requests (e.g. Claude's `HEAD /` reachability probe).
    Empty(u16),
    /// An HTTP status with an empty body, recorded as unexpected so a
    /// mis-scripted turn or stray route fails the test loudly.
    Status(u16),
}

/// A scripted localhost HTTP provider shared by every real-tool mock.
pub struct MockHttp {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    unexpected: Arc<Mutex<Vec<RecordedRequest>>>,
    errors: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockHttp {
    /// Start serving on an ephemeral localhost port. `responder` maps each
    /// parsed request to a [`Reply`]; it runs on worker threads so it must be
    /// `Send + Sync`.
    pub fn start<F>(responder: F) -> std::io::Result<Self>
    where
        F: Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;

        let requests = Arc::new(Mutex::new(Vec::new()));
        let unexpected = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let responder = Arc::new(responder);

        let h_requests = requests.clone();
        let h_unexpected = unexpected.clone();
        let h_errors = errors.clone();
        let h_shutdown = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = Vec::new();
            while !h_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let requests = h_requests.clone();
                        let unexpected = h_unexpected.clone();
                        let errors = h_errors.clone();
                        let responder = responder.clone();
                        workers.push(std::thread::spawn(move || {
                            if let Err(e) = handle_conn(stream, requests, unexpected, responder) {
                                // A reset/timeout after the reply is already
                                // written is benign (clients close eagerly);
                                // record everything else for the audit.
                                if e.kind() != std::io::ErrorKind::BrokenPipe {
                                    errors.lock().expect("errors lock").push(e.to_string());
                                }
                            }
                        }));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });

        Ok(Self {
            addr,
            requests,
            unexpected,
            errors,
            shutdown,
            handle: Some(handle),
        })
    }

    /// Ephemeral port the listener bound.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Every request observed so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    /// Request bodies, in arrival order — convenience for body-only matching.
    pub fn request_bodies(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests lock")
            .iter()
            .map(|request| request.body.clone())
            .collect()
    }

    /// Requests the responder rejected with [`Reply::Status`].
    pub fn unexpected(&self) -> Vec<RecordedRequest> {
        self.unexpected.lock().expect("unexpected lock").clone()
    }

    /// Transport-level errors (truncated/chunked/malformed requests, read
    /// timeouts) so they surface in the audit instead of vanishing downstream.
    pub fn transport_errors(&self) -> Vec<String> {
        self.errors.lock().expect("errors lock").clone()
    }
}

impl Drop for MockHttp {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_conn(
    stream: TcpStream,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    unexpected: Arc<Mutex<Vec<RecordedRequest>>>,
    responder: Arc<dyn Fn(&RecordedRequest) -> Reply + Send + Sync>,
) -> std::io::Result<()> {
    // Accepted streams can inherit nonblocking mode from the listener on some
    // platforms. The parser below expects ordinary blocking reads with a
    // timeout, otherwise a partially arrived body may surface as EAGAIN.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    // Request line: `METHOD PATH HTTP/1.1`.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name.to_string(), value.to_string()));
        }
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();

    let request = RecordedRequest {
        method,
        path,
        headers,
        body,
    };
    requests
        .lock()
        .expect("requests lock")
        .push(request.clone());

    match responder(&request) {
        Reply::Sse(sse) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                sse.len()
            );
            writer.write_all(head.as_bytes())?;
            writer.write_all(&sse)?;
        }
        Reply::Json(json) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                json.len()
            );
            writer.write_all(head.as_bytes())?;
            writer.write_all(json.as_bytes())?;
        }
        Reply::Raw { content_type, body } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            writer.write_all(head.as_bytes())?;
            writer.write_all(&body)?;
        }
        Reply::Empty(code) => {
            let head =
                format!("HTTP/1.1 {code} Mock\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            writer.write_all(head.as_bytes())?;
        }
        Reply::Status(code) => {
            unexpected.lock().expect("unexpected lock").push(request);
            let head =
                format!("HTTP/1.1 {code} Mock\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            writer.write_all(head.as_bytes())?;
        }
    }
    writer.flush()?;
    Ok(())
}
