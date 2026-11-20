# Privacy

Altered Audio Editor has no accounts, no sign-in, no telemetry, no analytics, no tracking and no ads. It works fully offline, and nobody has to do anything for you to use every feature.

## What the app does not do

- It never contacts the internet on its own. There is no crash reporter, usage counter or license check. The only exception is the update check below, which you start yourself or turn on yourself. The optional assistant connection listens on this computer only and connects nowhere.
- It does not collect, send or sell anything about you, your computer, your audio or how you use the app.
- It needs no account and never asks you to sign in.

## What stays on your computer

Everything the app saves stays in folders on your own computer:

- Sessions (`.altaudio`) and libraries wherever you save them.
- Settings, keyboard shortcuts, workspaces, autosaves and the waveform cache in the app's own config, data and cache folders.

## The only time the app touches the web

The Help menu has links (The GCRN website, the book, and an optional PayPal page to support the project). They open in your own web browser only when you pick them. The app itself only opens https links on a short allow list, kept in `src-tauri/src/commands/links.rs`.

## Update checks

Help > Check for Updates reads one small file (`latest.json`) from this project's GitHub releases and nothing else. The optional update notice does the same, at every start or at most once a week as you choose, and is off unless you turn it on in Edit > Settings > Privacy. Neither sends anything about you or your audio. GitHub sees a normal web request from your internet address, as with any download. A newer version is only installed when you click Install and Restart, unless you also tick "Install it for me when the program starts" in the same place (off by default), and the installer is checked against the project's signing key first. The portable zip never updates itself.

## Speech models, video and bug reports

The speech models behind the noise and speech tools are built into the app. Nothing is downloaded, and your audio is never sent anywhere to be processed. Video pictures are decoded by your operating system's own decoders, and the files you open are only read from disk.

Help > Report a Bug opens a new GitHub issue in your web browser with details about the app and your system filled in. You can read and change all of it before you submit, and nothing is sent unless you submit it on GitHub.

## The assistant connection

Edit > Settings > Assistant can let an AI assistant on your computer (such as Claude or Codex) read and edit the open session. It is off unless you turn it on.

- When on, the app listens for connections on `127.0.0.1` only, the address that means "this computer". Nothing on your network or the internet can reach it, and the app makes no outgoing connections for it. Every request must carry a token kept in the app's own settings folder; requests from web pages are refused. Backups leave the token out unless you ask for secrets to be included.
- The assistant can only read until you also tick Allow changes. Each change it makes is one undoable step, and Edit > Assistant Activity lists every request.
- The assistant app you connect is someone else's program. It may send what it reads (track names, file names, levels) to its own online service under that service's policy.
- If you put the connection on the internet yourself (a tunnel for ChatGPT, see docs/mcp.md), anyone with the address can control the editor while the tunnel runs. The app never does this for you.

## Plug-ins

VST3 plug-ins you load are made by other companies and may behave differently. Check their own policies if that matters to you.

## For contributors

Changes that add network access, telemetry, accounts or tracking of any kind will not be accepted. The privacy check (`scripts/check-privacy.mjs`, run by `npm run check`) allows sockets in exactly two files, the assistant connection's endpoint and bridge (`src-tauri/src/mcp/server.rs` and `bridge.rs`), and only on `127.0.0.1`. If a future feature truly needs the network (for example, an optional sync with a podcast host you choose), it must be off by default, clearly labeled, and never required to use the app.
