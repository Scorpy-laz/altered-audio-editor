// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The assistant endpoint: MCP Streamable HTTP on 127.0.0.1 only.
//!
//! - Binds the loopback address and nothing else.
//! - Every request needs the token, as `Authorization: Bearer <token>` or
//!   as the last part of the path (`/mcp/<token>`, for clients that cannot
//!   send headers).
//! - A browser `Origin` that is not this computer is refused, so a web page
//!   cannot reach the endpoint through DNS rebinding.
//! - Bodies, headers, connections and waiting time are all capped, and each
//!   connection runs on its own thread, so a misbehaving client can never
//!   hold up the editor.
//!
//! Each POST carries one JSON-RPC message, answered with one JSON object
//! (no event streams are needed: the server sends nothing on its own).

use std::io::{self, BufReader, BufWriter};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use super::base64;
use super::http::{self, ReadError, Request};
use super::protocol::{self, Handler};

/// Connections served at once; more are told to come back later.
const MAX_CONNECTIONS: usize = 16;
/// Requests on one kept-alive connection before it is closed.
const MAX_REQUESTS_PER_CONNECTION: usize = 1000;
/// How long an idle kept-alive connection, or a slow sender, is waited for.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// A running endpoint. Dropping it stops it.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Shared state of a running endpoint.
struct Shared {
    handler: Arc<Handler>,
    token: Arc<RwLock<String>>,
    active: AtomicUsize,
}

impl Server {
    /// Starts listening on 127.0.0.1:`port` (0 picks a free port). The token
    /// is read on every request, so changing it takes effect at once.
    pub fn start(
        port: u16,
        token: Arc<RwLock<String>>,
        handler: Arc<Handler>,
    ) -> io::Result<Server> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            handler,
            token,
            active: AtomicUsize::new(0),
        });
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("assistant-endpoint".into())
            .spawn(move || accept_loop(listener, shared, flag))?;
        Ok(Server {
            addr,
            stop,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Stops accepting and waits for the listening thread. Connections in
    /// progress finish on their own threads.
    pub fn stop(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // Wake the blocking accept.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else { continue };
        // Belt and braces: the listener is on loopback, so peers are too.
        if !stream.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        if shared.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            shared.active.fetch_sub(1, Ordering::SeqCst);
            let mut w = stream;
            let _ = http::write_response(&mut w, 503, &[("Retry-After", "1")], b"", false);
            continue;
        }
        let s = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("assistant-connection".into())
            .spawn(move || {
                serve_connection(stream, &s);
                s.active.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            shared.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn serve_connection(stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = BufWriter::new(stream);
    for _ in 0..MAX_REQUESTS_PER_CONNECTION {
        let req = match http::read_request(&mut reader, &mut writer) {
            Ok(r) => r,
            Err(ReadError::Closed) | Err(ReadError::Io(_)) => return,
            Err(ReadError::TooLarge) => {
                let _ = send(
                    &mut writer,
                    Reply::rpc_error(413, protocol::INVALID_REQUEST, "The request is too large."),
                    false,
                );
                return;
            }
            Err(ReadError::Bad(why)) => {
                let _ = send(
                    &mut writer,
                    Reply::rpc_error(
                        400,
                        protocol::INVALID_REQUEST,
                        &format!("Bad HTTP request: {why}."),
                    ),
                    false,
                );
                return;
            }
        };
        let keep = req.keep_alive();
        let token = shared.token.read().map(|t| t.clone()).unwrap_or_default();
        let reply = route(&req, &token, &shared.handler);
        if send(&mut writer, reply, keep).is_err() || !keep {
            return;
        }
    }
}

/// An HTTP answer.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Option<Value>,
    pub extra: Vec<(&'static str, &'static str)>,
}

impl Reply {
    fn json(status: u16, body: Value) -> Reply {
        Reply {
            status,
            body: Some(body),
            extra: Vec::new(),
        }
    }
    fn rpc_error(status: u16, code: i64, message: &str) -> Reply {
        Reply::json(
            status,
            protocol::error_response(Value::Null, code, message, None),
        )
    }
}

fn send(w: &mut impl io::Write, r: Reply, keep: bool) -> io::Result<()> {
    let body = r
        .body
        .map(|b| serde_json::to_vec(&b).unwrap_or_default())
        .unwrap_or_default();
    let mut headers: Vec<(&str, &str)> = r.extra.clone();
    if !body.is_empty() {
        headers.push(("Content-Type", "application/json"));
    }
    http::write_response(w, r.status, &headers, &body, keep)
}

/// True for an `Origin` on this computer: http or https on localhost,
/// 127.0.0.1 or [::1], any port.
pub fn origin_allowed(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let host = if let Some(v6) = rest.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, tail)) if tail.is_empty() || tail.starts_with(':') => format!("[{h}]"),
            _ => return false,
        }
    } else {
        rest.split(':').next().unwrap_or("").to_string()
    };
    let port_ok = match rest.rsplit_once(':') {
        Some((h, p)) if !h.ends_with('[') && !p.contains(']') => {
            p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty()
        }
        _ => true,
    };
    port_ok
        && matches!(
            host.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "[::1]"
        )
}

