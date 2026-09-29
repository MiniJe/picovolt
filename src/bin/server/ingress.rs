//! Bounded HTTP/1.x ingress for the three server endpoints.
//!
//! Parsing and connection admission happen here, before authentication. Each
//! accepted socket occupies one of a fixed number of workers or a fixed queue;
//! no socket can cause an unbounded parser allocation or create a new thread.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

pub(super) const WORKERS: usize = 32;
const WAITING_CONNECTIONS: usize = 32;
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_REQUEST_LINE_BYTES: usize = 4 * 1024;
const MAX_HEADERS: usize = 64;
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
const BODY_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct Request {
    stream: TcpStream,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body_prefix: Vec<u8>,
    body: BodyKind,
}

enum BodyKind {
    None,
    Length(usize),
    Chunked,
}

pub(super) enum BodyError {
    TooLarge,
    Invalid,
    Timeout,
}

/// Accept and dispatch sockets without ever spawning a thread per connection.
pub(super) fn serve(
    addr: &str,
    handler: impl Fn(Request) + Send + Sync + 'static,
) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let (sender, receiver) = mpsc::sync_channel::<TcpStream>(WAITING_CONNECTIONS);
    let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
    let handler = std::sync::Arc::new(handler);
    for _ in 0..WORKERS {
        let receiver = std::sync::Arc::clone(&receiver);
        let handler = std::sync::Arc::clone(&handler);
        thread::spawn(move || loop {
            let stream = match receiver.lock().expect("ingress queue poisoned").recv() {
                Ok(stream) => stream,
                Err(_) => return,
            };
            if let Some(request) = Request::read(stream) {
                handler(request);
            }
        });
    }
    println!("picovolt-server listening on http://{addr}");
    for accepted in listener.incoming() {
        let stream = accepted?;
        match sender.try_send(stream) {
            Ok(()) => {}
            Err(TrySendError::Full(mut stream)) => {
                // Admission failure is a short, explicit HTTP response. The
                // write timeout prevents this accept loop from stalling.
                let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                let _ = write_response(&mut stream, 503, r#"{"error":"server is busy"}"#);
            }
            Err(TrySendError::Disconnected(_)) => return Ok(()),
        }
    }
    Ok(())
}

