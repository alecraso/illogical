// S33: this device as a hand. With lending on, the page keeps a summary
// connection to each of the account's machines and offers them tools
// (`ClientMsg::Hand`); an agent there calls one through the daemon's MCP
// server (`device_call`), and the call comes here as `hand_call`. The
// person decides each call on a card: Deny, Allow, or (for a tool that
// doesn't need them) allow it for a while.
//
// Tools that need the person (the camera, the mic, a question) start from
// the Allow tap itself, since phones open the camera and the file picker
// only from a gesture.

import { Client } from "./client";
import type { ControlSession } from "./control";
import type { HandTool, ServerMsg } from "./proto";

type Call = Extract<ServerMsg, { type: "hand_call" }>;
type Args = Record<string, unknown>;

interface Tool extends HandTool {
  /** A person must act (camera, mic, answer): never granted standing. */
  person: boolean;
  /** What the card says it will do. */
  verb: (a: Args) => string;
  /** Runs it; called inside the Allow tap. */
  run: (a: Args, card: HTMLElement) => Promise<unknown>;
}

const KEY = "illogical-hand";
/** How long "for 15 minutes" lasts. */
const STANDING_MS = 15 * 60_000;

const obj = (properties: Record<string, unknown>, required: string[] = []) => ({ type: "object", properties, required });

function pickFile(accept: string, capture?: string): Promise<File> {
  return new Promise((resolve, reject) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    if (capture) input.setAttribute("capture", capture);
    input.style.display = "none";
    document.body.append(input);
    const done = () => input.remove();
    input.addEventListener("change", () => {
      const f = input.files?.[0];
      done();
      if (f) resolve(f);
      else reject(new Error("the person didn't pick anything"));
    });
    input.addEventListener("cancel", () => {
      done();
      reject(new Error("the person cancelled"));
    });
    input.click();
  });
}

function b64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

/** A picture, scaled to fit `max` pixels, as JPEG. */
async function picture(file: File, max: number): Promise<unknown> {
  const bmp = await createImageBitmap(file, { imageOrientation: "from-image" });
  const scale = Math.min(1, max / Math.max(bmp.width, bmp.height));
  const w = Math.round(bmp.width * scale);
  const h = Math.round(bmp.height * scale);
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  canvas.getContext("2d")!.drawImage(bmp, 0, 0, w, h);
  const blob = await new Promise<Blob>((ok, no) => canvas.toBlob((b) => (b ? ok(b) : no(new Error("can't encode"))), "image/jpeg", 0.85));
  return {
    image: { data: b64(await blob.arrayBuffer()), mime: "image/jpeg" },
    width: w,
    height: h,
    original: { width: bmp.width, height: bmp.height, bytes: file.size, type: file.type },
  };
}

const maxPx = (a: Args) => Math.min(4096, Math.max(256, Number(a.max_px) || 1600));

