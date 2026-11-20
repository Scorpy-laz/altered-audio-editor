// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! JSON-RPC 2.0 messages of the Model Context Protocol, independent of
//! the transport.
//!
//! Two eras of the protocol are served:
//! - Modern (2026-07-28): no handshake. Each request names its version in
//!   `params._meta["io.modelcontextprotocol/protocolVersion"]`, results
//!   carry `resultType`, and `server/discover` lists what is supported.
//! - Legacy (2024-11-05 to 2025-11-25): an `initialize` handshake picks the
//!   version. Requests without the modern `_meta` are served this way.
//!
//! The server keeps no session state, so either era works on any request.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::activity::ActivityLog;
use super::tools;

/// Versions served without a handshake, newest first.
pub const MODERN_VERSIONS: &[&str] = &["2026-07-28"];
/// Versions picked by `initialize`, newest first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

// JSON-RPC and protocol error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// The app is not running, or the assistant connection is off (bridge only).
pub const NOT_REACHABLE: i64 = -32000;
pub const HEADER_MISMATCH: i64 = -32020;
pub const UNSUPPORTED_VERSION: i64 = -32022;

const INSTRUCTIONS: &str = "Altered Audio Editor is a multitrack audio editor for podcasts and voiceover. \
Call get_session first: it gives the ids the other tools take. Times are always in seconds from the session start. \
Effects live in Voice Chains on tracks, clips, buses and the master; list_effects gives each effect's controls. \
Every change you make is one undoable step named \"Assistant: ...\" in the person's History, and every call is shown to them in an activity log. \
Tools that change the session or write files only work while the person has Allow changes turned on in Settings > Assistant.";

/// What a tool call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// Text for the model.
    pub text: String,
    /// The same answer as JSON, when the tool has one.
    pub structured: Option<Value>,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            structured: None,
            is_error: false,
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            structured: None,
            is_error: true,
        }
    }
    /// The MCP `CallToolResult` body.
    pub fn to_result(&self) -> Value {
        let mut r = json!({
            "content": [{ "type": "text", "text": self.text }],
            "isError": self.is_error,
        });
        if let Some(s) = &self.structured {
            r["structuredContent"] = s.clone();
        }
        r
    }
}

/// Runs tool calls that passed the input check. Implemented by the app
/// (hands the call to the interface) and by tests.
pub trait Executor: Send + Sync {
    /// Runs one call and waits for its answer, at most `timeout`.
    fn call(&self, tool: &str, args: &Value, timeout: Duration) -> ToolOutcome;
    /// Whether tools that edit or write files may run now.
    fn edits_allowed(&self) -> bool;
}

/// Answers protocol messages.
pub struct Handler {
    exec: Arc<dyn Executor>,
    log: Arc<ActivityLog>,
}

/// A JSON-RPC error response.
pub fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({ "code": code, "message": message });
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": e })
}

