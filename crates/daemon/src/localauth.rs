//! Who on this machine may use the daemon's TCP port.
//!
//! Loopback is shared by every account and every program on the machine,
//! so a connection from it says nothing about who is asking. The Unix
//! socket does (its directory is the owner's, 0700); TCP needs a secret:
//!
//! - **The local token**, `local-token` in the state directory (0600,
//!   made at the first start and kept, so browsers stay signed in across
//!   restarts). Anything on loopback that tailscaled didn't vouch for shows
//!   it, as `Authorization: Bearer …` (programs) or as a cookie (browsers).
//! - **Browsers get the cookie from a sign-in link**,
//!   `http://127.0.0.1:PORT/auth?token=…`: `illogical web` asks the daemon
//!   for it over the socket and opens it, and the desktop app reads the
//!   file. The link sets the cookie and moves on to the page, so the token
//!   leaves the address bar at once.
//! - **`tailscale serve`'s identity header** is only believed from
//!   tailscaled: on Linux, the connection's other end must belong to root
//!   (tailscaled) or to the owner's own account (`loopback_uid`), so
//!   another account can't claim to be the owner by sending the header.

use std::{
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The token's file in the state directory.
pub const FILE: &str = "local-token";
/// Another file instead (tests: every daemon of a run shares one).
pub const ENV_FILE: &str = "ILLOGICAL_LOCAL_TOKEN_FILE";
/// The sign-in link's path.
pub const AUTH_PATH: &str = "/auth";

/// Where the token is for a daemon with this state directory.
pub fn path(state_dir: &Path) -> PathBuf {
    match std::env::var_os(ENV_FILE).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => state_dir.join(FILE),
    }
}

/// The token in `path`, made (0600, owner only) if there is none. One
/// another account could read or swap is refused.
pub fn load_or_create(path: &Path) -> anyhow::Result<String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            if !m.file_type().is_file() {
                anyhow::bail!("{} isn't a plain file", path.display());
            }
            if m.uid() != nix::unistd::geteuid().as_raw() {
                anyhow::bail!("{} belongs to another account", path.display());
            }
            if m.mode() & 0o077 != 0 {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
            let mut s = String::new();
            std::fs::File::open(path)?.read_to_string(&mut s)?;
            let s = s.trim().to_owned();
            if s.len() < 32 {
                anyhow::bail!("{} is too short to be a token: delete it and restart", path.display());
            }
            Ok(s)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                crate::store::private_dir(dir)?;
            }
            let token = format!("ilt_{}", hex::encode(crate::push::random::<32>()));
            // Written aside and linked in: whoever starts beside us (two
            // daemons sharing a file) reads a whole token, never half of one.
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(token.as_bytes())?;
            f.sync_all()?;
            let linked = std::fs::hard_link(&tmp, path);
            let _ = std::fs::remove_file(&tmp);
            match linked {
                Ok(()) => Ok(token),
                // Someone else made it first: theirs.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => load_or_create(path),
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Whether a loopback connection carrying `tailscale serve`'s identity
/// header can be serve's: its other end is tailscaled's (root) or the
/// owner's own. Where that can't be told (not Linux), it is.
pub fn serve_peer_ok(peer: SocketAddr, port: u16) -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    let me = nix::unistd::geteuid().as_raw();
    match loopback_uid(peer, port) {
        Some(uid) => uid == 0 || uid == me,
        None => false,
    }
}

/// The account that owns the client end of a loopback TCP connection to
/// our `port` (Linux: `/proc/net/tcp{,6}`, which has both ends).
pub fn loopback_uid(peer: SocketAddr, port: u16) -> Option<u32> {
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .find_map(|table| uid_in(&table, peer, port))
}

fn uid_in(table: &str, peer: SocketAddr, port: u16) -> Option<u32> {
    let want = (peer.ip().to_canonical(), peer.port());
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let local = parse_addr(f.get(1)?)?;
        let remote = parse_addr(f.get(2)?)?;
        ((local.ip().to_canonical(), local.port()) == want && remote.port() == port).then(|| f.get(7)?.parse().ok())?
    })
}

/// `0100007F:1E01` (IPv4) or 32 hex digits and a port (IPv6): each 32-bit
/// word as the kernel holds it, printed as a native number.
fn parse_addr(s: &str) -> Option<SocketAddr> {
    let (ip, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |h: &str| u32::from_str_radix(h, 16).ok().map(u32::to_ne_bytes);
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(ip)?)),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[4 * i..4 * i + 4].copy_from_slice(&word(&ip[8 * i..8 * i + 8])?);
            }
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_file_is_private_and_kept() {
        let dir = std::env::temp_dir().join(format!("ilg-localauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join(FILE);
        let t = load_or_create(&p).unwrap();
        assert!(t.starts_with("ilt_") && t.len() == 68, "{t}");
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(load_or_create(&p).unwrap(), t, "kept across restarts");
        // Made readable by others: closed again.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(load_or_create(&p).unwrap(), t);
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        // A link isn't followed.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(load_or_create(&link).is_err());
        std::fs::write(&p, "short").unwrap();
        assert!(load_or_create(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn proc_net_tcp_names_the_client_ends_owner() {
        // 127.0.0.1:40000 -> 127.0.0.1:7681 (0x1E01), and the server's end.
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
            0: 0100007F:1E01 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1\n   \
            1: 0100007F:9C40 0100007F:1E01 01 00000000:00000000 00:00000000 00000000  1001        0 2 1\n   \
            2: 0100007F:1E01 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 3 1\n";
        let peer: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        if cfg!(target_endian = "little") {
            assert_eq!(uid_in(v4, peer, 7681), Some(1001));
            assert_eq!(uid_in(v4, peer, 7682), None);
            assert_eq!(uid_in(v4, "127.0.0.1:40001".parse().unwrap(), 7681), None);
            // ::1 and a v4-mapped peer (a dual-stack listener).
            let v6 = "header\n   0: 00000000000000000000000001000000:9C40 00000000000000000000000001000000:1E01 01 0 0 0 0 0 1\n   \
                1: 0000000000000000FFFF00000100007F:9C41 0000000000000000FFFF00000100007F:1E01 01 0 0 0 42 0 1\n";
            assert_eq!(uid_in(v6, "[::1]:40000".parse().unwrap(), 7681), Some(0));
            assert_eq!(uid_in(v6, "[::ffff:127.0.0.1]:40001".parse().unwrap(), 7681), Some(42));
            assert_eq!(uid_in(v6, "127.0.0.1:40001".parse().unwrap(), 7681), Some(42));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_connection_is_ours() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_s, peer) = l.accept().unwrap();
        assert_eq!(peer, c.local_addr().unwrap());
        assert_eq!(loopback_uid(peer, port), Some(nix::unistd::geteuid().as_raw()));
        assert!(serve_peer_ok(peer, port));
    }
}
