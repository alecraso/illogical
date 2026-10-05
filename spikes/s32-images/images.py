# S32: the test files forms.sh pastes, in /tmp/illogical-s32 (macOS: sips
# makes the JPEG, GIF, HEIC and TIFF).
import os, struct, subprocess, zlib

D = "/tmp/illogical-s32"
os.makedirs(f"{D}/with space", exist_ok=True)
up = os.path.join(os.environ["TMPDIR"], "illogical-uploads", "70")
os.makedirs(up, mode=0o700, exist_ok=True)

def png(path, rgb=(220, 40, 40), w=64, h=48):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))

for p in [f"{D}/img-red.png", f"{D}/with space/img red.png", f"{D}/noext", f"{D}/IMG.PNG", f"{up}/img-20261005-1200.png"]:
    png(p)
png(f"{D}/img-blue.png", rgb=(30, 60, 220))
for fmt, ext in [("jpeg", "jpg"), ("gif", "gif"), ("heic", "heic"), ("tiff", "tiff")]:
    subprocess.run(["sips", "-s", "format", fmt, f"{D}/img-red.png", "--out", f"{D}/img.{ext}"], capture_output=True)
open(f"{D}/doc.pdf", "w").write("%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n")
open(f"{D}/notes.txt", "w").write("hello\n")
