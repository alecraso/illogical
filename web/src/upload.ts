// Files into a pane (M70): pasted, dropped or picked, each goes to the
// pane's host in chunks (`/api/panes/<id>/upload`), then their paths are
// pasted into the pane together (`/paste`), so a `claude` there reads them
// as images. A chip on the pane shows the progress, then what happened.
//
// Before going: an image wider or taller than Claude reads (it scales
// anything over ~1568 px down itself) is scaled down here, which keeps a
// phone's 12 MP photo well under the cap; and HEIC, which only WebKit can
// decode, becomes a JPEG where the browser can.

import type { PaneId } from "./proto";

/** What each request carries; the daemon takes up to 4 MB. */
const CHUNK = 1 << 20;
/** The longest side an image is sent at. */
const MAX_SIDE = 2000;

export interface ChipAction {
  label: string;
  run: () => void;
}

/** Where an upload says how it's going: the pane's chip. */
export interface Chip {
  show(text: string, opts?: { actions?: ChipAction[]; error?: boolean; hideAfterMs?: number }): void;
  hide(): void;
}

type Request = (method: string, path: string, body?: unknown) => Promise<{ ok: boolean; status: number; json<T>(): Promise<T> }>;

const RASTER = new Set(["image/png", "image/jpeg", "image/webp"]);
const HEIC = /\.(heic|heif)$/i;

/** The extension the file goes by: its name's, else its type's. */
function extOf(name: string, type: string): string {
  const fromName = /\.([a-z0-9]{1,8})$/i.exec(name)?.[1];
  const fromType = /^[a-z]+\/([a-z0-9]{1,8})/i.exec(type)?.[1];
  return (fromName ?? (fromType === "jpeg" ? "jpg" : fromType) ?? "").toLowerCase();
}

/** The bytes to send and their extension: a big image scaled down, HEIC
 * turned into a JPEG; anything else as it is. */
export async function prepare(f: File): Promise<{ bytes: Uint8Array; ext: string }> {
  const heic = f.type === "image/heic" || f.type === "image/heif" || HEIC.test(f.name);
  const ext = extOf(f.name, f.type);
  if ((heic || RASTER.has(f.type)) && typeof createImageBitmap === "function") {
    try {
      const img = await createImageBitmap(f);
      const scale = Math.min(1, MAX_SIDE / Math.max(img.width, img.height));
      if (heic || scale < 1) {
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(img.width * scale);
        canvas.height = Math.round(img.height * scale);
        canvas.getContext("2d")!.drawImage(img, 0, 0, canvas.width, canvas.height);
        const type = f.type === "image/png" ? "image/png" : "image/jpeg";
        const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, type, 0.9));
        if (blob) return { bytes: new Uint8Array(await blob.arrayBuffer()), ext: type === "image/png" ? "png" : "jpg" };
      }
    } catch {
      // Not something this browser decodes (HEIC outside WebKit): as it is.
    }
  }
  return { bytes: new Uint8Array(await f.arrayBuffer()), ext };
}

function hexId(): string {
  return [...crypto.getRandomValues(new Uint8Array(8))].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function why(res: { status: number; json<T>(): Promise<T> }): Promise<string> {
  const e = await res.json<{ error?: string }>().catch(() => null);
  return e?.error ?? `refused (${res.status})`;
}

/** Send `files` to the pane's host, then paste their paths into it. */
export async function upload(request: Request, pane: PaneId, files: File[], chip: Chip | undefined): Promise<void> {
  if (!files.length) return;
  const paths: string[] = [];
  const many = files.length > 1;
  for (const [i, f] of files.entries()) {
    const label = many ? `${f.name || "file"} (${i + 1} of ${files.length})` : f.name || "file";
    chip?.show(`Preparing ${label}…`);
    const { bytes, ext } = await prepare(f);
    const id = hexId();
    let path = "";
    for (let at = 0; ; at += CHUNK) {
      const last = at + CHUNK >= bytes.length;
      chip?.show(`Uploading ${label}… ${Math.round((Math.min(at + CHUNK, bytes.length) / Math.max(bytes.length, 1)) * 100)}%`);
      const q = `id=${id}&ext=${encodeURIComponent(ext)}&offset=${at}${last ? "&last=true" : ""}`;
      let res;
      try {
        res = await request("POST", `/api/panes/${pane}/upload?${q}`, bytes.subarray(at, at + CHUNK));
      } catch (e) {
        chip?.show(`Couldn't upload ${label}: ${(e as Error).message}`, { error: true, hideAfterMs: 8000 });
        return;
      }
      if (!res.ok) {
        chip?.show(`Couldn't upload ${label}: ${await why(res)}`, { error: true, hideAfterMs: 8000 });
        return;
      }
      if (last) {
        path = (await res.json<{ path: string }>()).path;
        break;
      }
    }
    paths.push(path);
  }
  const what = many ? `${files.length} files` : "the file";
  const paste = async (force: boolean) => {
    const res = await request("POST", `/api/panes/${pane}/paste`, { paths, force });
    if (!res.ok) return chip?.show(`Uploaded ${what}, but couldn't paste: ${await why(res)}`, { error: true, hideAfterMs: 8000 });
    const v = await res.json<{ pasted: boolean; front?: string; text?: string }>();
    if (v.pasted) return chip?.show(`Pasted ${what}`, { hideAfterMs: 2000 });
    // Not a shell or an agent in front: the path probably means nothing
    // to what's running.
    const front = (v.front ?? "something").split(/\s+/)[0].split("/").pop();
    chip?.show(`Uploaded ${what}; ${front} is in front, so it wasn't pasted`, {
      actions: [
        {
          label: "Copy",
          run: () => {
            void navigator.clipboard?.writeText(v.text ?? paths.join(" "));
            chip?.show("Copied", { hideAfterMs: 1500 });
          },
        },
        { label: "Paste anyway", run: () => void paste(true) },
      ],
    });
  };
  await paste(false);
}

/** Ask for files: the browser's picker, which on a phone also offers its
 * photos and camera. Nothing if it's dismissed. */
export function pick(): Promise<File[]> {
  return new Promise((res) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.addEventListener("change", () => res([...(input.files ?? [])]));
    input.addEventListener("cancel", () => res([]));
    input.click();
  });
}
