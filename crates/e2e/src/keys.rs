//! A device's keys: X25519 (its Noise static key) and Ed25519 (what it
//! signs approvals, revocations and grants with). Daemons and the CLI keep
//! them in a file; browsers keep theirs in IndexedDB.
//!
//! ```text
//! illogical-device-key 1
//! noise <private hex> <public hex>
//! sign <seed hex>
//! ```

use std::{fs, io::Write, path::Path};

use anyhow::{Context, bail};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

use crate::channel::PARAMS;

const HEADER: &str = "illogical-device-key 1";

pub struct DeviceKeys {
    pub noise_private: [u8; 32],
    pub noise_public: [u8; 32],
    pub sign: SigningKey,
}

impl DeviceKeys {
    pub fn generate() -> Self {
        let kp = snow::Builder::new(PARAMS.parse().unwrap()).generate_keypair().expect("x25519 key pair");
        Self {
            noise_private: kp.private.try_into().unwrap(),
            noise_public: kp.public.try_into().unwrap(),
            sign: SigningKey::from_bytes(&crate::random()),
        }
    }

    pub fn sign_public(&self) -> [u8; 32] {
        self.sign.verifying_key().to_bytes()
    }

    /// The device's id: derived from both public keys, so control can't
    /// give one device another's id.
    pub fn id(&self) -> String {
        device_id(&self.noise_public, &self.sign_public())
    }

    pub fn signature(&self, msg: &[u8]) -> [u8; 64] {
        self.sign.sign(msg).to_bytes()
    }

    /// Read the file, or make it (0600) if it doesn't exist.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let k = Self::generate();
        k.save(path)?;
        Ok(k)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        // Windows: the file lives in the user's profile, whose ACL already
        // admits only the user (and SYSTEM and Administrators).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
                bail!("{} is readable by others: chmod 600 it", path.display());
            }
        }
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            bail!("{} is not an illogical device key", path.display());
        }
        let (mut noise, mut sign) = (None, None);
        for line in lines {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.as_slice() {
                ["noise", private, public] => noise = Some((hex32(private)?, hex32(public)?)),
                ["sign", seed] => sign = Some(SigningKey::from_bytes(&hex32(seed)?)),
                _ => {}
            }
        }
        let ((noise_private, noise_public), sign) = noise.zip(sign).context("incomplete key file")?;
        Ok(Self { noise_private, noise_public, sign })
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        writeln!(f, "{HEADER}")?;
        writeln!(f, "noise {} {}", hex::encode(self.noise_private), hex::encode(self.noise_public))?;
        writeln!(f, "sign {}", hex::encode(self.sign.to_bytes()))?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// `hex(SHA-256("illogical device id" ‖ noise ‖ sign))[..16]`.
pub fn device_id(noise: &[u8; 32], sign: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"illogical device id");
    h.update(noise);
    h.update(sign);
    hex::encode(&h.finalize()[..8])
}

/// What people compare when approving: the id in four groups.
pub fn fingerprint(id: &str) -> String {
    id.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join("-")
}

pub fn hex32(s: &str) -> anyhow::Result<[u8; 32]> {
    let v = hex::decode(s)?;
    v.try_into().map_err(|_| anyhow::anyhow!("expected 32 bytes of hex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("illogical-keys-{}", std::process::id()));
        let path = dir.join("device.key");
        let k = DeviceKeys::load_or_create(&path).unwrap();
        let again = DeviceKeys::load_or_create(&path).unwrap();
        assert_eq!(k.id(), again.id());
        assert_eq!(k.noise_private, again.noise_private);
        // Modes are Unix's; on Windows the profile's ACL keeps it private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(DeviceKeys::load(&path).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fingerprint_groups() {
        assert_eq!(fingerprint("0123456789abcdef"), "0123-4567-89ab-cdef");
    }
}
