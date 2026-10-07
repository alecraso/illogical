//! Encryption at rest for history synced from other hosts (M4c). Synced
//! sandbox logs are where an agent's secrets end up, so the home daemon
//! keeps them sealed with a key only it holds.
//!
//! **Keys.** A key ring file (`synced/key`, 0600, or `--sync-key-file`):
//! `arugula-sync-keys 1` then one `<id> <64 hex>` line per 256-bit key.
//! The highest id is current: new files are sealed with it. Rotating adds a
//! key, re-seals every file under it and then drops the old ones, so a
//! leaked old key file opens nothing written since. Keeping the ring in one
//! file is what lets it come from a secrets manager later (Risks).
//!
//! **Files.** Append-only, one sealed record per append:
//!
//! ```text
//! header  "ILGSEAL1" | key id (u32 BE) | 0 (u32) | salt (32 random bytes)   48 bytes
//! record  length of what follows (u32 BE) | AES-256-GCM ciphertext + tag
//! ```
//!
//! Each file has its own key: HKDF-SHA256 of the ring key, salted with the
//! file's random salt, with the file's place (`host/pane/name`) as info, so
//! a file moved elsewhere doesn't open. Record `i` is sealed with nonce
//! `0u32 || i (u64 BE)` (never reused under one file key) and the header
//! plus `i` as associated data, so records can't be reordered, dropped from
//! the middle or swapped between files. A torn final record (a crash mid
//! append) is cut off on the next append. Lengths and file names (pane ids,
//! stream offsets) are not secret; contents are.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use sha2::Sha256;

const MAGIC: &[u8; 8] = b"ILGSEAL1";
const HEADER: usize = 48;
const TAG: usize = 16;
const RING_HEADER: &str = "arugula-sync-keys 1";
/// Largest record we'll read back (a push is at most 1 MB).
const MAX_RECORD: u32 = 8 << 20;

fn random<const N: usize>() -> [u8; N] {
    crate::push::random()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The keys synced history is sealed with.
pub struct KeyRing {
    path: PathBuf,
    keys: BTreeMap<u32, [u8; 32]>,
}

impl KeyRing {
    /// Load the ring, making one with a fresh key if there's none. A ring
    /// others can read is refused rather than used.
    pub fn open(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mode = crate::perm::mode(&fs::metadata(path)?);
                if mode & 0o077 != 0 {
                    return Err(io::Error::other(format!(
                        "{} is readable by others (mode {:o}); chmod 600 it",
                        path.display(),
                        mode & 0o777
                    )));
                }
                let mut lines = text.lines();
                if lines.next().map(str::trim) != Some(RING_HEADER) {
                    return Err(io::Error::other(format!("{}: not a sync key ring", path.display())));
                }
                let mut keys = BTreeMap::new();
                for l in lines.filter(|l| !l.trim().is_empty()) {
                    let (id, key) = l
                        .split_once(' ')
                        .and_then(|(id, k)| Some((id.parse::<u32>().ok()?, unhex(k)?)))
                        .ok_or_else(|| io::Error::other(format!("{}: bad key line", path.display())))?;
                    keys.insert(id, key);
                }
                if keys.is_empty() {
                    return Err(io::Error::other(format!("{}: no keys", path.display())));
                }
                Ok(Self { path: path.to_owned(), keys })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(dir) = path.parent() {
                    crate::store::private_dir(dir)?;
                }
                let ring = Self { path: path.to_owned(), keys: BTreeMap::from([(1, random::<32>())]) };
                ring.save()?;
                Ok(ring)
            }
            Err(e) => Err(e),
        }
    }

    fn save(&self) -> io::Result<()> {
        let mut text = format!("{RING_HEADER}\n");
        for (id, k) in &self.keys {
            text.push_str(&format!("{id} {}\n", hex(k)));
        }
        crate::store::write_atomic(&self.path, text.as_bytes())
    }

    pub fn current(&self) -> u32 {
        *self.keys.keys().next_back().expect("a ring has a key")
    }

    /// Add a key, which becomes current.
    pub fn add(&mut self) -> io::Result<u32> {
        let id = self.current() + 1;
        self.keys.insert(id, random::<32>());
        self.save()?;
        Ok(id)
    }

    /// Forget every key but the current one (after re-sealing under it).
    pub fn drop_old(&mut self) -> io::Result<()> {
        let current = self.current();
        self.keys.retain(|id, _| *id == current);
        self.save()
    }

    fn file_key(&self, id: u32, salt: &[u8], context: &str) -> io::Result<Aes256Gcm> {
        let master = self.keys.get(&id).ok_or_else(|| io::Error::other(format!("no key {id} in the ring")))?;
        // Frozen (#504): synced files already sealed under it.
        let mut info = b"illogical sync v1\0".to_vec();
        info.extend_from_slice(context.as_bytes());
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(salt), master).expand(&info, &mut key).map_err(|_| io::Error::other("hkdf"))?;
        Aes256Gcm::new_from_slice(&key).map_err(|_| io::Error::other("aes key"))
    }
}

