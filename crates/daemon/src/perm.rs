//! File modes. On Unix they're what keeps keys, tokens and sockets the
//! user's own; on Windows these files live under the user's profile
//! (`%LOCALAPPDATA%`), whose inherited ACL already admits only the user,
//! SYSTEM and Administrators, so setting a mode is a no-op there.

use std::{fs, io, path::Path};

/// `chmod`.
pub fn set(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// The permission bits. Windows has none: a file reads as the user's
/// alone (0o600, or 0o400 when read-only).
pub fn mode(m: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        if m.permissions().readonly() { 0o400 } else { 0o600 }
    }
}

/// Create files with `mode`.
pub fn open_mode(o: &mut fs::OpenOptions, mode: u32) -> &mut fs::OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(o, mode);
    #[cfg(not(unix))]
    let _ = mode;
    o
}

/// Create directories with `mode`.
pub fn dir_mode(b: &mut fs::DirBuilder, mode: u32) -> &mut fs::DirBuilder {
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(b, mode);
    #[cfg(not(unix))]
    let _ = mode;
    b
}

/// Whether this process's user owns the file. Windows: files under the
/// user's profile are taken as theirs (the profile's ACL).
pub fn mine(m: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.uid() == nix::unistd::geteuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        true
    }
}
