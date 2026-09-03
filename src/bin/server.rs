//! picovolt-server: an HTTP/JSON server around the embedded engine.
//!
//! The engine is single-threaded and not `Send`, so one dedicated thread owns
//! the [`Database`] (it is created on that thread and never crosses a thread
//! boundary). HTTP worker threads accept connections concurrently and hand each
//! request to the engine thread over a channel, receiving the result back; the
//! engine executes statements serially. This serves concurrent clients while
//! leaving the single-threaded core unchanged.
//!
//! Build: `cargo build --release --features server`
//! Run:   `picovolt-server [--addr 127.0.0.1:8080] [--token-file <path>]
//!                         [--memory | --dev <path> | --prod <path>]`
//!
//! Endpoints:
//!   POST /v1/query   {"sql": "...", "params": [...]}  -> query result JSON
//!   GET  /v1/tx                                        -> {"tx": n}
//!   GET  /v1/health                                    -> {"status":"ok"}
//!
//! Loopback use may omit authentication. Non-loopback binds require a bearer
//! token supplied by `--token-file` or `PICOVOLT_SERVER_TOKEN`, and should still
//! run behind a TLS-terminating reverse proxy.

use std::env;
use std::net::IpAddr;
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// How long an HTTP worker waits for the engine before returning 504, so a slow
/// statement cannot block a worker indefinitely.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const QUERY_WAIT_TIMEOUT: Duration = Duration::from_secs(31);
const COMMAND_QUEUE_CAPACITY: usize = 64;
const MAX_QUERY_SCAN_ROWS: usize = 100_000;
const MAX_RESULT_ROWS: usize = 10_000;
const MAX_MATERIALIZED_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

use picovolt::engine::query::{parse, Statement};
use picovolt::{Database, QueryLimits, QueryResult, Value};
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

enum DbConfig {
    Memory,
    Dev(String),
    Prod(String),
}

struct ServerConfig {
    addr: String,
    db: DbConfig,
    token: Option<String>,
}

/// A request handed to the engine thread, with a one-shot reply channel.
enum Command {
    Query {
        sql: String,
        params: Vec<Value>,
        deadline: Instant,
        reply: Sender<Result<serde_json::Value, String>>,
    },
    Tx {
        reply: Sender<u64>,
    },
}