impl Request {
    fn read(mut stream: TcpStream) -> Option<Self> {
        let deadline = Instant::now() + HEAD_TIMEOUT;
        let mut bytes = [0u8; MAX_HEAD_BYTES];
        let mut used = 0;
        let head_end = loop {
            if Instant::now() >= deadline {
                reject(&mut stream, 408);
                return None;
            }
            if let Some(end) = bytes[..used].windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
            let line_end = bytes[..used].windows(2).position(|w| w == b"\r\n");
            if line_end.unwrap_or(used) > MAX_REQUEST_LINE_BYTES || used == MAX_HEAD_BYTES {
                reject(&mut stream, 431);
                return None;
            }
            if set_deadline(&stream, deadline).is_err() {
                reject(&mut stream, 408);
                return None;
            }
            match stream.read(&mut bytes[used..]) {
                Ok(0) => return None,
                Ok(n) => used += n,
                Err(e)
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    reject(&mut stream, 408);
                    return None;
                }
                Err(_) => return None,
            }
        };
        if bytes[..head_end]
            .windows(2)
            .position(|w| w == b"\r\n")
            .unwrap_or(head_end)
            > MAX_REQUEST_LINE_BYTES
        {
            reject(&mut stream, 431);
            return None;
        }
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut parsed = httparse::Request::new(&mut headers);
        match parsed.parse(&bytes[..head_end]) {
            Ok(httparse::Status::Complete(_)) => {}
            Err(httparse::Error::TooManyHeaders) => {
                reject(&mut stream, 431);
                return None;
            }
            _ => {
                reject(&mut stream, 400);
                return None;
            }
        }
        if !matches!(parsed.version, Some(0 | 1)) {
            reject(&mut stream, 400);
            return None;
        }
        let mut content_length = None;
        let mut transfer_encoding = None;
        let mut copied = Vec::with_capacity(parsed.headers.len());
        for header in parsed.headers {
            let Ok(value) = std::str::from_utf8(header.value) else {
                reject(&mut stream, 400);
                return None;
            };
            if header.name.eq_ignore_ascii_case("Content-Length")
                && content_length.replace(value.trim()).is_some()
            {
                reject(&mut stream, 400);
                return None;
            }
            if header.name.eq_ignore_ascii_case("Transfer-Encoding")
                && transfer_encoding.replace(value.trim()).is_some()
            {
                reject(&mut stream, 400);
                return None;
            }
            copied.push((header.name.to_string(), value.to_string()));
        }
        let body = match (content_length, transfer_encoding) {
            (Some(_), Some(_)) => None,
            (Some(length), None) => length.parse::<usize>().ok().map(BodyKind::Length),
            (None, Some(encoding)) if encoding.eq_ignore_ascii_case("chunked") => {
                Some(BodyKind::Chunked)
            }
            (None, None) => Some(BodyKind::None),
            _ => None,
        };
        let Some(body) = body else {
            reject(&mut stream, 400);
            return None;
        };
        let request = Self {
            stream,
            method: parsed.method.unwrap_or_default().to_string(),
            url: parsed.path.unwrap_or_default().to_string(),
            headers: copied,
            body_prefix: bytes[head_end..used].to_vec(),
            body,
        };
        Some(request)
    }

    pub(super) fn method(&self) -> &str {
        &self.method
    }

    pub(super) fn url(&self) -> &str {
        &self.url
    }

    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn read_body(&mut self, maximum: usize) -> Result<String, BodyError> {
        if matches!(self.body, BodyKind::Length(length) if length > maximum) {
            return Err(BodyError::TooLarge);
        }
        if self
            .header("Expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
        {
            self.stream
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .map_err(|_| BodyError::Invalid)?;
        }
        let mut reader = DeadlineReader {
            stream: &mut self.stream,
            prefix: &self.body_prefix,
            deadline: Instant::now() + BODY_TIMEOUT,
        };
        let mut bytes = Vec::new();
        let result = match self.body {
            BodyKind::None => Ok(()),
            BodyKind::Length(length) => {
                bytes.reserve(length);
                reader
                    .take(length as u64)
                    .read_to_end(&mut bytes)
                    .map(|_| ())
                    .and_then(|_| {
                        if bytes.len() == length {
                            Ok(())
                        } else {
                            Err(io::ErrorKind::UnexpectedEof.into())
                        }
                    })
            }
            BodyKind::Chunked => {
                let mut decoder = chunked_transfer::Decoder::new(&mut reader);
                decoder
                    .by_ref()
                    .take((maximum + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map(|_| ())
            }
        };
        if let Err(e) = result {
            return Err(
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock {
                    BodyError::Timeout
                } else {
                    BodyError::Invalid
                },
            );
        }
        if bytes.len() > maximum {
            return Err(BodyError::TooLarge);
        }
        String::from_utf8(bytes).map_err(|_| BodyError::Invalid)
    }

    pub(super) fn respond(mut self, status: u16, body: &str) {
        let _ = self.stream.set_write_timeout(Some(Duration::from_secs(5)));
        let _ = write_response(&mut self.stream, status, body);
    }
}

struct DeadlineReader<'a> {
    stream: &'a mut TcpStream,
    prefix: &'a [u8],
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.prefix.is_empty() {
            let count = buf.len().min(self.prefix.len());
            buf[..count].copy_from_slice(&self.prefix[..count]);
            self.prefix = &self.prefix[count..];
            return Ok(count);
        }
        set_deadline(self.stream, self.deadline)?;
        self.stream.read(buf)
    }
}

fn set_deadline(stream: &TcpStream, deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    stream.set_read_timeout(Some(remaining))
}

fn reject(stream: &mut TcpStream, status: u16) {
    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
    let _ = write_response(
        stream,
        status,
        r#"{"error":"invalid or incomplete request"}"#,
    );
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())
}
