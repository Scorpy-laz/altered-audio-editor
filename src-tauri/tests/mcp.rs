// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The assistant connection end to end, over real sockets: the endpoint on
//! an ephemeral loopback port with a stand-in editor, driven through the
//! legacy handshake and the modern stateless form; then the stdio bridge
//! (in process, and as the real program started with `--mcp-stdio`)
//! talking to it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use altered_audio_editor_lib::mcp::activity::ActivityLog;
use altered_audio_editor_lib::mcp::bridge::{self, BridgeOptions};
use altered_audio_editor_lib::mcp::config::{self, McpConfig};
use altered_audio_editor_lib::mcp::protocol::Handler;
use altered_audio_editor_lib::mcp::server::Server;
use altered_audio_editor_lib::mcp::{Executor, ToolOutcome};
use serde_json::{json, Value};

/// A stand-in for the editor: knows one track and records what it was asked.
#[derive(Default)]
struct FakeEditor {
    calls: Mutex<Vec<(String, Value)>>,
}

impl Executor for FakeEditor {
    fn call(&self, tool: &str, args: &Value, _timeout: Duration) -> ToolOutcome {
        self.calls
            .lock()
            .unwrap()
            .push((tool.to_string(), args.clone()));
        match tool {
            "get_session" => ToolOutcome {
                text: r#"{"name":"Episode 12","tracks":[{"id":"t1","name":"Host"}]}"#.into(),
                structured: Some(
                    json!({ "name": "Episode 12", "tracks": [{ "id": "t1", "name": "Host" }] }),
                ),
                is_error: false,
            },
            "add_effect" => ToolOutcome::ok("Added Limiter to Host at index 0 (slot_id s1)."),
            _ => ToolOutcome::error(format!("{tool} is not part of this test.")),
        }
    }
    fn edits_allowed(&self) -> bool {
        true
    }
}

const TOKEN: &str = "test-token-0123456789abcdef0123456789";

fn start() -> (Server, Arc<FakeEditor>, Arc<ActivityLog>) {
    let editor = Arc::new(FakeEditor::default());
    let log = Arc::new(ActivityLog::default());
    let handler = Arc::new(Handler::new(editor.clone(), log.clone()));
    let server = Server::start(0, Arc::new(RwLock::new(TOKEN.into())), handler).unwrap();
    (server, editor, log)
}

/// A small HTTP client on one kept-alive connection.
struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Client {
    fn connect(port: u16) -> Client {
        let s = TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Client {
            reader: BufReader::new(s.try_clone().unwrap()),
            writer: s,
        }
    }

    fn post(&mut self, body: &Value, headers: &[(&str, &str)]) -> (u16, Option<Value>) {
        let text = body.to_string();
        let mut req = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n", text.len());
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(&text);
        self.writer.write_all(req.as_bytes()).unwrap();
        self.read()
    }

    fn read(&mut self) -> (u16, Option<Value>) {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let status: u16 = line.split(' ').nth(1).unwrap().parse().unwrap();
        let mut len = 0;
        loop {
            let mut h = String::new();
            self.reader.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        self.reader.read_exact(&mut body).unwrap();
        (
            status,
            (!body.is_empty()).then(|| serde_json::from_slice(&body).unwrap()),
        )
    }
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

#[test]
fn legacy_client_handshakes_lists_and_calls_over_tcp() {
    let (server, editor, log) = start();
    let mut c = Client::connect(server.port());
    let auth = bearer();
    let (status, body) = c.post(
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } }),
        &[("Authorization", &auth)],
    );
    assert_eq!(status, 200);
    let body = body.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(body["result"]["serverInfo"]["name"], "altered-audio-editor");

    let v = [
        ("Authorization", auth.as_str()),
        ("MCP-Protocol-Version", "2025-06-18"),
    ];
    let (status, body) = c.post(
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        &v,
    );
    assert_eq!((status, body), (202, None));

    let (status, body) = c.post(
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        &v,
    );
    assert_eq!(status, 200);
    let tools = body.unwrap()["result"]["tools"].as_array().unwrap().clone();
    let add = tools.iter().find(|t| t["name"] == "add_effect").unwrap();
    assert_eq!(
        add["inputSchema"]["properties"]["target"]["properties"]["kind"]["enum"][0],
        "track"
    );
    assert_eq!(add["annotations"]["readOnlyHint"], false);

    let (status, body) = c.post(
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "add_effect", "arguments": { "target": { "kind": "track", "id": "Host" }, "effect": "altaudio.limiter", "params": { "ceiling": -3 } } } }),
        &v,
    );
    assert_eq!(status, 200);
    let body = body.unwrap();
    assert_eq!(body["result"]["isError"], false);
    assert!(body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("Added Limiter"));

    // Bad input is answered by the endpoint itself, as a tool error.
    let (_, body) = c.post(&json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "set_mix", "arguments": { "target": { "kind": "track", "id": "t1" }, "volume_db": 40 } } }), &v);
    assert_eq!(body.unwrap()["result"]["isError"], true);

    let calls = editor.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "add_effect");
    assert_eq!(calls[0].1["params"]["ceiling"], -3);
    assert_eq!(log.recent(10).len(), 2);
}