/// Compares in time that does not depend on where the first difference is.
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The token a request presented: the bearer header, or the path's last part.
fn presented_token(req: &Request) -> Option<String> {
    if let Some(auth) = req.header("authorization") {
        let (scheme, value) = auth.split_once(' ')?;
        return scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| value.trim().to_string());
    }
    req.path
        .strip_prefix("/mcp/")
        .map(|t| t.trim_end_matches('/').to_string())
}

fn mismatch(id: Value, message: &str) -> Reply {
    Reply::json(
        400,
        protocol::error_response(id, protocol::HEADER_MISMATCH, message, None),
    )
}

/// Answers one HTTP request.
pub fn route(req: &Request, token: &str, handler: &Handler) -> Reply {
    // DNS rebinding: a page in a browser always sends its Origin.
    if let Some(origin) = req.header("origin") {
        if !origin_allowed(origin) {
            return Reply::rpc_error(
                403,
                protocol::INVALID_REQUEST,
                "Requests from web pages are not accepted.",
            );
        }
    }
    if req.path != "/mcp" && req.path != "/mcp/" && !req.path.starts_with("/mcp/") {
        return Reply::rpc_error(404, protocol::INVALID_REQUEST, "The endpoint is /mcp.");
    }
    if !presented_token(req).is_some_and(|t| same_secret(&t, token)) {
        let mut r = Reply::rpc_error(401, protocol::INVALID_REQUEST, "A valid token is needed. Copy it from Altered Audio Editor, Edit > Settings > Assistant.");
        r.extra.push(("WWW-Authenticate", "Bearer"));
        return r;
    }
    if req.method != "POST" {
        let mut r = Reply::rpc_error(
            405,
            protocol::INVALID_REQUEST,
            "Only POST is used here; this server sends no event stream.",
        );
        r.extra.push(("Allow", "POST"));
        return r;
    }
    if let Some(ct) = req.header("content-type") {
        if !ct.to_ascii_lowercase().starts_with("application/json") {
            return Reply::rpc_error(
                415,
                protocol::INVALID_REQUEST,
                "Send JSON (Content-Type: application/json).",
            );
        }
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            return Reply::json(
                400,
                protocol::error_response(
                    Value::Null,
                    protocol::PARSE_ERROR,
                    &format!("Not valid JSON: {e}"),
                    None,
                ),
            )
        }
    };
    if let Some(r) = check_headers(req, &msg) {
        return r;
    }
    match handler.handle(&msg) {
        None => Reply {
            status: 202,
            body: None,
            extra: Vec::new(),
        },
        Some(answer) => {
            let modern = protocol::request_version(&msg).is_some();
            let status = match answer
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64)
            {
                Some(protocol::UNSUPPORTED_VERSION) | Some(protocol::HEADER_MISMATCH) => 400,
                Some(protocol::METHOD_NOT_FOUND) if modern => 404,
                _ => 200,
            };
            Reply::json(status, answer)
        }
    }
}

