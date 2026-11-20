// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The assistant connection's settings, in `assistant.json` in the app
//! config folder: on or off (off by default), the port, the token, and
//! whether the assistant may make changes (off by default). The stdio
//! bridge reads the same file to find the running app, so it also has to
//! find the config folder without Tauri.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "assistant.json";
pub const DEFAULT_PORT: u16 = 47321;
/// Same folder name as `paths::config_dir`.
const SETTINGS_FOLDER: &str = "Altered Audio";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct McpConfig {
    /// The endpoint listens while this is on.
    pub enabled: bool,
    pub port: u16,
    /// The bearer token every request must carry.
    pub token: String,
    /// Tools that change the session or write files are allowed.
    pub allow_edits: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            token: String::new(),
            allow_edits: false,
        }
    }
}

/// A new random token: 64 hex digits (two random v4 UUIDs, 244 random bits).
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Reads the settings; anything missing or unreadable takes its default.
/// A token is made (and saved) when there is none yet.
pub fn load(dir: &Path) -> McpConfig {
    let mut c = read(dir).unwrap_or_default();
    if c.port == 0 {
        c.port = DEFAULT_PORT;
    }
    if c.token.len() < 32 {
        c.token = new_token();
        let _ = save(dir, &c);
    }
    c
}

/// The settings as saved, or None when there is no readable file.
pub fn read(dir: &Path) -> Option<McpConfig> {
    let text = fs::read_to_string(dir.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes the settings through a temporary file, readable only by this
/// user where the system supports it (the file holds the token).
pub fn save(dir: &Path, c: &McpConfig) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(c).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(FILE))
}

/// The app's config folder found the way the app finds it, but without
/// Tauri: portable mode first, then the per-user config area (Roaming on
/// Windows, `~/.config` on Linux, Application Support on macOS).
pub fn config_dir_standalone() -> Option<PathBuf> {
    if let Some(root) = crate::paths::portable_root() {
        return Some(root.join("config"));
    }
    user_config_root().map(|d| d.join(SETTINGS_FOLDER))
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn user_config_root() -> Option<PathBuf> {
    // Some programs start servers with a trimmed environment, so fall back
    // to the profile folder when APPDATA is missing.
    env_dir("APPDATA").or_else(|| env_dir("USERPROFILE").map(|h| h.join("AppData").join("Roaming")))
}

#[cfg(target_os = "macos")]
fn user_config_root() -> Option<PathBuf> {
    env_dir("HOME").map(|h| h.join("Library").join("Application Support"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn user_config_root() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| env_dir("HOME").map(|h| h.join(".config")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_off_with_a_fresh_token_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let c = load(dir.path());
        assert!(!c.enabled && !c.allow_edits);
        assert_eq!(c.port, DEFAULT_PORT);
        assert_eq!(c.token.len(), 64);
        assert!(c.token.chars().all(|ch| ch.is_ascii_hexdigit()));
        // The token was saved, so the bridge finds the same one.
        assert_eq!(load(dir.path()).token, c.token);
        let on = McpConfig {
            enabled: true,
            port: 50000,
            allow_edits: true,
            ..c.clone()
        };
        save(dir.path(), &on).unwrap();
        assert_eq!(read(dir.path()).unwrap(), on);
        assert_ne!(new_token(), new_token());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "only the owner can read the token");
        }
    }

    #[test]
    fn a_damaged_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), "{ not json").unwrap();
        let c = load(dir.path());
        assert!(!c.enabled);
        fs::write(dir.path().join(FILE), r#"{"enabled":true,"port":0}"#).unwrap();
        let c = load(dir.path());
        assert!(c.enabled);
        assert_eq!(c.port, DEFAULT_PORT);
        assert!(!c.allow_edits);
    }
}
