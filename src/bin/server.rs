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
//! Run:   `picovolt-server [--addr 127.0.0.1:8080] [--memory | --dev <path> | --prod <path>]`
//!
//! Endpoints:
//!   POST /v1/query   {"sql": "...", "params": [...]}  -> query result JSON
//!   GET  /v1/tx                                        -> {"tx": n}
//!   GET  /v1/health                                    -> {"status":"ok"}
//!
//! There is no authentication or TLS; run it behind a reverse proxy.

use std::env;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;

use picovolt::{Database, QueryResult, Value};
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

enum DbConfig {
    Memory,
    Dev(String),
    Prod(String),
}

/// A request handed to the engine thread, with a one-shot reply channel.
enum Command {
    Query {
        sql: String,
        params: Vec<Value>,
        reply: Sender<Result<serde_json::Value, String>>,
    },
    Tx {
        reply: Sender<u64>,
    },
}

fn main() {
    let (addr, config) = parse_args();

    let (tx, rx) = mpsc::channel::<Command>();

    // The engine thread owns the Database: it is opened here, on this thread,
    // and never moves. Everything else only sends Commands over the channel.
    thread::spawn(move || {
        let mut db = open_db(&config);
        for cmd in rx {
            match cmd {
                Command::Query { sql, params, reply } => {
                    let result = db
                        .query_with(&sql, &params)
                        .map(|r| result_json(&r))
                        .map_err(|e| e.to_string());
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

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let mut handles = Vec::new();
    for _ in 0..workers {
        let server = Arc::clone(&server);
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            for request in server.incoming_requests() {
                handle(request, &tx);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}

fn parse_args() -> (String, DbConfig) {
    let mut addr = "127.0.0.1:8080".to_string();
    let mut config = DbConfig::Memory;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => {
                if let Some(a) = args.next() {
                    addr = a;
                }
            }
            "--memory" => config = DbConfig::Memory,
            "--dev" => {
                if let Some(p) = args.next() {
                    config = DbConfig::Dev(p);
                }
            }
            "--prod" => {
                if let Some(p) = args.next() {
                    config = DbConfig::Prod(p);
                }
            }
            "--help" | "-h" => {
                println!("usage: picovolt-server [--addr HOST:PORT] [--memory | --dev PATH | --prod PATH]");
                std::process::exit(0);
            }
            other => {
                eprintln!("picovolt-server: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    (addr, config)
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

fn handle(request: Request, engine: &Sender<Command>) {
    let method = request.method().clone();
    let url = request.url().to_string();
    match (&method, url.as_str()) {
        (Method::Get, "/v1/health") => respond(request, 200, json!({ "status": "ok" })),
        (Method::Get, "/v1/tx") => {
            let (reply, rx) = mpsc::channel();
            if engine.send(Command::Tx { reply }).is_err() {
                return respond(request, 503, json!({ "error": "engine unavailable" }));
            }
            match rx.recv() {
                Ok(tx) => respond(request, 200, json!({ "tx": tx })),
                Err(_) => respond(request, 503, json!({ "error": "engine unavailable" })),
            }
        }
        (Method::Post, "/v1/query") => handle_query(request, engine),
        _ => respond(request, 404, json!({ "error": "not found" })),
    }
}

fn handle_query(mut request: Request, engine: &Sender<Command>) {
    let mut body = String::new();
    if std::io::Read::read_to_string(request.as_reader(), &mut body).is_err() {
        return respond(request, 400, json!({ "error": "could not read body" }));
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
    let params = match parse_params(parsed.get("params")) {
        Ok(p) => p,
        Err(e) => return respond(request, 400, json!({ "error": e })),
    };

    let (reply, rx) = mpsc::channel();
    if engine.send(Command::Query { sql, params, reply }).is_err() {
        return respond(request, 503, json!({ "error": "engine unavailable" }));
    }
    match rx.recv() {
        Ok(Ok(result)) => respond(request, 200, result),
        Ok(Err(msg)) => respond(request, 400, json!({ "error": msg })),
        Err(_) => respond(request, 503, json!({ "error": "engine unavailable" })),
    }
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
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Decimal((f * 1_000_000.0).round() as i128))
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
    let text = body.to_string();
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let response = Response::from_string(text)
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(response);
}
