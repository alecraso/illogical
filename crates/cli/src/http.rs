//! Just enough HTTP/1.1 over the daemon's Unix socket: one request per
//! connection, with fixed-length or chunked (streamed) responses.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
};

use anyhow::{Context, bail};

pub struct Response {
    pub status: u16,
    reader: BufReader<UnixStream>,
    chunked: bool,
    length: Option<usize>,
    chunk_left: usize,
    done: bool,
}

pub fn request(socket: &Path, method: &str, path: &str, body: Option<&serde_json::Value>) -> anyhow::Result<Response> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("can't reach illogicald at {} (is it running?)", socket.display()))?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).context("bad HTTP response")?;
    let (mut chunked, mut length) = (false, None);
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').unwrap_or((l, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "content-length" => length = v.trim().parse().ok(),
            _ => {}
        }
    }
    Ok(Response { status, reader, chunked, length, chunk_left: 0, done: false })
}

impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if !self.chunked {
            let want = match self.length {
                Some(0) => return Ok(0),
                Some(n) => buf.len().min(n),
                None => buf.len(),
            };
            let n = self.reader.read(&mut buf[..want])?;
            if let Some(len) = &mut self.length {
                *len -= n;
            }
            return Ok(n);
        }
        if self.chunk_left == 0 {
            let mut size = String::new();
            self.reader.read_line(&mut size)?;
            if size.trim().is_empty() {
                // The CRLF after a chunk.
                size.clear();
                self.reader.read_line(&mut size)?;
            }
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
            if n == 0 {
                self.done = true;
                return Ok(0);
            }
            self.chunk_left = n;
        }
        let want = buf.len().min(self.chunk_left);
        let n = self.reader.read(&mut buf[..want])?;
        self.chunk_left -= n;
        Ok(n)
    }
}

impl Response {
    pub fn text(mut self) -> anyhow::Result<String> {
        let mut s = String::new();
        self.read_to_string(&mut s)?;
        Ok(s)
    }

    /// The body as JSON, or the API's error as an error.
    pub fn json(self) -> anyhow::Result<serde_json::Value> {
        let status = self.status;
        let text = self.text()?;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
        if !(200..300).contains(&status) {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(&v.to_string()));
        }
        Ok(v)
    }

    /// Fail with the API's error message on a non-2xx status.
    pub fn ok(self) -> anyhow::Result<Self> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        let status = self.status;
        let text = self.text()?;
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
            .unwrap_or(text);
        bail!("{msg} (HTTP {status})")
    }
}

/// Percent-encode a query value.
pub fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
