//! Arugula's VS Code extension as a VSIX (M28), for VS Code and Cursor
//! on your own machines: `arugula editors install`, or `just vsix` for
//! the marketplaces. A VSIX is a zip with a manifest; the files are the
//! ones editor blocks install (`ext/`).

use super::server::{EXT_FILES, EXT_VERSION};

const README: &str = include_str!("ext/README.md");
const LICENSE: &str = include_str!("../../../../LICENSE-MIT");

/// The extension's file name: `arugula-editor-0.2.0.vsix`.
pub fn file_name() -> String {
    format!("arugula-editor-{EXT_VERSION}.vsix")
}

/// The VSIX's bytes.
pub fn build() -> Vec<u8> {
    let pkg: serde_json::Value = serde_json::from_str(EXT_FILES[0].1).expect("package.json");
    let s = |k: &str| pkg[k].as_str().unwrap_or_default().to_owned();
    let esc = |t: String| t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let manifest = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id="{name}" Version="{version}" Publisher="{publisher}" />
    <DisplayName>{display}</DisplayName>
    <Description xml:space="preserve">{description}</Description>
    <Tags>{tags}</Tags>
    <Categories>Other,Themes</Categories>
    <GalleryFlags>Public</GalleryFlags>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="{engine}" />
      <Property Id="Microsoft.VisualStudio.Code.ExtensionKind" Value="workspace" />
      <Property Id="Microsoft.VisualStudio.Services.Links.Source" Value="{repo}" />
      <Property Id="Microsoft.VisualStudio.Services.Content.Pricing" Value="Free" />
    </Properties>
    <License>extension/LICENSE.txt</License>
  </Metadata>
  <Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true" />
    <Asset Type="Microsoft.VisualStudio.Services.Content.Details" Path="extension/README.md" Addressable="true" />
    <Asset Type="Microsoft.VisualStudio.Services.Content.License" Path="extension/LICENSE.txt" Addressable="true" />
  </Assets>
</PackageManifest>
"#,
        name = esc(s("name")),
        version = esc(s("version")),
        publisher = esc(s("publisher")),
        display = esc(s("displayName")),
        description = esc(s("description")),
        tags = esc(pkg["keywords"]
            .as_array()
            .map(|k| k.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(","))
            .unwrap_or_default()),
        engine = esc(pkg["engines"]["vscode"].as_str().unwrap_or("*").to_owned()),
        repo = esc(pkg["repository"]["url"].as_str().unwrap_or("").to_owned()),
    );
    let types = r#"<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension=".json" ContentType="application/json"/><Default Extension=".js" ContentType="application/javascript"/><Default Extension=".md" ContentType="text/markdown"/><Default Extension=".txt" ContentType="text/plain"/><Default Extension=".vsixmanifest" ContentType="text/xml"/></Types>
"#;
    let mut z = Zip::default();
    z.add("[Content_Types].xml", types.as_bytes());
    z.add("extension.vsixmanifest", manifest.as_bytes());
    for (name, text) in EXT_FILES {
        z.add(&format!("extension/{name}"), text.as_bytes());
    }
    z.add("extension/README.md", README.as_bytes());
    z.add("extension/LICENSE.txt", LICENSE.as_bytes());
    z.finish()
}

/// A zip of stored (uncompressed) files: all a VSIX needs.
#[derive(Default)]
struct Zip {
    out: Vec<u8>,
    central: Vec<u8>,
    n: u16,
}

impl Zip {
    fn add(&mut self, name: &str, data: &[u8]) {
        let crc = crc32(data);
        let at = self.out.len() as u32;
        let (len, nlen) = (data.len() as u32, name.len() as u16);
        // Local header: version 2.0, no flags, stored, a fixed 1980 date.
        self.out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for v in [20u16, 0, 0, 0, 0x21] {
            self.out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            self.out.extend_from_slice(&v.to_le_bytes());
        }
        self.out.extend_from_slice(&nlen.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(data);
        // Its central directory entry.
        self.central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for v in [20u16, 20, 0, 0, 0, 0x21] {
            self.central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            self.central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [nlen, 0, 0, 0, 0] {
            self.central.extend_from_slice(&v.to_le_bytes());
        }
        self.central.extend_from_slice(&0u32.to_le_bytes());
        self.central.extend_from_slice(&at.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.n += 1;
    }

    fn finish(mut self) -> Vec<u8> {
        let (at, size) = (self.out.len() as u32, self.central.len() as u32);
        self.out.extend_from_slice(&self.central);
        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        for v in [0u16, 0, self.n, self.n] {
            self.out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [size, at] {
            self.out.extend_from_slice(&v.to_le_bytes());
        }
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_is_zips() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_vsix_unzips_with_its_manifest() {
        let bytes = build();
        let path = std::env::temp_dir().join(format!("ilg-{}-{}", std::process::id(), file_name()));
        std::fs::write(&path, &bytes).unwrap();
        // Python's zipfile checks every entry's CRC.
        let out = std::process::Command::new("python3")
            .args(["-c", "import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); assert z.testzip() is None; print('\\n'.join(z.namelist()))"])
            .arg(&path)
            .output();
        let _ = std::fs::remove_file(&path);
        // No python3 (or Windows' Store stand-in for one): nothing to check with.
        let Ok(out) = out else { return };
        if String::from_utf8_lossy(&out.stderr).contains("Microsoft Store") {
            return;
        }
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let names = String::from_utf8_lossy(&out.stdout);
        for n in [
            "[Content_Types].xml",
            "extension.vsixmanifest",
            "extension/package.json",
            "extension/extension.js",
            "extension/README.md",
        ] {
            assert!(names.lines().any(|l| l == n), "{n} in {names}");
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(r#"Id="arugula-editor" Version="0.2.0" Publisher="arugula""#));
        assert!(text.contains(r#"ExtensionKind" Value="workspace""#));
    }
}
