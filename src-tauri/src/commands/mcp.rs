// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The app's side of the assistant connection (see `crate::mcp`): starts
//! and stops the endpoint from Settings > Assistant, hands each tool call
//! to the interface as an `assistant-call` event and waits for its
//! `assistant_reply`, and forwards the activity log to the interface.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::mcp::activity::{ActivityLog, Entry};
use crate::mcp::config::{self, McpConfig};
use crate::mcp::protocol::Handler;
use crate::mcp::server::Server;
use crate::mcp::{Executor, ToolOutcome};

/// Tool calls the editor works on at once; more are turned away.
const MAX_IN_FLIGHT: usize = 4;

/// Hands tool calls to the interface and waits for the answers.
#[derive(Default)]
pub struct UiExecutor {
    app: OnceLock<AppHandle>,
    pending: Mutex<HashMap<u64, mpsc::Sender<ToolOutcome>>>,
    next: AtomicU64,
    in_flight: AtomicUsize,
    /// The interface is listening for calls.
    ready: AtomicBool,
    allow_edits: AtomicBool,
}

impl Executor for UiExecutor {
    fn call(&self, tool: &str, args: &Value, timeout: Duration) -> ToolOutcome {
        let Some(app) = self.app.get() else {
            return ToolOutcome::error("The editor is still starting. Ask again in a moment.");
        };
        if !self.ready.load(Ordering::SeqCst) {
            return ToolOutcome::error(
                "The editor window is still starting. Ask again in a moment.",
            );
        }
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return ToolOutcome::error("The editor is busy with other assistant requests. Wait for them to finish, then ask again.");
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let sent = app.emit_to(
            "main",
            "assistant-call",
            json!({ "id": id, "tool": tool, "args": args }),
        );
        let outcome = match sent {
            Err(e) => ToolOutcome::error(format!("Could not reach the editor window: {e}")),
            Ok(()) => rx.recv_timeout(timeout).unwrap_or_else(|_| {
                ToolOutcome::error(format!(
                    "The editor did not answer within {} seconds. It may be busy or showing a dialog. The request may still finish; check get_session before trying again.",
                    timeout.as_secs()
                ))
            }),
        };
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        outcome
    }

    fn edits_allowed(&self) -> bool {
        self.allow_edits.load(Ordering::SeqCst)
    }
}

/// Everything the app keeps about the assistant connection.
pub struct Assistant {
    exec: Arc<UiExecutor>,
    log: Arc<ActivityLog>,
    handler: Arc<Handler>,
    token: Arc<RwLock<String>>,
    config: Mutex<McpConfig>,
    server: Mutex<Option<Server>>,
    /// Why the endpoint is not listening although it is on (port taken).
    problem: Mutex<Option<String>>,
}

impl Default for Assistant {
    fn default() -> Self {
        let exec = Arc::new(UiExecutor::default());
        let log = Arc::new(ActivityLog::default());
        let handler = Arc::new(Handler::new(exec.clone(), log.clone()));
        Self {
            exec,
            log,
            handler,
            token: Arc::new(RwLock::new(String::new())),
            config: Mutex::new(McpConfig::default()),
            server: Mutex::new(None),
            problem: Mutex::new(None),
        }
    }
}

/// What Settings > Assistant shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub token: String,
    pub allow_edits: bool,
    /// Why it is not listening although on.
    pub problem: Option<String>,
    pub url: String,
    /// This program, for the bridge command in client settings.
    pub exe: String,
    pub bridge_flag: String,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Assistant {
    fn status(&self) -> Status {
        let c = lock(&self.config).clone();
        let running = lock(&self.server).as_ref().map(Server::port);
        let port = running.unwrap_or(c.port);
        Status {
            enabled: c.enabled,
            running: running.is_some(),
            port,
            token: c.token.clone(),
            allow_edits: c.allow_edits,
            problem: lock(&self.problem).clone(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            exe: std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            bridge_flag: crate::mcp::bridge::FLAG.to_string(),
        }
    }

    /// Starts or stops the endpoint to match the settings.
    fn apply(&self) {
        let c = lock(&self.config).clone();
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = c.token.clone();
        self.exec.allow_edits.store(c.allow_edits, Ordering::SeqCst);
        let mut server = lock(&self.server);
        let same_port = server.as_ref().is_some_and(|s| s.port() == c.port);
        if !c.enabled || !same_port {
            if let Some(mut s) = server.take() {
                s.stop();
            }
        }
        *lock(&self.problem) = None;
        if c.enabled && server.is_none() {
            match Server::start(c.port, self.token.clone(), self.handler.clone()) {
                Ok(s) => *server = Some(s),
                Err(e) => {
                    *lock(&self.problem) = Some(if e.kind() == std::io::ErrorKind::AddrInUse {
                        format!(
                            "Port {} is in use by another program. Pick another port.",
                            c.port
                        )
                    } else {
                        format!("Could not listen on port {}: {e}", c.port)
                    });
                }
            }
        }
    }
}

/// Reads the settings at launch and starts the endpoint if it was left on.
pub fn init(app: &AppHandle, state: &Assistant) {
    let _ = state.exec.app.set(app.clone());
    let handle = app.clone();
    state.log.set_listener(move |e: &Entry| {
        let _ = handle.emit_to("main", "assistant-activity", e);
    });
    if let Ok(dir) = crate::paths::config_dir(app) {
        *lock(&state.config) = config::load(&dir);
    }
    state.apply();
}

fn save(app: &AppHandle, state: &Assistant) -> Result<(), String> {
    let dir = crate::paths::config_dir(app)?;
    config::save(&dir, &lock(&state.config)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn assistant_status(state: State<'_, Assistant>) -> Status {
    state.status()
}

/// Turns the connection on or off, sets the port and whether changes are allowed.
#[tauri::command]
pub fn assistant_configure(
    app: AppHandle,
    state: State<'_, Assistant>,
    enabled: bool,
    port: u16,
    allow_edits: bool,
) -> Result<Status, String> {
    if port < 1024 {
        return Err("Pick a port from 1024 to 65535.".into());
    }
    {
        let mut c = lock(&state.config);
        c.enabled = enabled;
        c.port = port;
        c.allow_edits = allow_edits;
    }
    save(&app, &state)?;
    state.apply();
    Ok(state.status())
}

/// Makes a new token. Clients set up with the old one stop working.
#[tauri::command]
pub fn assistant_new_token(app: AppHandle, state: State<'_, Assistant>) -> Result<Status, String> {
    lock(&state.config).token = config::new_token();
    save(&app, &state)?;
    state.apply();
    Ok(state.status())
}

/// The interface is listening (true) or has stopped listening (false) for calls.
#[tauri::command]
pub fn assistant_ready(state: State<'_, Assistant>, ready: bool) {
    state.exec.ready.store(ready, Ordering::SeqCst);
}

/// The interface's answer to one `assistant-call`.
#[tauri::command]
pub fn assistant_reply(
    state: State<'_, Assistant>,
    id: u64,
    text: String,
    structured: Option<Value>,
    is_error: bool,
) {
    if let Some(tx) = lock(&state.exec.pending).remove(&id) {
        let _ = tx.send(ToolOutcome {
            text,
            structured,
            is_error,
        });
    }
}

#[tauri::command]
pub fn assistant_activity(state: State<'_, Assistant>, limit: Option<usize>) -> Vec<Entry> {
    state.log.recent(limit.unwrap_or(500))
}

#[tauri::command]
pub fn assistant_clear_activity(state: State<'_, Assistant>) {
    state.log.clear();
}
