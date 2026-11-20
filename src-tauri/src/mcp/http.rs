// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Just enough HTTP/1.1 for the assistant endpoint, on the standard
//! library: read one request (headers capped, body by Content-Length or
//! chunked, capped), write one response, and a one-shot POST client for
//! the stdio bridge. Only ever used on the loopback address.

use std::io::{self, BufRead, Read, Write};

/// Most header bytes a request may send.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Largest request body. Tool arguments are small.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    /// The path without any query string.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// Whether the client wants the connection kept open afterwards.
    pub fn keep_alive(&self) -> bool {
        !self
            .header("connection")
            .is_some_and(|c| c.eq_ignore_ascii_case("close"))
    }
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[derive(Debug)]
pub enum ReadError {
    /// The client closed the connection before a new request began.
    Closed,
    /// Headers or body over the limits.
    TooLarge,
    /// Not valid HTTP.
    Bad(&'static str),
    Io(io::Error),
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// Reads one line ending in LF, counting it against `budget`.
fn read_line(r: &mut impl BufRead, budget: &mut usize) -> Result<String, ReadError> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return if line.is_empty() {
                Err(ReadError::Closed)
            } else {
                Err(ReadError::Bad("cut off"))
            };
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..=i], true),
            None => (buf, false),
        };
        if chunk.len() > *budget {
            return Err(ReadError::TooLarge);
        }
        *budget -= chunk.len();
        line.extend_from_slice(chunk);
        let n = chunk.len();
        r.consume(n);
        if done {
            break;
        }
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| ReadError::Bad("not UTF-8"))
}

