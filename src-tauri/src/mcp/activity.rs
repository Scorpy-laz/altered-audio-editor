// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! What the assistant did: one entry per tool call, kept in memory while
//! the app runs (the newest 500), shown in Settings > Assistant and the
//! Assistant Activity window, and readable by the assistant itself.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

const KEEP: usize = 500;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub seq: u64,
    /// Local time, such as 14:05:31.
    pub time: String,
    pub tool: String,
    /// The arguments, shortened.
    pub args: String,
    pub ok: bool,
    /// The first line of the answer, shortened.
    pub message: String,
}

type Listener = Box<dyn Fn(&Entry) + Send + Sync>;

#[derive(Default)]
pub struct ActivityLog {
    inner: Mutex<(u64, VecDeque<Entry>)>,
    listener: Mutex<Option<Listener>>,
}

/// `text` cut to `max` characters on a character boundary, with an ellipsis.
pub fn shorten(text: &str, max: usize) -> String {
    let one_line = text.lines().next().unwrap_or("").trim();
    if one_line.chars().count() <= max && !text.trim().contains('\n') {
        return one_line.to_string();
    }
    let cut: String = one_line.chars().take(max.saturating_sub(3)).collect();
    format!("{cut}...")
}

impl ActivityLog {
    /// Called with every new entry (the app forwards it to the interface).
    pub fn set_listener(&self, f: impl Fn(&Entry) + Send + Sync + 'static) {
        *self.listener.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    pub fn add(&self, tool: &str, args: &Value, ok: bool, message: &str) -> Entry {
        let entry = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.0 += 1;
            let args = if args.as_object().is_some_and(|o| o.is_empty()) || args.is_null() {
                String::new()
            } else {
                shorten(&args.to_string(), 200)
            };
            let entry = Entry {
                seq: g.0,
                time: chrono::Local::now().format("%H:%M:%S").to_string(),
                tool: tool.to_string(),
                args,
                ok,
                message: shorten(message, 300),
            };
            g.1.push_back(entry.clone());
            while g.1.len() > KEEP {
                g.1.pop_front();
            }
            entry
        };
        if let Some(f) = self
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            f(&entry);
        }
        entry
    }

    /// The newest `limit` entries, oldest first.
    pub fn recent(&self, limit: usize) -> Vec<Entry> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let skip = g.1.len().saturating_sub(limit);
        g.1.iter().skip(skip).cloned().collect()
    }

    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn keeps_the_newest_and_tells_the_listener() {
        let log = ActivityLog::default();
        let seen = Arc::new(AtomicUsize::new(0));
        let s = seen.clone();
        log.set_listener(move |_| {
            s.fetch_add(1, Ordering::SeqCst);
        });
        for i in 0..(KEEP + 5) {
            log.add("get_session", &json!({}), true, &format!("ok {i}"));
        }
        assert_eq!(seen.load(Ordering::SeqCst), KEEP + 5);
        let all = log.recent(10_000);
        assert_eq!(all.len(), KEEP);
        assert_eq!(all.last().unwrap().message, format!("ok {}", KEEP + 4));
        let few = log.recent(2);
        assert_eq!(few.len(), 2);
        assert!(few[0].seq < few[1].seq);
        assert_eq!(few[0].args, "");
        log.clear();
        assert!(log.recent(5).is_empty());
    }

    #[test]
    fn shortens_long_text_to_one_line() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("line one\nline two", 50), "line one...");
        let s = shorten(&"é".repeat(400), 300);
        assert_eq!(s.chars().count(), 300);
        assert!(s.ends_with("..."));
    }
}