#[test]
fn modern_client_calls_without_a_handshake() {
    let (server, _editor, _log) = start();
    let mut c = Client::connect(server.port());
    let auth = bearer();
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": { "name": "t", "version": "1" }, "io.modelcontextprotocol/clientCapabilities": {} });
    let (status, body) = c.post(
        &json!({ "jsonrpc": "2.0", "id": "d", "method": "server/discover", "params": { "_meta": meta } }),
        &[("Authorization", &auth), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "server/discover")],
    );
    assert_eq!(status, 200);
    assert_eq!(
        body.unwrap()["result"]["supportedVersions"][0],
        "2026-07-28"
    );
    let (status, body) = c.post(
        &json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "get_session", "arguments": {}, "_meta": meta } }),
        &[("Authorization", &auth), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/call"), ("Mcp-Name", "get_session")],
    );
    assert_eq!(status, 200);
    let body = body.unwrap();
    assert_eq!(body["result"]["resultType"], "complete");
    assert_eq!(
        body["result"]["structuredContent"]["tracks"][0]["name"],
        "Host"
    );
}

#[test]
fn refuses_strangers_without_disturbing_the_connection_limit() {
    let (server, editor, _log) = start();
    let mut c = Client::connect(server.port());
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    assert_eq!(c.post(&init, &[]).0, 401);
    assert_eq!(c.post(&init, &[("Authorization", "Bearer wrong")]).0, 401);
    let auth = bearer();
    assert_eq!(
        c.post(
            &init,
            &[
                ("Authorization", &auth),
                ("Origin", "http://attacker.example")
            ]
        )
        .0,
        403
    );
    // Garbage that is not HTTP closes only that connection.
    let mut raw =
        TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, server.port())).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    raw.write_all(b"\x16\x03\x01 not http at all\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    let _ = raw.read_to_string(&mut answer);
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    // Many idle connections at once are turned away past the limit, and the endpoint still answers afterwards.
    let idle: Vec<TcpStream> = (0..20)
        .map(|_| TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, server.port())).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    drop(idle);
    std::thread::sleep(Duration::from_millis(200));
    let mut c2 = Client::connect(server.port());
    assert_eq!(c2.post(&init, &[("Authorization", &auth)]).0, 200);
    assert!(editor.calls.lock().unwrap().is_empty());
}

fn bridge_config(port: u16) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    config::save(
        dir.path(),
        &McpConfig {
            enabled: true,
            port,
            token: TOKEN.into(),
            allow_edits: true,
        },
    )
    .unwrap();
    dir
}

fn lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const SESSION: [&str; 4] = [
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_session","arguments":{}}}"#,
];

fn check_bridge_answers(out: &[Value]) {
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
    assert!(out[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "export_mixdown"));
    assert_eq!(out[2]["id"], 3);
    assert_eq!(out[2]["result"]["isError"], false, "{}", out[2]);
    assert_eq!(out[2]["result"]["structuredContent"]["name"], "Episode 12");
}

#[test]
fn the_stdio_bridge_forwards_tool_calls_to_the_endpoint() {
    let (server, editor, _log) = start();
    let dir = bridge_config(server.port());
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    bridge::run(
        std::io::Cursor::new(SESSION.join("\n")),
        out.clone(),
        BridgeOptions {
            config_dir: Some(dir.path().into()),
        },
    )
    .unwrap();
    let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    check_bridge_answers(&lines(&text));
    assert_eq!(editor.calls.lock().unwrap()[0].0, "get_session");
    // A modern call through the bridge sends the mirrored headers the endpoint checks.
    let call = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "get_session", "arguments": {}, "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } });
    let r = bridge::forward(
        &call,
        &BridgeOptions {
            config_dir: Some(dir.path().into()),
        },
        None,
    );
    assert_eq!(r["result"]["resultType"], "complete", "{r}");
    // A stale token gets a plain explanation.
    config::save(
        dir.path(),
        &McpConfig {
            enabled: true,
            port: server.port(),
            token: "something-else-entirely-0123456789".into(),
            allow_edits: true,
        },
    )
    .unwrap();
    let r = bridge::forward(
        &call,
        &BridgeOptions {
            config_dir: Some(dir.path().into()),
        },
        None,
    );
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("token"),
        "{r}"
    );
}

#[test]
fn the_real_program_bridges_with_mcp_stdio() {
    let (server, editor, _log) = start();
    let dir = bridge_config(server.port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_altered-audio-editor"))
        .arg("--mcp-stdio")
        .arg("--config-dir")
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for l in SESSION {
            writeln!(stdin, "{l}").unwrap();
        }
        // Closing standard input ends the bridge once its calls are answered.
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    check_bridge_answers(&lines(&String::from_utf8(out.stdout).unwrap()));
    assert_eq!(editor.calls.lock().unwrap().len(), 1);
}