const TOOLS: Tool[] = [
  {
    name: "location",
    description: "Where the device is now: latitude, longitude, accuracy in meters, and altitude, heading and speed when known.",
    schema: obj({ precise: { type: "boolean", description: "GPS-precise (slower, more battery). Default true." } }),
    person: false,
    verb: () => "know where this phone is",
    run: (a) =>
      new Promise((resolve, reject) =>
        navigator.geolocation.getCurrentPosition(
          (p) =>
            resolve({
              lat: p.coords.latitude,
              lon: p.coords.longitude,
              accuracy_m: p.coords.accuracy,
              altitude_m: p.coords.altitude,
              heading: p.coords.heading,
              speed_mps: p.coords.speed,
              at: new Date(p.timestamp).toISOString(),
            }),
          (e) => reject(new Error(`location: ${e.message || ["", "permission denied", "unavailable", "timed out"][e.code]}`)),
          { enableHighAccuracy: a.precise !== false, timeout: 30_000, maximumAge: 10_000 },
        ),
      ),
  },
  {
    name: "take_photo",
    description: "The person takes a photo with the device's camera (for what `prompt` says). Comes back as a JPEG file on the calling machine.",
    schema: obj({
      prompt: { type: "string", description: "What to photograph, shown to the person." },
      max_px: { type: "integer", description: "Longest side in pixels (default 1600)." },
    }),
    person: true,
    verb: (a) => `take a photo${a.prompt ? `: “${String(a.prompt)}”` : ""}`,
    run: async (a) => picture(await pickFile("image/*", "environment"), maxPx(a)),
  },
  {
    name: "pick_photo",
    description: "The person picks a photo or screenshot from the device's library. Comes back as a JPEG file on the calling machine.",
    schema: obj({
      prompt: { type: "string", description: "Which picture, shown to the person." },
      max_px: { type: "integer", description: "Longest side in pixels (default 1600)." },
    }),
    person: true,
    verb: (a) => `pick a photo${a.prompt ? `: “${String(a.prompt)}”` : ""}`,
    run: async (a) => picture(await pickFile("image/*"), maxPx(a)),
  },
  {
    name: "record_audio",
    description: "Record from the device's microphone for up to `seconds` (default 10, at most 120); the person can stop early. Comes back as an audio file on the calling machine.",
    schema: obj({
      seconds: { type: "number" },
      prompt: { type: "string", description: "What to record, shown to the person." },
    }),
    person: true,
    verb: (a) => `record ${Math.min(120, Number(a.seconds) || 10)}s of audio${a.prompt ? `: “${String(a.prompt)}”` : ""}`,
    run: async (a, card) => {
      const secs = Math.min(120, Math.max(1, Number(a.seconds) || 10));
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const rec = new MediaRecorder(stream);
      const parts: Blob[] = [];
      rec.ondataavailable = (e) => parts.push(e.data);
      const stopped = new Promise<void>((r) => (rec.onstop = () => r()));
      rec.start();
      const started = Date.now();
      const body = card.querySelector("[data-hand-body]")!;
      body.textContent = `Recording… up to ${secs}s`;
      const stop = document.createElement("button");
      stop.textContent = "Stop";
      stop.onclick = () => rec.state !== "inactive" && rec.stop();
      card.querySelector("[data-hand-buttons]")!.replaceChildren(stop);
      const timer = setTimeout(() => rec.state !== "inactive" && rec.stop(), secs * 1000);
      await stopped;
      clearTimeout(timer);
      for (const t of stream.getTracks()) t.stop();
      const blob = new Blob(parts, { type: rec.mimeType || "audio/mp4" });
      return { audio: { data: b64(await blob.arrayBuffer()), mime: blob.type }, seconds: (Date.now() - started) / 1000 };
    },
  },
  {
    name: "ask",
    description: "Ask the person holding the device a question; with `choices`, they tap one, else they type an answer.",
    schema: obj({ question: { type: "string" }, choices: { type: "array", items: { type: "string" } } }, ["question"]),
    person: true,
    verb: (a) => `ask you: “${String(a.question ?? "")}”`,
    run: (a, card) =>
      new Promise((resolve, reject) => {
        const body = card.querySelector("[data-hand-body]")!;
        const buttons = card.querySelector("[data-hand-buttons]")!;
        body.textContent = String(a.question ?? "");
        const choices = Array.isArray(a.choices) ? a.choices.map(String) : [];
        const no = document.createElement("button");
        no.textContent = "Decline";
        no.onclick = () => reject(new Error("the person declined to answer"));
        if (choices.length) {
          buttons.replaceChildren(
            ...choices.map((c) => {
              const b = document.createElement("button");
              b.textContent = c;
              b.onclick = () => resolve({ answer: c });
              return b;
            }),
            no,
          );
        } else {
          const input = document.createElement("textarea");
          input.rows = 3;
          input.className = "hand-answer";
          body.append(input);
          const send = document.createElement("button");
          send.textContent = "Send";
          send.className = "primary";
          send.onclick = () => resolve({ answer: input.value });
          buttons.replaceChildren(no, send);
          input.focus();
        }
      }),
  },
  {
    name: "read_clipboard",
    description: "The text on the device's clipboard.",
    schema: obj({}),
    person: true,
    verb: () => "read your clipboard",
    run: async () => ({ text: await navigator.clipboard.readText() }),
  },
  {
    name: "device_info",
    description: "What the device is: its browser, screen, whether it's installed as an app, online, battery when the browser tells.",
    schema: obj({}),
    person: false,
    verb: () => "see what this device is (screen, battery, browser)",
    run: async () => {
      const nav = navigator as Navigator & {
        getBattery?: () => Promise<{ level: number; charging: boolean }>;
        connection?: { effectiveType?: string };
        standalone?: boolean;
      };
      const battery = nav.getBattery ? await nav.getBattery().then((b) => ({ level: b.level, charging: b.charging })) : null;
      return {
        agent: navigator.userAgent,
        screen: { width: screen.width, height: screen.height, dpr: devicePixelRatio },
        installed: matchMedia("(display-mode: standalone)").matches || nav.standalone === true,
        online: navigator.onLine,
        network: nav.connection?.effectiveType ?? null,
        battery,
        language: navigator.language,
        time_zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
      };
    },
  },
];

/** The tools this browser can actually do. */
function available(): Tool[] {
  return TOOLS.filter((t) => {
    if (t.name === "location") return "geolocation" in navigator;
    if (t.name === "record_audio") return !!navigator.mediaDevices?.getUserMedia && typeof MediaRecorder !== "undefined";
    if (t.name === "read_clipboard") return !!navigator.clipboard?.readText;
    return true;
  });
}

export function lending(): boolean {
  try {
    return localStorage.getItem(KEY) === "on";
  } catch {
    return false;
  }
}

class Hand {
  private clients = new Map<string, Client>();
  /** `tool from` → until when it's allowed without asking. */
  private standing = new Map<string, number>();
  private cards: HTMLElement | null = null;
  private unsub: (() => void) | null = null;
  readonly listeners = new Set<() => void>();

