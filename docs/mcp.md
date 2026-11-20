# Assistant connection (MCP)

Altered Audio Editor can let an AI assistant on your computer read the open session and, if you allow it, make changes: add and tune Voice Chain effects, set levels, place markers, measure loudness, export. It speaks the Model Context Protocol (MCP), so it works with Claude Desktop, Claude Code, the Codex CLI and other MCP apps.

It is **off until you turn it on**, in Edit > Settings > Assistant.

## How it keeps you in control

- **This computer only.** When on, the editor listens on `127.0.0.1` (port 47321 by default). Other computers on your network cannot reach it. The editor itself still sends nothing over the internet.
- **A token.** Every request must carry the token shown in Settings > Assistant. Web pages are refused outright (their `Origin` is checked), so a site you visit cannot reach the editor either.
- **Read only until you say so.** With Allow changes off, an assistant can read the session and measure loudness, nothing else. Turn Allow changes on to let it edit, play, export and save.
- **One undo per request.** Every change an assistant makes is a single step in History named "Assistant: ..." (for example "Assistant: Add Limiter to Host"). One Ctrl+Z takes it back. The assistant's own `undo` tool only undoes assistant steps, never yours.
- **A log.** Edit > Assistant Activity lists every request, what was asked and what happened. The status bar shows "Assistant on" while the connection is on and lights up as requests arrive; click it for the log.
- **It cannot hang the editor.** The connection runs on its own threads with limits on request size, connections and waiting time. A request the editor cannot answer in time comes back to the assistant as an error.

An assistant app may send what it reads (track names, file paths, levels) to its own online service. That is between you and that service.

## Turn it on

1. Edit > Settings > Assistant.
2. Tick **Allow assistants to connect**. The page shows "Listening at http://127.0.0.1:47321/mcp".
3. Tick **Allow changes** if the assistant should edit, not just read.
4. Use one of the **Copy ... Config** buttons and paste the result into your assistant app, as below. The buttons fill in the real path of the program on your computer.

The setting is remembered: if you leave it on, the connection starts with the editor next time.

## How assistants connect

Most assistant apps start their tools as programs. For them, the editor's own program doubles as a small bridge when started with `--mcp-stdio`: it opens no window, finds the running editor by itself (it reads the port and token from the editor's settings), and passes requests along. So your assistant settings never contain the token.

The assistant app can start before the editor. If a request comes while the editor is closed or the connection is off, the assistant is told so and can ask you to open it.

Apps that speak MCP over HTTP can also connect to `http://127.0.0.1:47321/mcp` directly, with the header `Authorization: Bearer <token>`.

### Claude Desktop (Windows)

1. In Claude Desktop, open Settings > Developer > Edit Config. This opens `%APPDATA%\Claude\claude_desktop_config.json`.
2. Paste what **Copy Claude Desktop Config** gave you. It looks like this. The path below is only an example: the button copies the real location and file name of the program on your computer, which depends on how it was installed (for you only, for all users, or the portable zip).

   ```json
   {
     "mcpServers": {
       "altered-audio": {
         "command": "C:\\Program Files\\Altered Audio Editor\\Altered Audio Editor.exe",
         "args": ["--mcp-stdio"]
       }
     }
   }
   ```

   If the file already has an `mcpServers` section, add the `"altered-audio": { ... }` entry inside it instead of replacing the file. Backslashes in the path are doubled in JSON.
3. Quit Claude Desktop completely and start it again. "altered-audio" appears under the connectors in a new chat.

On a Mac the file is `~/Library/Application Support/Claude/claude_desktop_config.json`; use the path the copy button gives.

### Claude Code

**Copy Claude Code Command** gives a command like this. Run it in a terminal (add `--scope user` to use it in every project):

```
claude mcp add --transport stdio altered-audio -- "C:\Program Files\Altered Audio Editor\Altered Audio Editor.exe" --mcp-stdio
```

Or connect over HTTP while the editor is running:

```
claude mcp add --transport http altered-audio http://127.0.0.1:47321/mcp --header "Authorization: Bearer <token>"
```

### Codex CLI

Codex reads `~/.codex/config.toml` (on Windows, `%USERPROFILE%\.codex\config.toml`). Paste what **Copy Codex Config** gave you:

```toml
[mcp_servers.altered-audio]
command = 'C:\Program Files\Altered Audio Editor\Altered Audio Editor.exe'
args = ['--mcp-stdio']
tool_timeout_sec = 1800
```

The single quotes keep the backslashes as they are. `tool_timeout_sec` is raised from Codex's default of 60 seconds because a long export or loudness measurement can take minutes.

Or over HTTP, with the token kept in an environment variable:

```toml
[mcp_servers.altered-audio]
url = "http://127.0.0.1:47321/mcp"
bearer_token_env_var = "ALTERED_AUDIO_TOKEN"
tool_timeout_sec = 1800
```

### ChatGPT

ChatGPT runs on OpenAI's servers, not on your computer, so it cannot reach `127.0.0.1`. Its custom MCP connectors need either a public HTTPS address or OpenAI's own Secure MCP Tunnel. The editor does not build in any tunnel and never opens itself to the internet. If you want ChatGPT to drive it, you set that up yourself, knowing what it means.

