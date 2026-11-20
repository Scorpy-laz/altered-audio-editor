// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The assistant connection (Model Context Protocol). Off by default.
//!
//! When the person turns it on in Settings > Assistant, a small HTTP
//! endpoint listens on 127.0.0.1 only, so an assistant on the same
//! computer (Claude, Codex) can read the session and make edits. Every
//! request needs the bearer token shown in Settings. Each edit the
//! assistant asks for runs in the interface through the normal command
//! stack as one undoable "Assistant: ..." step, and every call is listed
//! in the activity log.
//!
//! Clients that start servers as programs talk to the same endpoint
//! through [`bridge`]: the app started again with `--mcp-stdio`.
//!
//! Nothing here reaches beyond the loopback address. The protocol layer
//! ([`protocol`], [`tools`]) knows nothing about Tauri, so it is tested on
//! its own; [`server`] runs it behind [`http`], and the app supplies the
//! [`Executor`] that hands tool calls to the interface.

pub mod activity;
pub mod base64;
pub mod bridge;
pub mod config;
pub mod http;
pub mod protocol;
pub mod server;
pub mod tools;

pub use protocol::{Executor, ToolOutcome};