fn main() {
    let config = parse_args();
    let addr = config.addr.clone();
    let token = config.token.map(Arc::<str>::from);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);

    let (tx, rx) = mpsc::sync_channel::<Command>(COMMAND_QUEUE_CAPACITY);

    // The engine thread owns the Database: it is opened here, on this thread,
    // and never moves. Everything else only sends Commands over the channel.
    thread::spawn(move || {
        let mut db = open_db(&config.db);
        for cmd in rx {
            match cmd {
                Command::Query {
                    sql,
                    params,
                    deadline,
                    reply,
                } => {
                    // Catch a panicking statement so one bad query cannot take
                    // down the engine thread (and with it every other client).
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        db.query_with_limits(
                            &sql,
                            &params,
                            QueryLimits::new(
                                MAX_QUERY_SCAN_ROWS,
                                MAX_MATERIALIZED_BYTES,
                                MAX_RESULT_ROWS,
                                Some(deadline),
                            ),
                        )
                    }));
                    let result = match outcome {
                        Ok(Ok(r)) => Ok(result_json(&r)),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(_) => Err("internal error: the statement panicked".to_string()),
                    };
                    let _ = reply.send(result);
                }
                Command::Tx { reply } => {
                    let _ = reply.send(db.current_tx());
                }
            }
        }
    });

    let server = Arc::new(match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("picovolt-server: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    });
    println!("picovolt-server listening on http://{addr}");

    let mut handles = Vec::new();
    for _ in 0..workers {
        let server = Arc::clone(&server);
        let tx = tx.clone();
        let token = token.clone();
        handles.push(thread::spawn(move || {
            for request in server.incoming_requests() {
                handle(request, &tx, token.as_deref());
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}

fn parse_args() -> ServerConfig {
    let mut addr = "127.0.0.1:8080".to_string();
    let mut db = DbConfig::Memory;
    let mut token = env::var("PICOVOLT_SERVER_TOKEN")
        .ok()
        .filter(|value| !value.is_empty());
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => {
                if let Some(a) = args.next() {
                    addr = a;
                }
            }
            "--memory" => db = DbConfig::Memory,
            "--dev" => {
                if let Some(p) = args.next() {
                    db = DbConfig::Dev(p);
                }
            }
            "--prod" => {
                if let Some(p) = args.next() {
                    db = DbConfig::Prod(p);
                }
            }
            "--token-file" => {
                if let Some(path) = args.next() {
                    let value = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                        eprintln!("picovolt-server: could not read token file {path}: {e}");
                        std::process::exit(2);
                    });
                    let value = value.trim().to_string();
                    if value.is_empty() {
                        eprintln!("picovolt-server: token file must not be empty");
                        std::process::exit(2);
                    }
                    token = Some(value);
                }
            }
            "--help" | "-h" => {
                println!("usage: picovolt-server [--addr HOST:PORT] [--token-file PATH] [--memory | --dev PATH | --prod PATH]");
                std::process::exit(0);
            }
            other => {
                eprintln!("picovolt-server: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if !is_loopback_binding(&addr) && token.is_none() {
        eprintln!(
            "picovolt-server: refusing non-loopback bind without --token-file or PICOVOLT_SERVER_TOKEN"
        );
        std::process::exit(2);
    }
    ServerConfig { addr, db, token }
}

fn open_db(config: &DbConfig) -> Database {
    match config {
        DbConfig::Memory => Database::open_memory(),
        DbConfig::Dev(p) => Database::open_dev(p).unwrap_or_else(|e| fatal("open dev", e)),
        DbConfig::Prod(p) => Database::open_prod(p).unwrap_or_else(|e| fatal("open prod", e)),
    }
}

fn fatal(what: &str, e: picovolt::PvError) -> ! {
    eprintln!("picovolt-server: could not {what}: {e}");
    std::process::exit(1)
}

fn handle(request: Request, engine: &SyncSender<Command>, token: Option<&str>) {
    let method = request.method().clone();
    let url = request.url().to_string();
    match (&method, url.as_str()) {
        (Method::Get, "/v1/health") => respond(request, 200, json!({ "status": "ok" })),
        (Method::Get, "/v1/tx") => {
            if !authorized(&request, token) {
                return respond(request, 401, json!({ "error": "unauthorized" }));
            }
            let (reply, rx) = mpsc::channel();
            if let Err(error) = engine.try_send(Command::Tx { reply }) {
                return respond_queue_error(request, error);
            }
            match rx.recv_timeout(QUERY_TIMEOUT) {
                Ok(tx) => respond(request, 200, json!({ "tx": tx })),
                Err(_) => respond(request, 503, json!({ "error": "engine unavailable" })),
            }
        }
        (Method::Post, "/v1/query") => handle_query(request, engine, token),
        _ => respond(request, 404, json!({ "error": "not found" })),
    }
}

fn handle_query(mut request: Request, engine: &SyncSender<Command>, token: Option<&str>) {
    use std::io::Read;
    if !authorized(&request, token) {
        return respond(request, 401, json!({ "error": "unauthorized" }));
    }
    if header_value(&request, "Origin").is_some() {
        return respond(
            request,
            403,
            json!({ "error": "browser cross-origin requests are not accepted" }),
        );
    }
    if !header_value(&request, "Content-Type").is_some_and(is_json_content_type) {
        return respond(
            request,
            415,
            json!({ "error": "Content-Type must be application/json" }),
        );
    }
    // Cap request bodies so a huge POST cannot exhaust memory.
    const MAX_BODY: u64 = 1 << 20; // 1 MiB
    let mut body = String::new();
    if request
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_string(&mut body)
        .is_err()
    {
        return respond(request, 400, json!({ "error": "could not read body" }));
    }
    if body.len() as u64 > MAX_BODY {
        return respond(request, 413, json!({ "error": "request body too large" }));
    }
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return respond(
                request,
                400,
                json!({ "error": format!("invalid JSON: {e}") }),
            )
        }
    };
    let sql = match parsed.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return respond(request, 400, json!({ "error": "missing \"sql\" string" })),
    };
    if is_transaction_control(&sql) {
        return respond(
            request,
            400,
            json!({
                "error": "explicit transactions require a session-bound embedded handle and are not available through /v1/query"
            }),
        );
    }
    let params = match parse_params(parsed.get("params")) {
        Ok(p) => p,
        Err(e) => return respond(request, 400, json!({ "error": e })),
    };

    let (reply, rx) = mpsc::channel();
    let command = Command::Query {
        sql,
        params,
        deadline: Instant::now() + QUERY_TIMEOUT,
        reply,
    };
    if let Err(error) = engine.try_send(command) {
        return respond_queue_error(request, error);
    }
    match rx.recv_timeout(QUERY_WAIT_TIMEOUT) {
        Ok(Ok(result)) => respond(request, 200, result),
        Ok(Err(msg)) => respond(request, 400, json!({ "error": msg })),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            respond(request, 504, json!({ "error": "query timed out" }))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            respond(request, 503, json!({ "error": "engine unavailable" }))
        }
    }
}

fn is_transaction_control(sql: &str) -> bool {
    matches!(
        parse(sql),
        Ok(Statement::Begin | Statement::Commit | Statement::Rollback)
    )
}

fn respond_queue_error(request: Request, error: TrySendError<Command>) {
    match error {
        TrySendError::Full(_) => respond(request, 503, json!({ "error": "server is busy" })),
        TrySendError::Disconnected(_) => {
            respond(request, 503, json!({ "error": "engine unavailable" }))
        }
    }
}

fn header_value<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

fn is_json_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

fn authorized(request: &Request, token: Option<&str>) -> bool {
    let Some(expected) = token else {
        return true;
    };
    let Some(value) = header_value(request, "Authorization") else {
        return false;
    };
    let Some(provided) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(provided.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut different = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        different |= usize::from(a ^ b);
    }
    different == 0
}

fn is_loopback_binding(addr: &str) -> bool {
    let host = match addr.rsplit_once(':') {
        Some((host, _)) => host.trim_matches(['[', ']']),
        None => addr,
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn parse_params(value: Option<&serde_json::Value>) -> Result<Vec<Value>, String> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(arr)) => arr.iter().map(json_to_value).collect(),
        Some(_) => Err("\"params\" must be an array".to_string()),
    }
}