  /** Without control, the one daemon that served this page. */
  constructor(private session: ControlSession | null) {}

  start() {
    if (this.session) this.unsub ??= this.session.subscribe(() => this.sync());
    this.sync();
  }

  stop() {
    this.unsub?.();
    this.unsub = null;
    for (const c of this.clients.values()) {
      c.send({ type: "hand", tools: [] });
      c.close();
    }
    this.clients.clear();
    this.emit();
  }

  /** Machines reached now. */
  connected(): number {
    return [...this.clients.values()].filter((c) => c.connected).length;
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }

  /** One connection per machine of the account. */
  private sync() {
    const s = this.session;
    if (!s) {
      if (!this.clients.has("")) this.add("", new Client("", undefined, true));
      return;
    }
    const want = new Set(s.daemons.filter((d) => !d.sandbox).map((d) => d.id));
    for (const [id, c] of this.clients) {
      if (!want.has(id)) {
        c.close();
        this.clients.delete(id);
      }
    }
    for (const id of want) {
      if (this.clients.has(id)) continue;
      const t = s.target(id);
      if (t) this.add(id, new Client(`e2e:${id}`, t, true));
    }
  }

  private add(id: string, c: Client) {
    c.onHello = () => {
      c.send({ type: "hand", tools: available().map(({ name, description, schema }) => ({ name, description, schema })), name: this.name() });
      this.emit();
    };
    c.onHandCall = (msg) => this.call(c, msg);
    c.connect();
    this.clients.set(id, c);
  }

  private name(): string {
    const cert = this.session?.enrollment?.cert.name;
    if (cert) return cert;
    const ua = navigator.userAgent;
    return /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android phone" : "a browser";
  }

  private call(c: Client, msg: Call) {
    const tool = available().find((t) => t.name === msg.tool);
    const reply = (result?: unknown, error?: string) => c.send({ type: "hand_reply", id: msg.id, result, error });
    if (!tool) return reply(undefined, `this device has no tool ${msg.tool}`);
    const key = `${msg.tool} ${msg.from}`;
    if (!tool.person && (this.standing.get(key) ?? 0) > Date.now()) {
      tool.run(msg.args ?? {}, document.createElement("div")).then(
        (r) => reply(r),
        (e: Error) => reply(undefined, e.message),
      );
      return;
    }
    this.card(msg, tool, key, reply);
  }

  private card(msg: Call, tool: Tool, key: string, reply: (r?: unknown, e?: string) => void) {
    if (!this.cards) {
      this.cards = document.createElement("div");
      this.cards.className = "hand-cards";
      document.body.append(this.cards);
    }
    const card = document.createElement("div");
    card.className = "hand-card";
    card.dataset.handCall = String(msg.id);
    const title = document.createElement("div");
    title.className = "hand-title";
    title.textContent = `${msg.from} wants to ${tool.verb(msg.args ?? {})}`;
    const body = document.createElement("div");
    body.dataset.handBody = "";
    body.className = "dim";
    const buttons = document.createElement("div");
    buttons.dataset.handButtons = "";
    buttons.className = "prompt-buttons";
    card.append(title, body, buttons);
    this.cards.prepend(card);
    navigator.vibrate?.(80);

    const close = () => card.remove();
    const run = () => {
      buttons.replaceChildren();
      body.textContent = "Working…";
      tool.run(msg.args ?? {}, card).then(
        (r) => {
          reply(r);
          close();
        },
        (e: Error) => {
          reply(undefined, e.message);
          close();
        },
      );
    };
    const deny = document.createElement("button");
    deny.textContent = "Deny";
    deny.onclick = () => {
      reply(undefined, "the person denied it");
      close();
    };
    const allow = document.createElement("button");
    allow.className = "primary";
    allow.dataset.handAllow = "";
    allow.textContent = "Allow";
    allow.onclick = run;
    buttons.append(deny);
    if (!tool.person) {
      const keep = document.createElement("button");
      keep.textContent = "Allow for 15 min";
      keep.onclick = () => {
        this.standing.set(key, Date.now() + STANDING_MS);
        run();
      };
      buttons.append(keep);
    }
    buttons.append(allow);
  }
}

let hand: Hand | null = null;

/** Start lending if it's on (at load), or turn it on or off. `#lend=on`
 * or `#lend=off` in the address sets it too. */
export function setLending(session: ControlSession | null, on?: boolean): Hand | null {
  const m = /(?:^#|&)lend=(on|off)/.exec(location.hash);
  if (on === undefined && m) on = m[1] === "on";
  if (on !== undefined) {
    try {
      localStorage.setItem(KEY, on ? "on" : "off");
    } catch {
      // The setting just won't stick.
    }
  }
  if (lending()) {
    hand ??= new Hand(session);
    hand.start();
  } else {
    hand?.stop();
  }
  return hand;
}

export function currentHand(): Hand | null {
  return lending() ? hand : null;
}
