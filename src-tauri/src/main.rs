// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

// Hide the extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Started again as a plug-in host or scanner: see the plughost crate.
    let args: Vec<String> = std::env::args().collect();
    // Started by an assistant as its MCP server: bridge standard input and
    // output to the running editor, and never open a window.
    if args.get(1).map(String::as_str) == Some(altered_audio_editor_lib::mcp::bridge::FLAG) {
        std::process::exit(altered_audio_editor_lib::mcp::bridge::run_stdio(&args[2..]));
    }
    if let Some(code) = altered_plughost::main_with_args(&args) {
        std::process::exit(code);
    }
    altered_audio_editor_lib::run()
}