/// The header rules of the transport: the protocol version header must
/// name a version we serve and agree with the body, and the mirrored
/// `Mcp-Method` and `Mcp-Name` headers must agree with the body (and are
/// required on modern requests).
fn check_headers(req: &Request, msg: &Value) -> Option<Reply> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let header_version = req.header("mcp-protocol-version");
    if let Some(h) = header_version {
        if !protocol::MODERN_VERSIONS.contains(&h) && !protocol::LEGACY_VERSIONS.contains(&h) {
            return Some(Reply::json(400, protocol::unsupported_version(id, h)));
        }
    }
    // Batches (older versions only) and notifications carry no mirrored headers.
    let obj = msg.as_object()?;
    if !obj.contains_key("id") {
        return None;
    }
    let body_version = protocol::request_version(msg);
    let modern = body_version.is_some()
        || header_version.is_some_and(|h| protocol::MODERN_VERSIONS.contains(&h));
    match (header_version, body_version) {
        (Some(h), Some(b)) if h != b => {
            return Some(mismatch(id, &format!("Header mismatch: MCP-Protocol-Version header value '{h}' does not match body value '{b}'")));
        }
        (None, Some(_)) => {
            return Some(mismatch(
                id,
                "Header mismatch: the MCP-Protocol-Version header is required",
            ))
        }
        (Some(h), None) if protocol::MODERN_VERSIONS.contains(&h) => {
            return Some(mismatch(id, &format!("Header mismatch: the body has no _meta protocol version but the header says {h}")));
        }
        _ => {}
    }
    let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
    match req.header("mcp-method") {
        Some(m) if m != method => {
            return Some(mismatch(id, &format!("Header mismatch: Mcp-Method header value '{m}' does not match body value '{method}'")));
        }
        None if modern => {
            return Some(mismatch(
                id,
                "Header mismatch: the Mcp-Method header is required",
            ))
        }
        _ => {}
    }
    if method == "tools/call" {
        let name = msg
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match req.header("mcp-name").map(base64::header_value) {
            Some(None) => {
                return Some(mismatch(
                    id,
                    "Header mismatch: the Mcp-Name header is not valid Base64",
                ))
            }
            Some(Some(h)) if h != name => {
                return Some(mismatch(id, &format!("Header mismatch: Mcp-Name header value '{h}' does not match body value '{name}'")));
            }
            None if modern => {
                return Some(mismatch(
                    id,
                    "Header mismatch: the Mcp-Name header is required",
                ))
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::activity::ActivityLog;
    use crate::mcp::protocol::tests::Echo;
    use serde_json::json;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn handler() -> Handler {
        Handler::new(Arc::new(Echo::default()), Arc::new(ActivityLog::default()))
    }

    fn post(body: Value, headers: &[(&str, &str)]) -> Request {
        let mut h: Vec<(String, String)> = vec![
            ("Authorization".into(), format!("Bearer {TOKEN}")),
            ("Content-Type".into(), "application/json".into()),
        ];
        h.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        Request {
            method: "POST".into(),
            path: "/mcp".into(),
            headers: h,
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn init() -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })
    }

    #[test]
    fn origins_on_this_computer_only() {
        for ok in [
            "http://localhost",
            "http://localhost:5173",
            "https://127.0.0.1:47321",
            "http://[::1]:80",
            "http://LOCALHOST",
        ] {
            assert!(origin_allowed(ok), "{ok}");
        }
        for bad in [
            "null",
            "http://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.nip.io",
            "file://",
            "http://[::1]evil",
            "http://localhost:80x",
            "ws://localhost",
            "",
        ] {
            assert!(!origin_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn needs_the_token_and_a_local_origin() {
        let h = handler();
        assert_eq!(route(&post(init(), &[]), TOKEN, &h).status, 200);
        // Wrong or missing token.
        let mut r = post(init(), &[]);
        r.headers[0].1 = "Bearer nope".into();
        assert_eq!(route(&r, TOKEN, &h).status, 401);
        r.headers.remove(0);
        let reply = route(&r, TOKEN, &h);
        assert_eq!(reply.status, 401);
        assert!(reply.extra.contains(&("WWW-Authenticate", "Bearer")));
        r.headers
            .push(("Authorization".into(), format!("Basic {TOKEN}")));
        assert_eq!(route(&r, TOKEN, &h).status, 401);
        // The token in the path works for clients that cannot send headers.
        let mut r = post(init(), &[]);
        r.headers.remove(0);
        r.path = format!("/mcp/{TOKEN}");
        assert_eq!(route(&r, TOKEN, &h).status, 200);
        r.path = "/mcp/wrong".into();
        assert_eq!(route(&r, TOKEN, &h).status, 401);
        // An empty token never matches.
        assert_eq!(route(&post(init(), &[]), "", &h).status, 401);
        // A web page elsewhere is refused even with the token.
        assert_eq!(
            route(
                &post(init(), &[("Origin", "https://evil.example")]),
                TOKEN,
                &h
            )
            .status,
            403
        );
        assert_eq!(
            route(
                &post(init(), &[("Origin", "http://localhost:3000")]),
                TOKEN,
                &h
            )
            .status,
            200
        );
        // Other paths and methods.
        let mut r = post(init(), &[]);
        r.path = "/".into();
        assert_eq!(route(&r, TOKEN, &h).status, 404);
        let mut r = post(init(), &[]);
        r.method = "GET".into();
        let reply = route(&r, TOKEN, &h);
        assert_eq!(reply.status, 405);
        assert!(reply.extra.contains(&("Allow", "POST")));
    }

    #[test]
    fn malformed_bodies_and_content_types() {
        let h = handler();
        let mut r = post(init(), &[]);
        r.body = b"{nope".to_vec();
        let reply = route(&r, TOKEN, &h);
        assert_eq!(reply.status, 400);
        assert_eq!(reply.body.unwrap()["error"]["code"], protocol::PARSE_ERROR);
        let mut r = post(init(), &[]);
        r.headers[1].1 = "text/plain".into();
        assert_eq!(route(&r, TOKEN, &h).status, 415);
        let r = post(
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            &[],
        );
        let reply = route(&r, TOKEN, &h);
        assert_eq!(reply.status, 202);
        assert!(reply.body.is_none());
    }

    fn modern_call(name: &str) -> Value {
        json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": name, "arguments": {}, "_meta": { protocol::META_VERSION: "2026-07-28" } } })
    }

    #[test]
    fn modern_requests_must_mirror_their_headers() {
        let h = handler();
        let good = [
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "get_session"),
        ];
        let reply = route(&post(modern_call("get_session"), &good), TOKEN, &h);
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body.unwrap()["result"]["resultType"], "complete");
        // Base64 sentinel names are decoded before comparing.
        let enc = [
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "=?base64?Z2V0X3Nlc3Npb24=?="),
        ];
        assert_eq!(
            route(&post(modern_call("get_session"), &enc), TOKEN, &h).status,
            200
        );
        for (k, v, needle) in [
            ("Mcp-Name", "list_markers", "Mcp-Name header value"),
            ("Mcp-Method", "tools/list", "Mcp-Method header value"),
            (
                "MCP-Protocol-Version",
                "2025-06-18",
                "MCP-Protocol-Version header value",
            ),
            ("Mcp-Name", "=?base64?***?=", "not valid Base64"),
        ] {
            let mut hs: Vec<(&str, &str)> =
                good.iter().filter(|(gk, _)| *gk != k).copied().collect();
            hs.push((k, v));
            let reply = route(&post(modern_call("get_session"), &hs), TOKEN, &h);
            assert_eq!(reply.status, 400, "{k}");
            let body = reply.body.unwrap();
            assert_eq!(body["error"]["code"], protocol::HEADER_MISMATCH);
            assert_eq!(body["id"], 5);
            assert!(
                body["error"]["message"].as_str().unwrap().contains(needle),
                "{body}"
            );
        }
        for missing in ["MCP-Protocol-Version", "Mcp-Method", "Mcp-Name"] {
            let hs: Vec<(&str, &str)> = good
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            let reply = route(&post(modern_call("get_session"), &hs), TOKEN, &h);
            assert_eq!(reply.status, 400, "{missing}");
            assert_eq!(
                reply.body.unwrap()["error"]["code"],
                protocol::HEADER_MISMATCH
            );
        }
        // A modern header with a body that has no _meta.
        let reply = route(
            &post(
                json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
                &[
                    ("MCP-Protocol-Version", "2026-07-28"),
                    ("Mcp-Method", "tools/list"),
                ],
            ),
            TOKEN,
            &h,
        );
        assert_eq!(reply.status, 400);
        // An unknown method on a modern request is a 404 with a JSON-RPC error.
        let body = json!({ "jsonrpc": "2.0", "id": 9, "method": "prompts/list", "params": { "_meta": { protocol::META_VERSION: "2026-07-28" } } });
        let reply = route(
            &post(
                body,
                &[
                    ("MCP-Protocol-Version", "2026-07-28"),
                    ("Mcp-Method", "prompts/list"),
                ],
            ),
            TOKEN,
            &h,
        );
        assert_eq!(reply.status, 404);
        assert_eq!(
            reply.body.unwrap()["error"]["code"],
            protocol::METHOD_NOT_FOUND
        );
    }

    #[test]
    fn versions_in_headers_are_checked() {
        let h = handler();
        // Legacy clients without the header are served as before.
        let r = post(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            &[],
        );
        assert_eq!(route(&r, TOKEN, &h).status, 200);
        let r = post(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            &[("MCP-Protocol-Version", "2025-06-18")],
        );
        assert_eq!(route(&r, TOKEN, &h).status, 200);
        let r = post(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            &[("MCP-Protocol-Version", "1999-01-01")],
        );
        let reply = route(&r, TOKEN, &h);
        assert_eq!(reply.status, 400);
        assert_eq!(
            reply.body.unwrap()["error"]["code"],
            protocol::UNSUPPORTED_VERSION
        );
        // A modern request for a version we do not serve.
        let body = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": { "_meta": { protocol::META_VERSION: "2030-01-01" } } });
        let reply = route(
            &post(
                body,
                &[
                    ("MCP-Protocol-Version", "2030-01-01"),
                    ("Mcp-Method", "tools/list"),
                ],
            ),
            TOKEN,
            &h,
        );
        assert_eq!(reply.status, 400);
        assert_eq!(
            reply.body.unwrap()["error"]["data"]["supported"][0],
            "2026-07-28"
        );
    }

    #[test]
    fn binds_loopback_only_and_stops() {
        let tok = Arc::new(RwLock::new(TOKEN.to_string()));
        let mut s = Server::start(0, tok, Arc::new(handler())).unwrap();
        assert_ne!(s.port(), 0);
        assert!(s.addr.ip().is_loopback());
        s.stop();
        // Stopping twice is harmless, and the port is free again.
        s.stop();
        assert!(TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, s.port())).is_ok());
    }
}