> **Security warning.** A tunnel puts the editor on the internet. Anyone who has the full address can read your session and, with Allow changes on, edit it, export files to your disk and save over sessions, for as long as the tunnel runs. Only do this on your own computer, keep the address private, leave Allow changes off unless you need it, stop the tunnel when you are done, and press **New Token** in Settings > Assistant afterwards.

- **Secure MCP Tunnel (OpenAI).** Where your ChatGPT workspace offers it, OpenAI's `tunnel-client` runs on your computer, connects out to OpenAI, and can start a local MCP program. Point it at the bridge (`"...\Altered Audio Editor.exe" --mcp-stdio` as its MCP command); no inbound port is opened. Follow OpenAI's Secure MCP Tunnel guide for the setup.
- **Your own tunnel.** With a tunnel program you install and run yourself, for example `cloudflared tunnel --url http://127.0.0.1:47321` or `ngrok http 47321`, you get a public `https://...` address. ChatGPT's custom connectors offer OAuth or no authentication, not a fixed token header, so put the token in the address instead: the editor accepts `https://<your-tunnel-address>/mcp/<token>` in place of the header. In ChatGPT, add a custom MCP server with that URL and "No authentication".

## Tools

Times are in seconds from the session start. Ids come from `get_session`; tracks and buses can also be named.

| Tool | Changes? | What it does |
| --- | --- | --- |
| `get_session` | no | Session name, file, rate, length, unsaved state, playhead, selection, tracks with clips and effects, buses, master, markers |
| `list_effects` | no | Built-in effects, VST3 plug-ins and Voice Chain presets; with `effect`, that effect's controls (ranges, units) and presets |
| `get_chain` | no | One Voice Chain (track, clip, bus or master) with every effect's controls |
| `add_effect` | yes | Adds an effect, optionally from one of its presets and with control values |
| `remove_effect` | yes | Removes an effect (by `slot_id` or `index`) |
| `set_effect_params` | yes | Sets controls; values are kept in range and the answer says so |
| `move_effect` | yes | Moves an effect within its chain |
| `set_effect_bypass` | yes | Bypasses an effect, or a whole chain |
| `apply_chain_preset` | yes | Applies a Voice Chain preset such as Podcast Voice |
| `transport` | yes (not in History) | Play, pause, stop, seek, set or clear the time selection |
| `list_markers` | no | Markers in time order |
| `add_marker`, `remove_marker` | yes | Cue, chapter, ad slot and other markers, points or ranges |
| `set_mix` | yes | Volume, pan, mute and solo of a track or bus; Master volume |
| `measure_loudness` | no | Integrated LUFS, range, true peak, as an export renders it |
| `export_mixdown` | yes (not in History) | WAV, FLAC, MP3 or M4A, optional loudness target; never overwrites unless asked |
| `save_session`, `open_session` | yes (not in History) | Save (or save to a new `.altaudio` file); open keeps the current session open behind |
| `import_audio` | yes | Places audio files on a track, or a new track, from a time |
| `undo`, `redo` | yes | Only the assistant's own steps |
| `get_activity_log` | no | What the assistant has done so far |

"Not in History" means the tool does not change the session's content (playing, exporting, saving), so there is nothing to undo. These still need Allow changes and are still in the activity log. Exports from an assistant skip the Export dialog, including its loudness pass/fail card; ask the assistant to run `measure_loudness` first, or use `loudness_lufs`.

## Troubleshooting

- **"Altered Audio Editor is not running..."**: start the editor and check that Settings > Assistant says "Listening".
- **"The assistant connection is off"**: tick Allow assistants to connect.
- **"Changes are turned off"**: tick Allow changes, or ask for something read only.
- **"Port 47321 is in use"**: another program has the port. Type another port, press Apply, and use the copy buttons again (the bridge follows on its own; HTTP set-ups need the new address).
- **Claude Desktop shows no tools**: check the path in the config (doubled backslashes), quit Claude Desktop fully and start it again. Its logs are in `%APPDATA%\Claude\logs` (`mcp-server-altered-audio.log`).
- **A request timed out**: the editor may be showing a dialog (an unsaved-changes question when opening a session). Answer it in the editor.

## For developers

- Transport: MCP Streamable HTTP on `POST /mcp` (or `/mcp/<token>`), one JSON object per response, no event streams. GET and DELETE answer 405. JSON-RPC 2.0.
- Versions: the stateless 2026-07-28 revision (per-request `_meta`, `server/discover`, `Mcp-Method` and `Mcp-Name` header checks, `resultType`), and the handshake revisions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05 through `initialize`. A request without the modern `_meta` and without an `MCP-Protocol-Version` header is served as a handshake-era request.
- Limits: 1 MiB bodies, 16 KiB headers, 16 connections, 30-second idle and write timeouts; at most 4 tool calls in the editor at once.
- Code: `src-tauri/src/mcp/` (protocol, HTTP, endpoint, bridge, settings; no Tauri inside, unit tested), `src-tauri/src/commands/mcp.rs` (the app's glue), `src/app/assistant/` (tool definitions in `tools.json`, shared by both sides, and the dispatcher that runs them through the command stack). Tests: `src-tauri/tests/mcp.rs` drives the endpoint and the real `--mcp-stdio` program over TCP; `src/app/assistant/dispatcher.test.ts` checks that each edit is one undoable step.
- The settings live in `assistant.json` in the app config folder. Backups leave the token out unless secrets are included.
