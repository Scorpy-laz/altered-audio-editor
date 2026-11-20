// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * Ready-to-paste settings for assistant apps, built from this program's
 * own path. The stdio forms start this program with `--mcp-stdio`, which
 * reads the port and token from the editor's settings itself, so no token
 * lands in another app's files. The HTTP forms carry the token.
 */

/** The name the editor is listed under in assistant apps. */
export const SERVER_NAME = "altered-audio";

/** Claude Desktop: the `mcpServers` entry for claude_desktop_config.json. */
export function claudeDesktopConfig(exe: string, flag = "--mcp-stdio"): string {
  return JSON.stringify({ mcpServers: { [SERVER_NAME]: { command: exe, args: [flag] } } }, null, 2);
}

/** A TOML string: a literal one when it can be (Windows paths stay readable). */
export function tomlString(s: string): string {
  if (!/['\n\r]/.test(s)) return `'${s}'`;
  return `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r")}"`;
}

/** Codex: the table for ~/.codex/config.toml. Exports can take a while, so the tool timeout is long. */
export function codexConfig(exe: string, flag = "--mcp-stdio"): string {
  return [`[mcp_servers.${SERVER_NAME}]`, `command = ${tomlString(exe)}`, `args = [${tomlString(flag)}]`, "tool_timeout_sec = 1800", ""].join("\n");
}

/** A command-line argument quoted for the shell the person is likely using. */
function shellArg(s: string, windows: boolean): string {
  if (/^[\w./:\\-]+$/.test(s)) return s;
  return windows ? `"${s.replace(/"/g, '\\"')}"` : `'${s.replace(/'/g, "'\\''")}'`;
}

/** Claude Code: the command that adds the editor as a stdio server. */
export function claudeCodeCommand(exe: string, windows: boolean, flag = "--mcp-stdio"): string {
  return `claude mcp add --transport stdio ${SERVER_NAME} -- ${shellArg(exe, windows)} ${flag}`;
}

/** Claude Code over HTTP, with the token in a header (the editor must be running). */
export function claudeCodeHttpCommand(url: string, token: string): string {
  return `claude mcp add --transport http ${SERVER_NAME} ${url} --header "Authorization: Bearer ${token}"`;
}
