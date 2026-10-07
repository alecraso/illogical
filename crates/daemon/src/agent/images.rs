//! M71: images in an agent block's prompts and transcript. Each is kept in
//! the block's folder (`images/<hash>.<ext>`), so the log and the
//! transcript hold its name, not the image, and a client fetches it when
//! it shows it (the block's `image {name}`).

use std::{
    io,
    path::{Path, PathBuf},
};

use base64::Engine;
use sha2::{Digest, Sha256};

/// Where in the block's folder.
pub const DIR: &str = "images";

/// What an image prompt block's `_meta` names it by.
pub const META: &str = "arugula/image";

/// The image types agents take (Claude's): what the bytes say, not the
/// file's name.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

fn ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// A kept image's type, by its name; `None` for a name we didn't make.
pub fn mime(name: &str) -> Option<&'static str> {
    let (hash, e) = name.split_once('.')?;
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    ["image/png", "image/jpeg", "image/gif", "image/webp"].into_iter().find(|m| ext(m) == Some(e))
}

/// An image's name: its hash, and its type's extension.
pub fn name(bytes: &[u8]) -> Option<String> {
    let ext = ext(sniff(bytes)?)?;
    Some(format!("{}.{ext}", hex::encode(&Sha256::digest(bytes)[..8])))
}

/// Keep an image in `dir` (the block's folder): its name. The same image
/// is kept once.
pub fn keep(dir: &Path, bytes: &[u8]) -> io::Result<String> {
    let name = name(bytes).ok_or_else(|| io::Error::other("not a PNG, JPEG, GIF or WebP image"))?;
    let dir = dir.join(DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&name);
    if !path.exists() {
        crate::store::write_atomic(&path, bytes)?;
    }
    Ok(name)
}

/// An image that came as base64 (in an imported conversation).
pub fn decode(data: &str) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?;
    sniff(&bytes).map(|_| bytes)
}

/// Where a kept image is, if `name` is one of ours.
pub fn path(dir: &Path, name: &str) -> Option<PathBuf> {
    mime(name)?;
    Some(dir.join(DIR).join(name))
}

/// An ACP image content block for a kept image, named in its `_meta`.
pub fn block(dir: &Path, name: &str) -> io::Result<serde_json::Value> {
    let mime = mime(name).ok_or_else(|| io::Error::other("not an image of ours"))?;
    let bytes = std::fs::read(dir.join(DIR).join(name))?;
    Ok(serde_json::json!({
        "type": "image",
        "mimeType": mime,
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        "_meta": { META: name },
    }))
}

/// `image {name}`: a kept image, for a client to show.
pub fn read(dir: &Path, name: &str) -> Result<serde_json::Value, String> {
    let mime = mime(name).ok_or("no such image")?;
    let bytes = std::fs::read(dir.join(DIR).join(name)).map_err(|_| "no such image".to_owned())?;
    Ok(serde_json::json!({ "mime": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n rest of it";

    #[test]
    fn an_image_is_kept_once_by_its_hash_and_read_back() {
        let dir = std::env::temp_dir().join(format!("ilg-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let name = keep(&dir, PNG).unwrap();
        assert!(name.ends_with(".png") && name.len() == 20, "{name}");
        assert_eq!(keep(&dir, PNG).unwrap(), name);
        assert_eq!(std::fs::read_dir(dir.join(DIR)).unwrap().count(), 1);
        let b = block(&dir, &name).unwrap();
        assert_eq!((b["type"].as_str(), b["mimeType"].as_str()), (Some("image"), Some("image/png")));
        assert_eq!(b["_meta"][META], name.as_str());
        assert_eq!(read(&dir, &name).unwrap()["data"], b["data"]);
        assert!(keep(&dir, b"plain text").is_err());
        assert_eq!(decode("iVBORw0KGgo=").and_then(|b| super::name(&b)).as_deref().map(mime), Some(Some("image/png")));
        assert_eq!(decode("aGVsbG8="), None, "base64, but not an image");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_names_we_made_are_read() {
        let dir = Path::new("/nonexistent");
        for bad in ["../../etc/passwd", "0123456789abcdef.txt", "0123456789abcdef", "x.png", "0123456789abcdeg.png"] {
            assert!(read(dir, bad).is_err() && path(dir, bad).is_none(), "{bad}");
        }
        assert_eq!(mime("0123456789abcdef.jpg"), Some("image/jpeg"));
    }
}
