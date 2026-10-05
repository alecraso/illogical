//! Paths compared through links: a shell, an editor and the OS may each
//! name the same place differently (`/var/x` on macOS is `/private/var/x`,
//! and any symlinked directory likewise), so a path is tried as given and
//! resolved.

/// `d` as given (without a trailing slash) and resolved, if that differs.
pub fn forms(d: &str) -> Vec<String> {
    let d = if d.len() > 1 { d.trim_end_matches('/') } else { d };
    let mut out = vec![d.to_owned()];
    if let Ok(real) = std::fs::canonicalize(d) {
        let real = real.to_string_lossy().into_owned();
        if real != d {
            out.push(real);
        }
    }
    out
}

/// `path` is `dir` or below it (`/src/a` isn't under `/src/ab`).
pub fn is_under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || dir.ends_with('/'))
}

/// `file` relative to `dir` when it's inside it, through links on either
/// side; otherwise `file` as it was.
pub fn relative(dir: &str, file: &str) -> String {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        return file.to_owned();
    }
    let inside = |f: &str, d: &str| f.strip_prefix(d).and_then(|r| r.strip_prefix('/')).map(str::to_owned);
    if let Some(r) = inside(file, dir) {
        return r;
    }
    // The file may not exist yet (a new one): resolve its directory.
    let files = match file.rsplit_once('/') {
        Some((parent, name)) if !parent.is_empty() => {
            forms(parent).into_iter().map(|p| format!("{p}/{name}")).chain([file.to_owned()]).collect()
        }
        _ => vec![file.to_owned()],
    };
    for d in forms(dir) {
        for f in &files {
            if let Some(r) = inside(f, &d) {
                return r;
            }
        }
    }
    file.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn relative_through_links() {
        let root = std::env::temp_dir().join(format!("ilg-paths-{}", std::process::id()));
        std::fs::create_dir_all(root.join("real/src")).unwrap();
        let _ = std::os::unix::fs::symlink(root.join("real"), root.join("link"));
        let s = |p: std::path::PathBuf| p.display().to_string();
        assert_eq!(relative(&s(root.join("real")), &s(root.join("real/src/a.rs"))), "src/a.rs");
        // One side through the link, the other resolved: either way round,
        // and for a file that isn't there yet.
        assert_eq!(relative(&s(root.join("link")), &s(root.join("real/src/new.rs"))), "src/new.rs");
        assert_eq!(relative(&s(root.join("real")), &s(root.join("link/src/new.rs"))), "src/new.rs");
        assert_eq!(relative(&s(root.join("real/")), &s(root.join("real/x"))), "x");
        assert_eq!(relative(&s(root.join("real")), "/etc/hosts"), "/etc/hosts");
        assert_eq!(relative(&s(root.join("rea")), &s(root.join("real/x"))), s(root.join("real/x")));
        assert_eq!(relative("", "/a/b"), "/a/b");
        assert!(is_under("/src/a/b", "/src/a") && !is_under("/src/ab", "/src/a"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
