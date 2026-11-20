// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * Privacy guard. Fails the build if code that could reach the network,
 * track people or ask for accounts creeps in. See PRIVACY.md.
 *
 * Checks the interface sources, the core sources, the Rust dependency
 * list and the Tauri config. A real need can be discussed in a pull
 * request; it is never added quietly.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
/** @type {string[]} */
const problems = [];

/**
 * @param {string} dir
 * @param {string[]} exts
 * @param {string[]} [out]
 * @returns {string[]}
 */
function walk(dir, exts, out = []) {
  for (const name of readdirSync(dir)) {
    if (name === "node_modules" || name === "target" || name.startsWith(".")) continue;
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, exts, out);
    else if (exts.some((e) => name.endsWith(e)) && !/\.test\.ts$/.test(name)) out.push(p);
  }
  return out;
}

// Interface: no network APIs and no tracking or analytics libraries.
/** @type {[RegExp, string][]} */
const webBanned = [
  [/\bfetch\s*\(/, "fetch()"],
  [/\bXMLHttpRequest\b/, "XMLHttpRequest"],
  [/\bnew\s+WebSocket\b/, "WebSocket"],
  [/\bsendBeacon\s*\(/, "navigator.sendBeacon"],
  [/\bnew\s+EventSource\b/, "EventSource"],
  [/from\s+["'][^"']*(sentry|posthog|mixpanel|amplitude|segment|analytics|telemetry)[^"']*["']/i, "a tracking or analytics library"],
  [/\bgtag\s*\(/, "Google Analytics"],
];
for (const f of walk(join(root, "src"), [".ts", ".svelte"])) {
  const text = readFileSync(f, "utf8");
  for (const [re, what] of webBanned) {
    if (re.test(text)) problems.push(`${relative(root, f)}: uses ${what}`);
  }
}

// Core: no HTTP client crates, no telemetry crates.
const crateBanned = /^\s*(reqwest|ureq|hyper|isahc|surf|attohttpc|curl|tauri-plugin-http|sentry[\w-]*|opentelemetry[\w-]*)\s*=/m;
const cargo = readFileSync(join(root, "src-tauri", "Cargo.toml"), "utf8");
const m = cargo.match(crateBanned);
if (m) problems.push(`src-tauri/Cargo.toml: depends on ${m[1]}`);
// The one exception: the assistant connection (Settings > Assistant, off by
// default) listens on 127.0.0.1 and its stdio bridge connects to 127.0.0.1.
// Only these two files may use sockets, and only on the loopback address:
// every bind and connect must name Ipv4Addr::LOCALHOST, and nothing that
// could reach another machine (other addresses, name lookups, UDP, IPv6
// or "any" addresses) may appear in them.
const LOOPBACK_ONLY = new Set(["src-tauri/src/mcp/server.rs", "src-tauri/src/mcp/bridge.rs"]);
for (const f of walk(join(root, "src-tauri", "src"), [".rs"])) {
  const text = readFileSync(f, "utf8");
  const rel = relative(root, f).replace(/\\/g, "/");
  if (!/std::net::|TcpStream|TcpListener|UdpSocket/.test(text)) continue;
  if (!LOOPBACK_ONLY.has(rel)) {
    problems.push(`${rel}: opens network sockets`);
    continue;
  }
  /** @type {[RegExp, string][]} */
  const remote = [
    [/UdpSocket/, "UDP"],
    [/Ipv6Addr|UNSPECIFIED|0\.0\.0\.0|\[::\]/, "an address other than 127.0.0.1"],
    [/ToSocketAddrs|to_socket_addrs|lookup_host|getaddrinfo/, "a host name lookup"],
    [/\b(?!127\.0\.0\.1\b)\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b/, "an IP address other than 127.0.0.1"],
    [/TcpListener::bind\((?!\s*SocketAddrV4::new\(\s*Ipv4Addr::LOCALHOST\b)/, "a listener not bound to Ipv4Addr::LOCALHOST"],
    [/TcpStream::connect\((?!\s*SocketAddrV4::new\(\s*Ipv4Addr::LOCALHOST\b)/, "a connection not made to Ipv4Addr::LOCALHOST"],
  ];
  for (const [re, what] of remote) {
    if (re.test(text)) problems.push(`${rel}: the assistant connection may only use 127.0.0.1, but this file has ${what}`);
  }
  // connect_timeout takes an address built in the file: it must come from LOCALHOST.
  for (const m of text.matchAll(/TcpStream::connect_timeout\(\s*&?\s*([\w.]+)/g)) {
    const name = m[1] ?? "";
    const built = new RegExp(`let\\s+${name.replace(/\./g, "\\.")}\\s*=\\s*SocketAddr::V4\\(\\s*SocketAddrV4::new\\(\\s*Ipv4Addr::LOCALHOST\\b`);
    const fromListener = /^self\.addr$/.test(name) && /let addr = listener\.local_addr\(\)/.test(text);
    if (!built.test(text) && !fromListener) problems.push(`${rel}: connect_timeout to ${name}, which is not built from Ipv4Addr::LOCALHOST`);
  }
}

// The updater plugin is allowed for one purpose: reading the project's own
// signed latest.json from GitHub releases when the user asks (or has
// turned on the weekly check). It must stay pointed at that one place and
// must keep a public key so only our signed installers are accepted.
const confText = readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8");
const updater = JSON.parse(confText)?.plugins?.updater;
const OWN_RELEASES = /^https:\/\/github\.com\/Scorpy-laz\/altered-audio-editor\/releases\//;
if (updater) {
  const ends = Array.isArray(updater.endpoints) ? updater.endpoints : [];
  if (ends.length === 0 || !ends.every((/** @type {string} */ e) => OWN_RELEASES.test(e))) {
    problems.push("src-tauri/tauri.conf.json: updater endpoints must be this project's GitHub releases only");
  }
  if (typeof updater.pubkey !== "string" || updater.pubkey.length < 40 || /PASTE_PUBLIC_KEY_HERE/.test(updater.pubkey)) {
    problems.push("src-tauri/tauri.conf.json: the updater needs the real public key (npm run tauri signer generate)");
  }
}

// Tauri config: the page may only load the app's own files.
const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
const csp = conf?.app?.security?.csp ?? "";
if (!/default-src 'self'/.test(csp) || /connect-src|https?:/.test(csp)) {
  problems.push("src-tauri/tauri.conf.json: the content security policy must stay default-src 'self' with no remote hosts");
}

if (problems.length) {
  console.error("Privacy check failed. See PRIVACY.md.\n" + problems.map((p) => `  - ${p}`).join("\n"));
  process.exit(1);
}
console.log("Privacy check OK: no network, tracking or account code (the assistant connection keeps to 127.0.0.1).");