fn json_to_value(v: &serde_json::Value) -> Result<Value, String> {
    use serde_json::Value as J;
    match v {
        J::Null => Ok(Value::Null),
        J::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Int(i))
            } else if n.as_u64().is_some() {
                Err("integer parameter exceeds the i64 range".to_string())
            } else if let Some(f) = n.as_f64() {
                let scaled = f * 1_000_000.0;
                if !scaled.is_finite() || scaled.abs() >= 1.7e38 {
                    Err("numeric parameter out of range".to_string())
                } else {
                    Ok(Value::Decimal(scaled.round() as i128))
                }
            } else {
                Err("numeric parameter out of range".to_string())
            }
        }
        J::String(s) => Ok(Value::Text(s.clone())),
        J::Array(_) | J::Object(_) => {
            Err("array and object parameters are not supported".to_string())
        }
    }
}

fn result_json(result: &QueryResult) -> serde_json::Value {
    match result {
        QueryResult::Rows { columns, rows } => {
            let rows: Vec<Vec<serde_json::Value>> = rows
                .iter()
                .map(|row| row.iter().map(value_json).collect())
                .collect();
            json!({ "columns": columns, "rows": rows })
        }
        QueryResult::Mutated(n) => json!({ "mutated": n }),
        QueryResult::Done => json!({ "done": true }),
    }
}

fn value_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Int(i) => serde_json::Value::from(*i),
        Value::Decimal(_) => serde_json::Value::from(v.to_string()),
        Value::Text(s) => serde_json::Value::from(s.as_str()),
        Value::Blob(b) => serde_json::Value::from(b.clone()),
    }
}

fn respond(request: Request, status: u16, body: serde_json::Value) {
    let mut status = status;
    let mut text = body.to_string();
    if text.len() > MAX_RESPONSE_BYTES {
        status = 413;
        text = json!({ "error": "result exceeds the server response limit" }).to_string();
    }
    let content_type = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let no_store = Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap();
    let no_sniff = Header::from_bytes(&b"X-Content-Type-Options"[..], &b"nosniff"[..]).unwrap();
    let response = Response::from_string(text)
        .with_status_code(status)
        .with_header(content_type)
        .with_header(no_store)
        .with_header(no_sniff);
    let _ = request.respond(response);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_loopback_bindings() {
        assert!(is_loopback_binding("127.0.0.1:8080"));
        assert!(is_loopback_binding("[::1]:8080"));
        assert!(is_loopback_binding("localhost:8080"));
        assert!(!is_loopback_binding("0.0.0.0:8080"));
        assert!(!is_loopback_binding("192.168.1.20:8080"));
    }

    #[test]
    fn validates_content_types_and_tokens() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type("Application/JSON; charset=utf-8"));
        assert!(!is_json_content_type("text/plain"));
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"different"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    #[test]
    fn rejects_sessionless_transaction_control() {
        assert!(is_transaction_control("BEGIN TRANSACTION"));
        assert!(is_transaction_control("commit"));
        assert!(is_transaction_control("ROLLBACK;"));
        assert!(!is_transaction_control("SELECT * FROM users"));
    }
}