fn server_info() -> Value {
    json!({
        "name": "altered-audio-editor",
        "title": "Altered Audio Editor",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// The protocol version a modern request names in its `_meta`, if any.
pub fn request_version(msg: &Value) -> Option<&str> {
    msg.get("params")?.get("_meta")?.get(META_VERSION)?.as_str()
}

/// True for the error codes that only a modern server returns, so a
/// client can tell this server apart from a legacy one.
pub fn is_modern_error(code: i64) -> bool {
    matches!(code, HEADER_MISMATCH | UNSUPPORTED_VERSION)
}

pub fn unsupported_version(id: Value, requested: &str) -> Value {
    error_response(
        id,
        UNSUPPORTED_VERSION,
        "Unsupported protocol version",
        Some(json!({ "supported": MODERN_VERSIONS, "requested": requested })),
    )
}

impl Handler {
    pub fn new(exec: Arc<dyn Executor>, log: Arc<ActivityLog>) -> Self {
        Self { exec, log }
    }

    pub fn log(&self) -> &Arc<ActivityLog> {
        &self.log
    }

    /// Answers one message. None for notifications and stray responses,
    /// which get no answer. A JSON array (a batch, older versions) gets an
    /// array of answers.
    pub fn handle(&self, msg: &Value) -> Option<Value> {
        if let Value::Array(items) = msg {
            if items.is_empty() {
                return Some(error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    "Empty batch.",
                    None,
                ));
            }
            let out: Vec<Value> = items.iter().filter_map(|m| self.handle_one(m)).collect();
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        self.handle_one(msg)
    }

    fn handle_one(&self, msg: &Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "A message must be a JSON object.",
                None,
            ));
        };
        let id = obj.get("id").cloned();
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\".",
                None,
            ));
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client (we never send requests): nothing to answer.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(error_response(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "The message has no method.",
                None,
            ));
        };
        let Some(id) = id else {
            // Notifications (initialized, cancelled, ...) need no answer.
            return None;
        };
        if !(id.is_string() || id.is_number()) {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "id must be a string or a number.",
                None,
            ));
        }
        let modern = request_version(msg);
        if let Some(v) = modern {
            if !MODERN_VERSIONS.contains(&v) {
                return Some(unsupported_version(id, v));
            }
        }
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "server/discover" => Ok(json!({
                "supportedVersions": MODERN_VERSIONS,
                "capabilities": { "tools": { "listChanged": false } },
                "instructions": INSTRUCTIONS,
                "ttlMs": 3_600_000,
                "cacheScope": "private",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let tools: Vec<Value> = tools::all().iter().map(|t| t.listing.clone()).collect();
                let mut r = json!({ "tools": tools });
                if modern.is_some() {
                    r["ttlMs"] = json!(3_600_000);
                    r["cacheScope"] = json!("private");
                }
                Ok(r)
            }
            "tools/call" => self.call(&params),
            _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
        };
        Some(match result {
            Ok(mut r) => {
                if modern.is_some() || method == "server/discover" {
                    r["resultType"] = json!("complete");
                    r["_meta"] = json!({ META_SERVER_INFO: server_info() });
                }
                json!({ "jsonrpc": "2.0", "id": id, "result": r })
            }
            Err((code, message)) => error_response(id, code, &message, None),
        })
    }

    /// Legacy handshake: the client's version when we have it, else our newest legacy one.
    fn initialize(&self, params: &Value) -> Value {
        let asked = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let version = LEGACY_VERSIONS
            .iter()
            .find(|v| **v == asked)
            .copied()
            .unwrap_or(LEGACY_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": server_info(),
            "instructions": INSTRUCTIONS,
        })
    }

    fn call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "tools/call needs params.name.".to_string()))?;
        let tool = tools::find(name).ok_or((INVALID_PARAMS, format!("Unknown tool: {name}")))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(a @ Value::Object(_)) => a.clone(),
            Some(_) => {
                return Err((
                    INVALID_PARAMS,
                    "arguments must be a JSON object.".to_string(),
                ))
            }
        };
        let outcome = self.run(tool, &args);
        self.log.add(name, &args, !outcome.is_error, &outcome.text);
        Ok(outcome.to_result())
    }

    fn run(&self, tool: &tools::Tool, args: &Value) -> ToolOutcome {
        if let Err(e) = tools::validate(&tool.input_schema, args) {
            return ToolOutcome::error(format!("{e} See the tool's input schema."));
        }
        if tool.edits && !self.exec.edits_allowed() {
            return ToolOutcome::error(
                "Changes are turned off. The person can allow them in Altered Audio Editor under Edit > Settings > Assistant (Allow changes). Reading the session still works.",
            );
        }
        if tool.name == "get_activity_log" {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            let entries = self.log.recent(limit);
            let text = if entries.is_empty() {
                "Nothing yet.".to_string()
            } else {
                entries
                    .iter()
                    .map(|e| {
                        format!(
                            "{} {} {} {}{}",
                            e.time,
                            if e.ok { "ok" } else { "failed" },
                            e.tool,
                            e.args,
                            if e.message.is_empty() {
                                String::new()
                            } else {
                                format!(" -> {}", e.message)
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            return ToolOutcome {
                text,
                structured: Some(json!({ "entries": entries })),
                is_error: false,
            };
        }
        self.exec
            .call(&tool.name, args, Duration::from_secs(tool.timeout_secs))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// Answers every call with its name and arguments.
    #[derive(Default)]
    pub struct Echo {
        pub locked: AtomicBool,
        pub calls: Mutex<Vec<(String, Value)>>,
    }
    impl Executor for Echo {
        fn call(&self, tool: &str, args: &Value, _t: Duration) -> ToolOutcome {
            self.calls
                .lock()
                .unwrap()
                .push((tool.to_string(), args.clone()));
            if tool == "remove_marker" {
                return ToolOutcome::error("There is no marker m9.");
            }
            ToolOutcome {
                text: format!("ran {tool}"),
                structured: Some(json!({ "tool": tool })),
                is_error: false,
            }
        }
        fn edits_allowed(&self) -> bool {
            !self.locked.load(Ordering::SeqCst)
        }
    }

    fn handler() -> (Handler, Arc<Echo>) {
        let e = Arc::new(Echo::default());
        (Handler::new(e.clone(), Arc::new(ActivityLog::default())), e)
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn modern(params: Value) -> Value {
        let mut p = params;
        p["_meta"] = json!({ META_VERSION: "2026-07-28", "io.modelcontextprotocol/clientInfo": { "name": "t", "version": "1" }, "io.modelcontextprotocol/clientCapabilities": {} });
        p
    }

    #[test]
    fn legacy_handshake_negotiates_the_version() {
        let (h, _) = handler();
        let r = h.handle(&req(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "c", "version": "1" } }))).unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "altered-audio-editor");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert!(r["result"].get("resultType").is_none());
        // Unknown (newer or older) versions get our newest legacy one, which the client may reject.
        let r = h
            .handle(&req(
                2,
                "initialize",
                json!({ "protocolVersion": "2099-01-01" }),
            ))
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
        let r = h.handle(&req(3, "initialize", json!({}))).unwrap();
        assert_eq!(r["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
        // The initialized notification gets no answer.
        assert!(h
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
        assert_eq!(
            h.handle(&req(4, "ping", json!({}))).unwrap()["result"],
            json!({})
        );
    }

    #[test]
    fn modern_requests_carry_result_type_and_server_info() {
        let (h, _) = handler();
        let r = h
            .handle(&req(1, "server/discover", modern(json!({}))))
            .unwrap();
        assert_eq!(r["result"]["supportedVersions"], json!(MODERN_VERSIONS));
        assert_eq!(r["result"]["resultType"], "complete");
        assert_eq!(
            r["result"]["_meta"][META_SERVER_INFO]["name"],
            "altered-audio-editor"
        );
        let r = h.handle(&req(2, "tools/list", modern(json!({})))).unwrap();
        assert_eq!(r["result"]["resultType"], "complete");
        assert_eq!(r["result"]["cacheScope"], "private");
        assert!(r["result"]["ttlMs"].as_u64().unwrap() > 0);
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names[0], "get_session");
        assert!(names.contains(&"export_mixdown"));
        // The same list twice, in the same order.
        assert_eq!(
            r["result"]["tools"],
            h.handle(&req(3, "tools/list", json!({}))).unwrap()["result"]["tools"]
        );
    }

    #[test]
    fn rejects_an_unsupported_modern_version() {
        let (h, _) = handler();
        let mut p = modern(json!({}));
        p["_meta"][META_VERSION] = json!("1900-01-01");
        let r = h.handle(&req(7, "tools/list", p)).unwrap();
        assert_eq!(r["error"]["code"], UNSUPPORTED_VERSION);
        assert_eq!(r["error"]["data"]["requested"], "1900-01-01");
        assert_eq!(r["error"]["data"]["supported"][0], "2026-07-28");
        assert_eq!(r["id"], 7);
    }

    #[test]
    fn tool_calls_are_checked_run_and_logged() {
        let (h, e) = handler();
        let call = |id: i64, name: &str, args: Value| {
            h.handle(&req(
                id,
                "tools/call",
                modern(json!({ "name": name, "arguments": args })),
            ))
            .unwrap()
        };
        let r = call(1, "get_chain", json!({ "target": { "kind": "master" } }));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "ran get_chain");
        assert_eq!(r["result"]["structuredContent"]["tool"], "get_chain");
        // Bad input is a tool error the model can fix, and never reaches the editor.
        let r = call(2, "add_effect", json!({ "target": { "kind": "track" } }));
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("`effect` is required"));
        // An error from the editor comes back as a tool error too.
        let r = call(3, "remove_marker", json!({ "marker_ids": ["m9"] }));
        assert_eq!(r["result"]["isError"], true);
        // Unknown tools and bad arguments are protocol errors.
        let r = call(4, "format_disk", json!({}));
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = h
            .handle(&req(
                5,
                "tools/call",
                json!({ "name": "get_session", "arguments": [1] }),
            ))
            .unwrap();
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = h.handle(&req(6, "tools/call", json!({}))).unwrap();
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        // Missing arguments mean none.
        let r = h
            .handle(&req(7, "tools/call", json!({ "name": "get_session" })))
            .unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(e.calls.lock().unwrap().len(), 3);
        // Everything that reached the tool stage is in the log, failures included.
        let log = h.log().recent(100);
        assert_eq!(log.len(), 4);
        assert!(!log[1].ok && log[1].tool == "add_effect");
        let r = call(8, "get_activity_log", json!({ "limit": 2 }));
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("remove_marker") && text.contains("get_session"),
            "{text}"
        );
        assert_eq!(
            r["result"]["structuredContent"]["entries"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn edits_are_refused_while_changes_are_off() {
        let (h, e) = handler();
        e.locked.store(true, Ordering::SeqCst);
        let r = h.handle(&req(1, "tools/call", json!({ "name": "set_mix", "arguments": { "target": { "kind": "master" }, "volume_db": -3 } }))).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Allow changes"));
        let r = h
            .handle(&req(
                2,
                "tools/call",
                json!({ "name": "get_session", "arguments": {} }),
            ))
            .unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(e.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn malformed_messages_get_json_rpc_errors() {
        let (h, _) = handler();
        assert_eq!(
            h.handle(&json!(5)).unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            h.handle(&json!({ "id": 1, "method": "ping" })).unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            h.handle(&json!({ "jsonrpc": "2.0", "id": 1 })).unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            h.handle(&json!({ "jsonrpc": "2.0", "id": {}, "method": "ping" }))
                .unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            h.handle(&req(1, "resources/list", json!({}))).unwrap()["error"]["code"],
            METHOD_NOT_FOUND
        );
        assert!(h
            .handle(&json!({ "jsonrpc": "2.0", "id": 1, "result": {} }))
            .is_none());
        assert_eq!(
            h.handle(&json!([])).unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        let r = h.handle(&json!([req(1, "ping", json!({})), { "jsonrpc": "2.0", "method": "notifications/initialized" }, req(2, "ping", json!({}))])).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
    }
}
