// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The stdio bridge: the app started again as `--mcp-stdio`, for
//! assistants that start their tools as programs and talk over standard
//! input and output (Claude Desktop, Claude Code, Codex).
//!
//! It answers the handshake, discovery and the tool list itself (the tool
//! list is built into this same program, so it always matches the app),
//! and forwards each tool call to the running app's endpoint on
//! 127.0.0.1, with the port and token read from the app's settings at the
//! time of the call. So the assistant can start before the editor: a call
//! made while the editor is closed, or while the connection is off, comes
//! back as a tool error that says what to do.
//!
//! Messages are one JSON object per line. Nothing but protocol messages
//! goes to standard output; notes go to standard error.

use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::activity::ActivityLog;
use super::config;
use super::http;
use super::protocol::{self, Executor, Handler, ToolOutcome};
use super::tools;

/// The flag that starts the bridge instead of the app.
pub const FLAG: &str = "--mcp-stdio";

/// The local handler never runs tools; tool calls are forwarded.
struct NoTools;
impl Executor for NoTools {
    fn call(&self, _tool: &str, _args: &Value, _t: Duration) -> ToolOutcome {
        ToolOutcome::error("Tool calls go to the editor.")
    }
    fn edits_allowed(&self) -> bool {
        false
    }
}

/// Where the bridge finds the app's settings.
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub config_dir: Option<PathBuf>,
}

impl BridgeOptions {
    /// Reads `--config-dir <folder>` from the arguments after the flag.
    pub fn from_args(args: &[String]) -> Self {
        let mut config_dir = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--config-dir" {
                config_dir = it.next().map(PathBuf::from);
            } else if let Some(v) = a.strip_prefix("--config-dir=") {
                config_dir = Some(PathBuf::from(v));
            }
        }
        Self { config_dir }
    }

    fn dir(&self) -> Option<PathBuf> {
        self.config_dir
            .clone()
            .or_else(config::config_dir_standalone)
    }
}

/// Runs the bridge on this process's standard input and output until the
/// client closes its end. Returns the exit code.
pub fn run_stdio(args: &[String]) -> i32 {
    let opts = BridgeOptions::from_args(args);
    let stdin = io::stdin();
    let out = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
    match run(stdin.lock(), out, opts) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Altered Audio Editor bridge: {e}");
            1
        }
    }
}

/// The bridge loop over any reader and writer (tests use pipes).
pub fn run<R: BufRead, W: Write + Send + 'static>(
    input: R,
    out: Arc<Mutex<W>>,
    opts: BridgeOptions,
) -> io::Result<()> {
    let local = Arc::new(Handler::new(
        Arc::new(NoTools),
        Arc::new(ActivityLog::default()),
    ));
    // The version the client picked in its handshake (legacy clients), sent on forwarded calls.
    let negotiated: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut workers = Vec::new();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_line(
                    &out,
                    &protocol::error_response(
                        Value::Null,
                        protocol::PARSE_ERROR,
                        &format!("Not valid JSON: {e}"),
                        None,
                    ),
                )?;
                continue;
            }
        };
        let is_call = msg.get("method").and_then(Value::as_str) == Some("tools/call")
            && msg.get("id").is_some();
        if is_call {
            // Calls can take a while (an export); answer them on their own
            // thread so pings and other requests are not held up.
            let (out, opts, negotiated) = (out.clone(), opts.clone(), negotiated.clone());
            workers.retain(|w: &std::thread::JoinHandle<()>| !w.is_finished());
            workers.push(std::thread::spawn(move || {
                let version = negotiated.lock().ok().and_then(|g| g.clone());
                let answer = forward(&msg, &opts, version.as_deref());
                let _ = write_line(&out, &answer);
            }));
            continue;
        }
        if let Some(answer) = local.handle(&msg) {
            if msg.get("method").and_then(Value::as_str) == Some("initialize") {
                if let Some(v) = answer
                    .pointer("/result/protocolVersion")
                    .and_then(Value::as_str)
                {
                    *negotiated.lock().unwrap_or_else(|e| e.into_inner()) = Some(v.to_string());
                }
            }
            write_line(&out, &answer)?;
        }
    }
    // The client closed its end: let calls in progress finish writing.
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

fn write_line<W: Write>(out: &Mutex<W>, v: &Value) -> io::Result<()> {
    let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::to_writer(&mut *w, v)?;
    w.write_all(b"\n")?;
    w.flush()
}

/// A tool call answered with an error the model can pass on to the person.
fn call_error(id: &Value, text: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": ToolOutcome::error(text).to_result() })
}

/// A header value as the transport wants it: Base64-wrapped when it is not plain ASCII.
fn header_safe(v: &str) -> String {
    let plain = !v.is_empty()
        && v.bytes().all(|b| (0x20..0x7f).contains(&b))
        && !v.starts_with(' ')
        && !v.ends_with(' ')
        && !(v.starts_with("=?base64?") && v.ends_with("?="));
    if plain {
        v.to_string()
    } else {
        format!("=?base64?{}?=", encode_base64(v.as_bytes()))
    }
}

