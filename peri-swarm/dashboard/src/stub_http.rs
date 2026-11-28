//! One-request-per-connection HTTP stub for unit tests.
//!
//! Compiled only under `cfg(test)`. The stub records what the client sent and
//! replies from a fixed list; `Connection: close` keeps request/response
//! pairing unambiguous.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One request received by the stub.
#[derive(Clone, Debug)]
pub(crate) struct RecordedRequest {
    /// HTTP method.
    pub(crate) method: String,
    /// Request target including the query string.
    pub(crate) path: String,
    /// Header name/value pairs.
    pub(crate) headers: Vec<(String, String)>,
    /// Body decoded as UTF-8 (tests only send JSON/text).
    pub(crate) body: String,
}

impl RecordedRequest {
    /// Case-insensitive header lookup.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// One canned response.
#[derive(Clone, Debug)]
pub(crate) struct StubResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl StubResponse {
    /// JSON response with `Content-Type: application/json`.
    pub(crate) fn json(status: u16, reason: &'static str, body: &str) -> Self {
        Self {
            status,
            reason,
            headers: vec![("Content-Type", "application/json".to_owned())],
            body: body.to_owned(),
        }
    }

    /// 302 response pointing at `location`.
    pub(crate) fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            reason: "Found",
            headers: vec![("Location", location.to_owned())],
            body: String::new(),
        }
    }
}

/// Stub server bound to an ephemeral loopback port.
pub(crate) struct Stub {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Stub {
    /// Start a stub that answers the queued responses in order.
    pub(crate) fn start(responses: Vec<StubResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("stub addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Accepted sockets inherit O_NONBLOCK on macOS; a
                        // blocking read keeps request/response pairing
                        // deterministic and avoids a WouldBlock race.
                        stream
                            .set_nonblocking(false)
                            .expect("stub connection is blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .expect("stub read timeout");
                        let request = read_request(&mut stream);
                        let index = {
                            let mut recorded = recorded.lock().expect("requests lock");
                            let index = recorded.len();
                            recorded.push(request);
                            index
                        };
                        let response = responses.get(index).cloned().unwrap_or_else(|| {
                            StubResponse::json(500, "Internal Server Error", "")
                        });
                        write_response(&mut stream, &response);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Requests received so far.
    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> RecordedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut data = Vec::new();
    let mut buffer = [0u8; 2048];
    let header_end = loop {
        if let Some(position) = find_headers_end(&data) {
            break position;
        }
        let read = stream.read(&mut buffer).expect("read request");
        if read == 0 {
            break data.len();
        }
        data.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = data.get(header_end + 4..).unwrap_or_default().to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut buffer).expect("read body");
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    body.truncate(content_length.min(body.len()));
    RecordedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn find_headers_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_response(stream: &mut TcpStream, response: &StubResponse) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).expect("write head");
    stream
        .write_all(response.body.as_bytes())
        .expect("write body");
    stream.flush().expect("flush response");
}
