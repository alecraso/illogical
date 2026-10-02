// GHOSTSNP decoding in the browser with upstream libghostty-vt compiled to
// wasm32-freestanding at S5's pinned Ghostty commit (22d13172), the same
// engine the daemon runs. There is no renderer here: upstream ships the VT
// core only, and ghostty-web's renderer reads its own (older) wasm.
const OK = 0, NO_VALUE = -4;
const DEC_HISTORY_ROWS_PRIMARY = 3;
const TERM_ROWS = 2, TERM_SCROLLBACK_ROWS = 15, TERM_SCROLLBACK_MAX_BYTES = 34, OPT_SCROLLBACK_MAX_BYTES = 27;

export class GhostSnp {
  static cache = {};
  static async load(url) {
    if (!GhostSnp.cache[url]) {
      let inst;
      const { instance } = await WebAssembly.instantiateStreaming(fetch(url), {
        env: { log: (p, n) => console.log("[wasm]", new TextDecoder().decode(new Uint8Array(inst.exports.memory.buffer, p, n))) },
      });
      inst = instance;
      GhostSnp.cache[url] = new GhostSnp(instance.exports);
    }
    return GhostSnp.cache[url];
  }

  constructor(x) {
    this.x = x;
    const p = x.ghostty_type_json();
    const mem = new Uint8Array(x.memory.buffer);
    let end = p;
    while (mem[end]) end++;
    this.types = JSON.parse(new TextDecoder().decode(mem.subarray(p, end)));
  }
  get dv() { return new DataView(this.x.memory.buffer); }
  ptrOut(fn) {
    const pp = this.x.ghostty_wasm_alloc_opaque();
    const rc = fn(pp);
    const v = this.dv.getUint32(pp, true);
    this.x.ghostty_wasm_free_opaque(pp);
    return [rc, v];
  }
  termGet(term, key) {
    const p = this.x.ghostty_wasm_alloc_usize();
    this.x.ghostty_terminal_get(term, key, p);
    const v = this.dv.getUint32(p, true);
    this.x.ghostty_wasm_free_usize(p);
    return v;
  }

  /** Plain text of the active screen including scrollback, trimmed. */
  plain(term) {
    const x = this.x, T = this.types, size = T.GhosttyFormatterTerminalOptions.size;
    const o = x.ghostty_wasm_alloc_u8_array(size);
    new Uint8Array(x.memory.buffer, o, size).fill(0);
    const f = T.GhosttyFormatterTerminalOptions.fields, e = T.GhosttyFormatterTerminalExtra, s = T.GhosttyFormatterScreenExtra;
    const dv = this.dv;
    dv.setUint32(o + f.size.offset, size, true);
    dv.setUint32(o + f.emit.offset, 0, true); // plain
    dv.setUint8(o + f.trim.offset, 1);
    dv.setUint32(o + f.extra.offset + e.fields.size.offset, e.size, true);
    dv.setUint32(o + f.extra.offset + e.fields.screen.offset + s.fields.size.offset, s.size, true);
    const [rc, fmt] = this.ptrOut((pp) => x.ghostty_formatter_terminal_new(0, pp, term, o));
    x.ghostty_wasm_free_u8_array(o, size);
    if (rc !== OK) throw new Error(`formatter_new ${rc}`);
    const pp = x.ghostty_wasm_alloc_opaque(), lp = x.ghostty_wasm_alloc_usize();
    if (x.ghostty_formatter_format_alloc(fmt, 0, pp, lp) !== OK) throw new Error("format");
    const ptr = this.dv.getUint32(pp, true), len = this.dv.getUint32(lp, true);
    const text = new TextDecoder().decode(new Uint8Array(x.memory.buffer, ptr, len));
    x.ghostty_free(0, ptr, len);
    x.ghostty_wasm_free_opaque(pp);
    x.ghostty_wasm_free_usize(lp);
    x.ghostty_formatter_free(fmt);
    return text;
  }

  /** Incremental decode: READY (visible screen usable), then history pages. */
  decode(bytes, { withPlain = true, maxBytes = 80 * 1024 * 1024 } = {}) {
    const x = this.x;
    const t0 = performance.now();
    const buf = x.ghostty_wasm_alloc_u8_array(bytes.length);
    new Uint8Array(x.memory.buffer, buf, bytes.length).set(bytes);
    const copied = performance.now() - t0;
    const [rc, dec] = this.ptrOut((pp) => x.ghostty_snapshot_decoder_new_buf(0, pp, buf, bytes.length));
    if (rc !== OK) throw new Error(`decoder_new_buf ${rc}`);
    const [rr, term] = this.ptrOut((pp) => x.ghostty_snapshot_decoder_ready(dec, pp));
    const ready = performance.now() - t0;
    if (rr !== OK) throw new Error(`ready ${rr}`);
    const hp = x.ghostty_wasm_alloc_u8_array(8);
    const hrc = x.ghostty_snapshot_decoder_get(dec, DEC_HISTORY_ROWS_PRIMARY, hp);
    const historyRows = hrc === OK ? Number(this.dv.getBigUint64(hp, true)) : null;
    x.ghostty_wasm_free_u8_array(hp, 8);
    const rowsAtReady = this.termGet(term, TERM_SCROLLBACK_ROWS);
    // The decoded terminal gets the default 64MiB budget, but wasm32 pages
    // hold fewer rows per byte than native: 64MiB keeps 56,384 of the
    // daemon's 64,511 rows. ~76MiB keeps them all; use 80MiB.
    const defaultMaxBytes = this.termGet(term, TERM_SCROLLBACK_MAX_BYTES);
    if (maxBytes) {
      const p = x.ghostty_wasm_alloc_usize();
      this.dv.setUint32(p, maxBytes, true);
      x.ghostty_terminal_set(term, OPT_SCROLLBACK_MAX_BYTES, p);
      x.ghostty_wasm_free_usize(p);
    }
    const screenAtReady = withPlain ? this.plain(term).trimEnd().split("\n").slice(-this.termGet(term, TERM_ROWS)) : null;
    const t1 = performance.now();
    let pages = 0, longestPage = 0, n;
    for (;;) {
      const s = performance.now();
      n = x.ghostty_snapshot_decoder_next(dec);
      if (n !== OK) break;
      pages++;
      longestPage = Math.max(longestPage, performance.now() - s);
    }
    const history = performance.now() - t1;
    if (n !== NO_VALUE) throw new Error(`next ${n}`);
    x.ghostty_snapshot_decoder_free(dec);
    x.ghostty_wasm_free_u8_array(buf, bytes.length);
    const res = { copied, ready, rowsAtReady, defaultMaxBytes, historyRows, pages, history, longestPage, done: ready + history,
      scrollbackRows: this.termGet(term, TERM_SCROLLBACK_ROWS) };
    if (withPlain) {
      res.plain = this.plain(term);
      const final = res.plain.trimEnd().split("\n").slice(-screenAtReady.length);
      res.screenAtReadyMatches = final.join("\n") === screenAtReady.join("\n");
    }
    x.ghostty_terminal_free(term);
    return res;
  }
}
