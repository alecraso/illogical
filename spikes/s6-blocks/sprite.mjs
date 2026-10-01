// Minimal Sprites API helpers for the S6 harnesses (Node).
// env: SPRITES_API_URL (default local wisp), SPRITE_TOKEN
const BASE = process.env.SPRITES_API_URL ?? "http://127.0.0.1:7788";
const auth = { Authorization: `Bearer ${process.env.SPRITE_TOKEN ?? ""}` };

export async function writeFile(sprite, path, content) {
  const r = await fetch(`${BASE}/v1/sprites/${sprite}/fs/write?path=${encodeURIComponent(path)}&mkdir=true`, {
    method: "PUT", headers: auth, body: content,
  });
  if (!r.ok) throw new Error(`write ${path}: ${r.status} ${await r.text()}`);
}