fn encode_base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// Sends one tool call to the running app and returns its answer.
pub fn forward(msg: &Value, opts: &BridgeOptions, negotiated: Option<&str>) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let Some(dir) = opts.dir() else {
        return call_error(&id, "The editor's settings folder could not be found, so the bridge cannot reach Altered Audio Editor.");
    };
    let Some(cfg) = config::read(&dir) else {
        return call_error(&id, "Altered Audio Editor's assistant connection has never been set up. Open Altered Audio Editor, go to Edit > Settings > Assistant and turn on Allow assistants to connect.");
    };
    if !cfg.enabled {
        return call_error(&id, "The assistant connection is off in Altered Audio Editor. The person can turn it on in Edit > Settings > Assistant.");
    }
    let name = msg
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let timeout = tools::find(name).map(|t| t.timeout_secs).unwrap_or(30) + 30;
    let body = serde_json::to_vec(msg).unwrap_or_default();
    let modern = protocol::request_version(msg);
    let version = modern
        .or(negotiated)
        .unwrap_or(protocol::LEGACY_VERSIONS[1]);
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {}", cfg.token)),
        ("MCP-Protocol-Version".to_string(), version.to_string()),
    ];
    if modern.is_some() {
        headers.push(("Mcp-Method".to_string(), "tools/call".to_string()));
        headers.push(("Mcp-Name".to_string(), header_safe(name)));
    }
    match post(cfg.port, &headers, &body, Duration::from_secs(timeout)) {
        Ok(r) if r.status == 200 || r.status == 400 || r.status == 404 => match serde_json::from_slice::<Value>(&r.body) {
            Ok(v) => v,
            Err(_) => call_error(&id, "The editor sent an answer the bridge could not read."),
        },
        Ok(r) if r.status == 401 => call_error(&id, "The editor did not accept the bridge's token. Restart the assistant app so the bridge reads the current token."),
        Ok(r) => call_error(&id, &format!("The editor answered with HTTP {}.", r.status)),
        Err(e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
            call_error(&id, "The editor did not answer in time. It may be busy or showing a dialog; check it, then ask again.")
        }
        Err(_) => call_error(&id, &format!("Altered Audio Editor is not running, or its assistant connection is not listening on port {}. Start the editor, check Edit > Settings > Assistant, then ask again.", cfg.port)),
    }
}

/// One POST to the app's endpoint on 127.0.0.1.
fn post(
    port: u16,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Duration,
) -> io::Result<http::Response> {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut w = BufWriter::new(stream.try_clone()?);
    let mut head = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.write_all(body)?;
    w.flush()?;
    drop(w);
    let mut r = BufReader::new(stream);
    http::read_response(&mut r).map_err(|e| match e {
        http::ReadError::Io(e) => e,
        other => io::Error::other(format!("{other:?}")),
    })
}

/// The command line an assistant should run for the bridge: this program and the flag.
pub fn command_for(exe: &Path) -> (String, Vec<String>) {
    (exe.to_string_lossy().into_owned(), vec![FLAG.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_config_dir_flag() {
        let o = BridgeOptions::from_args(&["--config-dir".into(), "/tmp/x".into()]);
        assert_eq!(o.config_dir.unwrap(), PathBuf::from("/tmp/x"));
        let o = BridgeOptions::from_args(&["--config-dir=/y".into()]);
        assert_eq!(o.config_dir.unwrap(), PathBuf::from("/y"));
        assert!(BridgeOptions::from_args(&[]).config_dir.is_none());
    }

    #[test]
    fn encodes_unsafe_header_values() {
        assert_eq!(header_safe("get_session"), "get_session");
        assert_eq!(
            header_safe("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        assert_eq!(header_safe(" padded "), "=?base64?IHBhZGRlZCA=?=");
        assert_eq!(
            super::super::base64::header_value(&header_safe("=?base64?literal?=")).unwrap(),
            "=?base64?literal?="
        );
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn answers_locally_and_explains_when_the_editor_is_away() {
        let dir = tempfile::tempdir().unwrap();
        let opts = BridgeOptions {
            config_dir: Some(dir.path().to_path_buf()),
        };
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "",
            "not json",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_session","arguments":{}}}"#,
        ]
        .join("\n");
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        run(io::Cursor::new(input), out.clone(), opts.clone()).unwrap();
        let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert_eq!(lines[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(lines[1]["error"]["code"], protocol::PARSE_ERROR);
        assert!(lines[2]["result"]["tools"].as_array().unwrap().len() >= 20);
        assert_eq!(lines[3]["id"], 3);
        assert_eq!(lines[3]["result"]["isError"], true);
        assert!(lines[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("never been set up"));
        // Off in the settings.
        config::save(
            dir.path(),
            &config::McpConfig {
                token: config::new_token(),
                ..Default::default()
            },
        )
        .unwrap();
        let call =
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_session"}});
        let r = forward(&call, &opts, None);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("is off"));
        // On, but nothing listening on the port.
        let free = std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        config::save(
            dir.path(),
            &config::McpConfig {
                enabled: true,
                port: free,
                token: config::new_token(),
                allow_edits: false,
            },
        )
        .unwrap();
        let r = forward(&call, &opts, None);
        assert_eq!(r["result"]["isError"], true);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not running"),
            "{r}"
        );
    }
}