/// Reads one request. `w` receives the interim `100 Continue` when the
/// client waits for it.
pub fn read_request(r: &mut impl BufRead, w: &mut impl Write) -> Result<Request, ReadError> {
    let mut budget = MAX_HEADER_BYTES;
    // Tolerate blank lines between requests.
    let mut line = read_line(r, &mut budget)?;
    while line.is_empty() {
        line = read_line(r, &mut budget)?;
    }
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().ok_or(ReadError::Bad("no path"))?;
    let version = parts.next().ok_or(ReadError::Bad("no version"))?;
    if !version.starts_with("HTTP/1.")
        || method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
    {
        return Err(ReadError::Bad("not an HTTP/1 request"));
    }
    let path = target.split(['?', '#']).next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let l = read_line(r, &mut budget).map_err(|e| match e {
            ReadError::Closed => ReadError::Bad("cut off"),
            e => e,
        })?;
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').ok_or(ReadError::Bad("bad header"))?;
        if k.is_empty() || k.contains(' ') {
            return Err(ReadError::Bad("bad header name"));
        }
        if headers.len() >= 100 {
            return Err(ReadError::TooLarge);
        }
        headers.push((k.to_string(), v.trim().to_string()));
    }
    let chunked = header(&headers, "transfer-encoding")
        .is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let length = match header(&headers, "content-length") {
        Some(v) => Some(
            v.parse::<usize>()
                .map_err(|_| ReadError::Bad("bad Content-Length"))?,
        ),
        None => None,
    };
    if length.is_some_and(|n| n > MAX_BODY_BYTES) {
        return Err(ReadError::TooLarge);
    }
    if (chunked || length.is_some_and(|n| n > 0))
        && header(&headers, "expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue"))
    {
        w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        w.flush()?;
    }
    let body = if chunked {
        read_chunked(r)?
    } else {
        let mut body = vec![0; length.unwrap_or(0)];
        r.read_exact(&mut body).map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => ReadError::Bad("body cut off"),
            _ => ReadError::Io(e),
        })?;
        body
    };
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn read_chunked(r: &mut impl BufRead) -> Result<Vec<u8>, ReadError> {
    let mut body = Vec::new();
    let mut budget = MAX_HEADER_BYTES;
    loop {
        let line = read_line(r, &mut budget)?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size =
            usize::from_str_radix(size_text, 16).map_err(|_| ReadError::Bad("bad chunk size"))?;
        if size == 0 {
            // Trailers, then the blank line.
            while !read_line(r, &mut budget)?.is_empty() {}
            return Ok(body);
        }
        if body.len() + size > MAX_BODY_BYTES {
            return Err(ReadError::TooLarge);
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])?;
        if !read_line(r, &mut budget)?.is_empty() {
            return Err(ReadError::Bad("bad chunk end"));
        }
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// Writes a complete response.
pub fn write_response(
    w: &mut impl Write,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
    keep_alive: bool,
) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nCache-Control: no-store\r\nConnection: {}\r\n\r\n",
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    ));
    w.write_all(head.as_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// A response read back by the client.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Reads a response (body by Content-Length, chunked, or to the end).
pub fn read_response(r: &mut impl BufRead) -> Result<Response, ReadError> {
    let mut budget = MAX_HEADER_BYTES;
    let mut status;
    let mut headers;
    loop {
        let line = read_line(r, &mut budget)?;
        let mut parts = line.splitn(3, ' ');
        if !parts.next().unwrap_or("").starts_with("HTTP/1.") {
            return Err(ReadError::Bad("not an HTTP response"));
        }
        status = parts
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or(ReadError::Bad("bad status"))?;
        headers = Vec::new();
        loop {
            let l = read_line(r, &mut budget)?;
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                headers.push((k.to_string(), v.trim().to_string()));
            }
        }
        // Skip interim 1xx responses.
        if !(100..200).contains(&status) {
            break;
        }
    }
    let body = if header(&headers, "transfer-encoding")
        .is_some_and(|t| t.to_ascii_lowercase().contains("chunked"))
    {
        read_chunked(r)?
    } else if let Some(n) = header(&headers, "content-length").and_then(|v| v.parse::<usize>().ok())
    {
        if n > MAX_BODY_BYTES * 64 {
            return Err(ReadError::TooLarge);
        }
        let mut b = vec![0; n];
        r.read_exact(&mut b)?;
        b
    } else {
        let mut b = Vec::new();
        r.take((MAX_BODY_BYTES * 64) as u64).read_to_end(&mut b)?;
        b
    };
    Ok(Response {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(raw: &str) -> (Result<Request, ReadError>, Vec<u8>) {
        let mut out = Vec::new();
        let r = read_request(&mut Cursor::new(raw.as_bytes().to_vec()), &mut out);
        (r, out)
    }

    #[test]
    fn reads_a_post_with_a_length() {
        let (r, out) = parse("POST /mcp?x=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nAuthorization: Bearer abc\r\n\r\n{}");
        let r = r.unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/mcp");
        assert_eq!(r.header("AUTHORIZATION"), Some("Bearer abc"));
        assert_eq!(r.body, b"{}");
        assert!(r.keep_alive());
        assert!(out.is_empty());
    }

    #[test]
    fn reads_chunked_bodies_and_answers_expect() {
        let (r, out) = parse("POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n3\r\n{\"a\r\n4;ext=1\r\n\":1}\r\n0\r\n\r\n");
        let r = r.unwrap();
        assert_eq!(r.body, br#"{"a":1}"#);
        assert!(!r.keep_alive());
        assert_eq!(out, b"HTTP/1.1 100 Continue\r\n\r\n");
    }

    #[test]
    fn rejects_oversized_and_malformed_requests() {
        let big = format!(
            "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert!(matches!(parse(&big).0, Err(ReadError::TooLarge)));
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_HEADER_BYTES));
        assert!(matches!(parse(&long).0, Err(ReadError::TooLarge)));
        let chunks = format!(
            "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            MAX_BODY_BYTES + 1
        );
        assert!(matches!(parse(&chunks).0, Err(ReadError::TooLarge)));
        assert!(matches!(parse("hello\r\n\r\n").0, Err(ReadError::Bad(_))));
        assert!(matches!(
            parse("POST /mcp SPDY/3\r\n\r\n").0,
            Err(ReadError::Bad(_))
        ));
        assert!(matches!(
            parse("POST /mcp HTTP/1.1\r\nContent-Length: x\r\n\r\n").0,
            Err(ReadError::Bad(_))
        ));
        assert!(matches!(
            parse("POST /mcp HTTP/1.1\r\nno colon\r\n\r\n").0,
            Err(ReadError::Bad(_))
        ));
        assert!(matches!(
            parse("POST /mcp HTTP/1.1\r\nContent-Length: 10\r\n\r\n{}").0,
            Err(ReadError::Bad(_))
        ));
        assert!(matches!(
            parse("POST /mcp HTTP/1.1\r\nHost: x").0,
            Err(ReadError::Bad(_))
        ));
        assert!(matches!(parse("").0, Err(ReadError::Closed)));
    }

    #[test]
    fn writes_and_reads_back_a_response() {
        let mut out = Vec::new();
        write_response(&mut out, 202, &[("X-Test", "1")], b"", false).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.starts_with("HTTP/1.1 202 Accepted\r\n"));
        assert!(text.contains("Content-Length: 0\r\n") && text.contains("Connection: close\r\n"));
        let mut out = b"HTTP/1.1 100 Continue\r\n\r\n".to_vec();
        write_response(
            &mut out,
            200,
            &[("Content-Type", "application/json")],
            b"{\"ok\":1}",
            true,
        )
        .unwrap();
        let r = read_response(&mut Cursor::new(out)).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(header(&r.headers, "content-type"), Some("application/json"));
        assert_eq!(r.body, b"{\"ok\":1}");
    }
}