fn nonce(i: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&i.to_be_bytes());
    n
}

fn aad(header: &[u8], i: u64) -> Vec<u8> {
    let mut a = header.to_vec();
    a.extend_from_slice(&i.to_be_bytes());
    a
}

/// A sealed file's header and where its records are.
struct Layout {
    header: [u8; HEADER],
    /// (offset, ciphertext length) of each whole record.
    records: Vec<(u64, u32)>,
    /// Where the whole records end (anything after is torn).
    end: u64,
}

fn layout(f: &mut File) -> io::Result<Layout> {
    let len = f.metadata()?.len();
    let mut header = [0u8; HEADER];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut header)?;
    if &header[..8] != MAGIC {
        return Err(io::Error::other("not a sealed file"));
    }
    let mut records = Vec::new();
    let mut at = HEADER as u64;
    while at + 4 <= len {
        let mut l = [0u8; 4];
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut l)?;
        let n = u32::from_be_bytes(l);
        if n < TAG as u32 || n > MAX_RECORD || at + 4 + n as u64 > len {
            break;
        }
        records.push((at + 4, n));
        at += 4 + n as u64;
    }
    Ok(Layout { header, records, end: at })
}

fn key_id(header: &[u8; HEADER]) -> u32 {
    u32::from_be_bytes([header[8], header[9], header[10], header[11]])
}

fn new_header(key: u32) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&key.to_be_bytes());
    h[16..].copy_from_slice(&random::<32>());
    h
}

fn private_rw() -> OpenOptions {
    let mut o = OpenOptions::new();
    crate::perm::open_mode(o.read(true).write(true), 0o600);
    o
}

/// Seal `plaintext` as the next record of the file at `path` (made if
/// missing, under the ring's current key). `context` is the file's place.
pub fn append(ring: &KeyRing, path: &Path, context: &str, plaintext: &[u8]) -> io::Result<()> {
    let mut f = private_rw().create(true).truncate(false).open(path)?;
    if f.metadata()?.len() < HEADER as u64 {
        f.set_len(0)?;
        f.write_all(&new_header(ring.current()))?;
    }
    let l = layout(&mut f)?;
    if l.end < f.metadata()?.len() {
        f.set_len(l.end)?; // a torn record from a crash
    }
    let cipher = ring.file_key(key_id(&l.header), &l.header[16..], context)?;
    let i = l.records.len() as u64;
    let sealed = cipher
        .encrypt(&nonce(i).into(), Payload { msg: plaintext, aad: &aad(&l.header, i) })
        .map_err(|_| io::Error::other("aes-gcm"))?;
    let mut rec = (sealed.len() as u32).to_be_bytes().to_vec();
    rec.extend_from_slice(&sealed);
    f.seek(SeekFrom::Start(l.end))?;
    f.write_all(&rec)?;
    f.sync_data()
}

/// Every record of a sealed file, opened and joined. Fails if any record
/// doesn't open (tampered, or the wrong key or place).
pub fn read(ring: &KeyRing, path: &Path, context: &str) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let l = layout(&mut f)?;
    let cipher = ring.file_key(key_id(&l.header), &l.header[16..], context)?;
    let mut out = Vec::new();
    for (i, (at, n)) in l.records.iter().enumerate() {
        let mut ct = vec![0u8; *n as usize];
        f.seek(SeekFrom::Start(*at))?;
        f.read_exact(&mut ct)?;
        let pt = cipher
            .decrypt(&nonce(i as u64).into(), Payload { msg: &ct, aad: &aad(&l.header, i as u64) })
            .map_err(|_| io::Error::other(format!("{}: record {i} doesn't open", path.display())))?;
        out.extend_from_slice(&pt);
    }
    Ok(out)
}

/// How many plaintext bytes a sealed file holds (no key needed).
pub fn plaintext_len(path: &Path) -> io::Result<u64> {
    let mut f = File::open(path)?;
    let l = layout(&mut f)?;
    Ok(l.records.iter().map(|(_, n)| (*n as usize - TAG) as u64).sum())
}

