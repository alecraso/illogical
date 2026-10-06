//! `illogical fs ls|stat|cat|watch|recent` and `illogical cd`: files on a
//! host, read-only (M7). A path is this host's (relative to the current
//! directory), `%N:PATH` on whatever pane %N runs on (its VM, say), or
//! `mN:PATH` on machine N; with `--host`, another daemon's.

use std::io::{BufRead, BufReader, Write};

use anyhow::Context;
use clap::Subcommand;
use serde_json::{Value, json};

use crate::http::{Target, enc, request};

#[derive(Subcommand)]
pub enum FsCmd {
    /// A directory's entries.
    Ls {
        /// `PATH`, `%N:PATH` or `mN:PATH` [default: the home directory].
        path: Option<String>,
        /// Only directories.
        #[arg(short, long)]
        dirs: bool,
        /// Size, mode and time too.
        #[arg(short, long)]
        long: bool,
    },
    /// One file or directory (symlinks resolved).
    Stat { path: String },
    /// A file's contents (in 1 MiB ranges; `--offset`/`--len` for part).
    Cat {
        path: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// Bytes [default: to the end].
        #[arg(long)]
        len: Option<u64>,
    },
    /// Changes to a directory or file, as NDJSON, until interrupted.
    Watch { path: String },
    /// Directories used lately on a host (`%N` or `mN`; default this one).
    Recent { on: Option<String> },
}

/// `(query for where, path)`: `%N:p` is pane N's host, `mN:p` machine N.
fn place(spec: &str, remote: bool) -> anyhow::Result<(String, String)> {
    if let Some((on, path)) = spec.split_once(':') {
        if let Some(n) = on.strip_prefix('%') {
            let n: u32 = n.parse().with_context(|| format!("not a pane: {on}"))?;
            return Ok((format!("pane={n}"), path.to_owned()));
        }
        if let Some(n) = on.strip_prefix('m')
            && let Ok(n) = n.parse::<u32>()
        {
            return Ok((format!("machine={n}"), path.to_owned()));
        }
    }
    // Ours: relative to where we are (another daemon's: to its home).
    let path = match std::env::current_dir() {
        Ok(cwd) if !remote && !spec.starts_with('/') && !spec.starts_with('~') => cwd.join(spec).display().to_string(),
        _ => spec.to_owned(),
    };
    Ok((String::new(), path))
}

fn query(on: &str, path: &str, extra: &[String]) -> String {
    let mut q: Vec<String> = Vec::new();
    if !on.is_empty() {
        q.push(on.to_owned());
    }
    q.push(format!("path={}", enc(path)));
    q.extend(extra.iter().cloned());
    q.join("&")
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn kind_char(e: &Value) -> char {
    match (e["type"].as_str(), e["target"].as_str()) {
        (Some("directory"), _) => 'd',
        (Some("symlink"), _) => 'l',
        (Some("file"), _) => '-',
        _ => '?',
    }
}

pub fn run(sock: &Target, cmd: FsCmd, json_out: bool, remote: bool) -> anyhow::Result<i32> {
    match cmd {
        FsCmd::Ls { path, dirs, long } => {
            let (on, path) = place(path.as_deref().unwrap_or("~"), remote)?;
            let extra = if dirs { vec!["dirs=1".to_owned()] } else { vec![] };
            let v = request(sock, "GET", &format!("/api/fs/list?{}", query(&on, &path, &extra)), None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for e in v["entries"].as_array().into_iter().flatten() {
                let name = e["name"].as_str().unwrap_or("?");
                let slash = if e["type"] == "directory" || e["target"] == "directory" { "/" } else { "" };
                if long {
                    let mode = e["mode"].as_u64().unwrap_or(0);
                    println!("{}{mode:04o} {:>12}  {name}{slash}", kind_char(e), e["size"].as_u64().unwrap_or(0));
                } else {
                    println!("{name}{slash}");
                }
            }
            if v["truncated"] == true {
                eprintln!("illogical: (more entries than one listing holds)");
            }
        }
        FsCmd::Stat { path } => {
            let (on, path) = place(&path, remote)?;
            let v = request(sock, "GET", &format!("/api/fs/stat?{}", query(&on, &path, &[])), None)?.json()?;
            if json_out {
                print_json(&v);
            } else {
                let mode = v["mode"].as_u64().unwrap_or(0);
                println!(
                    "{} {}{mode:04o} {} bytes",
                    v["path"].as_str().unwrap_or("?"),
                    kind_char(&v),
                    v["size"].as_u64().unwrap_or(0)
                );
            }
        }
        FsCmd::Cat { path, offset, len } => {
            let (on, path) = place(&path, remote)?;
            let mut out = std::io::stdout().lock();
            let (mut at, end) = (offset, len.map(|l| offset + l));
            loop {
                let want = end.map_or(1 << 20, |e| e.saturating_sub(at).min(1 << 20));
                if want == 0 {
                    break;
                }
                let extra = [format!("offset={at}"), format!("len={want}")];
                let res = request(sock, "GET", &format!("/api/fs/read?{}", query(&on, &path, &extra)), None)?.ok()?;
                let size: u64 = illogical_proto::rename::either(illogical_proto::rename::SIZE, |n| res.header(n))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let bytes = res.bytes()?;
                out.write_all(&bytes)?;
                at += bytes.len() as u64;
                if bytes.is_empty() || at >= size {
                    break;
                }
            }
            out.flush()?;
        }
        FsCmd::Watch { path } => {
            let (on, path) = place(&path, remote)?;
            let res = request(sock, "GET", &format!("/api/fs/watch?{}", query(&on, &path, &[])), None)?.ok()?;
            let mut out = std::io::stdout().lock();
            for line in BufReader::new(res).lines() {
                writeln!(out, "{}", line?)?;
                out.flush()?;
            }
        }
        FsCmd::Recent { on } => {
            let q = match on.as_deref() {
                None => String::new(),
                Some(o) => place(&format!("{o}:"), remote)?.0,
            };
            let v = request(sock, "GET", &format!("/api/fs/recent?{q}"), None)?.json()?;
            if json_out {
                print_json(&v);
            } else {
                for d in v.as_array().into_iter().flatten() {
                    println!("{}", d.as_str().unwrap_or(""));
                }
            }
        }
    }
    Ok(0)
}

/// `illogical cd %N DIR`: typed into pane N's shell if it's idle at its
/// prompt; refused (with why) otherwise.
pub fn cd(sock: &Target, pane: u32, path: &str) -> anyhow::Result<i32> {
    request(sock, "POST", &format!("/api/panes/{pane}/cd"), Some(&json!({ "path": path })))?.json()?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn places() {
        assert_eq!(super::place("%3:src", false).unwrap(), ("pane=3".into(), "src".into()));
        assert_eq!(super::place("m2:/etc", false).unwrap(), ("machine=2".into(), "/etc".into()));
        assert_eq!(super::place("/etc", false).unwrap(), (String::new(), "/etc".into()));
        assert_eq!(super::place("~/x", true).unwrap(), (String::new(), "~/x".into()));
        assert_eq!(super::place("rel", true).unwrap(), (String::new(), "rel".into()));
        let here = std::env::current_dir().unwrap().join("rel").display().to_string();
        assert_eq!(super::place("rel", false).unwrap().1, here);
        // A colon in an ordinary name isn't a place.
        assert_eq!(super::place("/a:b", false).unwrap().1, "/a:b");
    }
}
