// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Standard Base64 decoding, for the `=?base64?...?=` header values the
//! protocol allows. Small enough not to need a crate.

/// Decodes standard Base64 (with or without `=` padding). None when the
/// text is not valid Base64.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = text.trim_end_matches('=').as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i as u32);
        }
        let n = chunk.len() - 1;
        for i in 0..n {
            out.push((acc >> (16 - 8 * i as u32)) as u8);
        }
    }
    Some(out)
}

/// A header value as the protocol means it: `=?base64?X?=` decoded, other
/// values as they are. None for a malformed encoded value.
pub fn header_value(raw: &str) -> Option<String> {
    match raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(enc) => String::from_utf8(decode(enc)?).ok(),
        None => Some(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_protocol_examples() {
        assert_eq!(
            header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, 世界")
        );
        assert_eq!(
            header_value("=?base64?IHBhZGRlZCA=?=").as_deref(),
            Some(" padded ")
        );
        assert_eq!(header_value("us-west1").as_deref(), Some("us-west1"));
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode("Zm9vYg").unwrap(), b"foob");
        assert!(decode("Zm9v!").is_none());
        assert!(header_value("=?base64?%%%?=").is_none());
    }
}