/// Seal the file again under the ring's current key (rotation): opened
/// with whatever key it had, written whole to a new file, swapped in.
pub fn reseal(ring: &KeyRing, path: &Path, context: &str) -> io::Result<()> {
    let mut f = File::open(path)?;
    let l = layout(&mut f)?;
    if key_id(&l.header) == ring.current() {
        return Ok(());
    }
    let cipher = ring.file_key(key_id(&l.header), &l.header[16..], context)?;
    let mut plain = Vec::new();
    for (i, (at, n)) in l.records.iter().enumerate() {
        let mut ct = vec![0u8; *n as usize];
        f.seek(SeekFrom::Start(*at))?;
        f.read_exact(&mut ct)?;
        plain.push(
            cipher
                .decrypt(&nonce(i as u64).into(), Payload { msg: &ct, aad: &aad(&l.header, i as u64) })
                .map_err(|_| io::Error::other(format!("{}: record {i} doesn't open", path.display())))?,
        );
    }
    let tmp = path.with_extension("reseal");
    let _ = fs::remove_file(&tmp);
    for p in &plain {
        append(ring, &tmp, context, p)?;
    }
    if plain.is_empty() {
        private_rw().create(true).truncate(true).open(&tmp)?.write_all(&new_header(ring.current()))?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ilg-seal-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sealed_files_round_trip_and_are_ciphertext() {
        let d = dir("rt");
        let ring = KeyRing::open(&d.join("key")).unwrap();
        let mode = crate::perm::mode(&fs::metadata(d.join("key")).unwrap());
        assert_eq!(mode & 0o777, 0o600);
        let f = d.join("seg.enc");
        append(&ring, &f, "box/3/seg", b"export TOKEN=hunter2\n").unwrap();
        append(&ring, &f, "box/3/seg", b"second record").unwrap();
        assert_eq!(read(&ring, &f, "box/3/seg").unwrap(), b"export TOKEN=hunter2\nsecond record");
        assert_eq!(plaintext_len(&f).unwrap(), 34);
        let raw = fs::read(&f).unwrap();
        assert!(!raw.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(crate::perm::mode(&fs::metadata(&f).unwrap()) & 0o777, 0o600);
        // Somewhere else, it doesn't open.
        assert!(read(&ring, &f, "box/4/seg").is_err());
        // Another ring doesn't open it.
        let other = KeyRing::open(&d.join("key2")).unwrap();
        assert!(read(&other, &f, "box/3/seg").is_err());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn tampering_and_reordering_are_caught() {
        let d = dir("tamper");
        let ring = KeyRing::open(&d.join("key")).unwrap();
        let f = d.join("x.enc");
        append(&ring, &f, "c", b"aaaa").unwrap();
        append(&ring, &f, "c", b"bbbb").unwrap();
        let good = fs::read(&f).unwrap();
        let mut bad = good.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        fs::write(&f, &bad).unwrap();
        assert!(read(&ring, &f, "c").is_err(), "a flipped bit");
        // Swap the two records (same length): the nonces no longer match.
        let rec = 4 + 4 + TAG;
        let mut swapped = good[..HEADER].to_vec();
        swapped.extend_from_slice(&good[HEADER + rec..]);
        swapped.extend_from_slice(&good[HEADER..HEADER + rec]);
        fs::write(&f, &swapped).unwrap();
        assert!(read(&ring, &f, "c").is_err(), "reordered");
        // A torn tail is cut off by the next append.
        let mut torn = good.clone();
        torn.extend_from_slice(&[0, 0, 0, 40, 1, 2, 3]);
        fs::write(&f, &torn).unwrap();
        assert_eq!(read(&ring, &f, "c").unwrap(), b"aaaabbbb");
        append(&ring, &f, "c", b"cc").unwrap();
        assert_eq!(read(&ring, &f, "c").unwrap(), b"aaaabbbbcc");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn rotation_reseals_and_drops_the_old_key() {
        let d = dir("rotate");
        let mut ring = KeyRing::open(&d.join("key")).unwrap();
        let f = d.join("x.enc");
        append(&ring, &f, "c", b"before").unwrap();
        let old_ring_text = fs::read_to_string(d.join("key")).unwrap();
        assert_eq!(ring.add().unwrap(), 2);
        reseal(&ring, &f, "c").unwrap();
        ring.drop_old().unwrap();
        append(&ring, &f, "c", b" after").unwrap();
        let ring = KeyRing::open(&d.join("key")).unwrap();
        assert_eq!(ring.current(), 2);
        assert_eq!(read(&ring, &f, "c").unwrap(), b"before after");
        // The old key alone opens nothing now.
        fs::write(d.join("old"), old_ring_text).unwrap();
        crate::perm::set(&d.join("old"), 0o600).unwrap();
        assert!(read(&KeyRing::open(&d.join("old")).unwrap(), &f, "c").is_err());
        // A ring others can read is refused (Unix: Windows has no such mode).
        #[cfg(unix)]
        {
            crate::perm::set(&d.join("old"), 0o644).unwrap();
            assert!(KeyRing::open(&d.join("old")).is_err());
        }
        fs::remove_dir_all(d).unwrap();
    }
}
