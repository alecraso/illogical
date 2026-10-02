// Cross-build GHOSTSNP check, compiled once against each libghostty-vt with
// that build's own headers (the Rust bindings at 8953a74 match only the
// pinned C API).
//
//   snapcheck info
//   snapcheck encode <fixture.bin> <cols> <rows> <out.snap> [off:cols:rows]
//   snapcheck decode <in.snap> [<fixture.bin> <cols> <rows> [off:cols:rows]]
//
// decode prints the decoder's result. With a fixture it also feeds the
// fixture into a fresh terminal of this build and compares it with the
// decoded one: formatter VT output with every extra (cells, styles,
// cursor, modes, scroll region, tab stops, pwd...), plain text including
// scrollback, cursor and pending wrap, as restored and after a DECRC probe
// and after leaving the alt screen.
#include <ghostty/vt.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *rname(GhosttyResult r) {
  switch (r) {
  case GHOSTTY_SUCCESS: return "SUCCESS";
  case GHOSTTY_OUT_OF_MEMORY: return "OUT_OF_MEMORY";
  case GHOSTTY_INVALID_VALUE: return "INVALID_VALUE";
  case GHOSTTY_OUT_OF_SPACE: return "OUT_OF_SPACE";
  case GHOSTTY_NO_VALUE: return "NO_VALUE";
  default: return "OTHER";
  }
}

static uint8_t *slurp(const char *path, size_t *len) {
  FILE *f = fopen(path, "rb");
  if (!f) { perror(path); exit(2); }
  fseek(f, 0, SEEK_END);
  *len = ftell(f);
  fseek(f, 0, SEEK_SET);
  uint8_t *b = malloc(*len ? *len : 1);
  if (fread(b, 1, *len, f) != *len) { perror("read"); exit(2); }
  fclose(f);
  return b;
}

static GhosttyTerminal mk(uint16_t cols, uint16_t rows) {
  GhosttyTerminal t;
  if (ghostty_terminal_new(NULL, &t, cols, rows) != GHOSTTY_SUCCESS) exit(3);
  size_t sb = 64u << 20;
  ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &sb);
  ghostty_terminal_resize(t, cols, rows, 8, 16);
  return t;
}

static GhosttyTerminal feed(const char *path, int cols, int rows, const char *rs) {
  size_t n;
  uint8_t *b = slurp(path, &n);
  GhosttyTerminal t = mk(cols, rows);
  size_t off = n;
  int c2 = 0, r2 = 0;
  if (rs) sscanf(rs, "%zu:%d:%d", &off, &c2, &r2);
  ghostty_terminal_vt_write(t, b, off);
  if (rs) {
    ghostty_terminal_resize(t, c2, r2, 8, 16);
    ghostty_terminal_vt_write(t, b + off, n - off);
  }
  free(b);
  return t;
}

static char *fmt(GhosttyTerminal t, GhosttyFormatterFormat emit, size_t *len) {
  GhosttyFormatterTerminalOptions o = GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
  o.emit = emit;
  o.extra = (GhosttyFormatterTerminalExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalExtra);
  o.extra.screen = (GhosttyFormatterScreenExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterScreenExtra);
  if (emit == GHOSTTY_FORMATTER_FORMAT_VT) {
    o.extra.palette = o.extra.modes = o.extra.scrolling_region = o.extra.tabstops = true;
    o.extra.pwd = o.extra.keyboard = true;
    o.extra.screen.cursor = o.extra.screen.style = o.extra.screen.hyperlink = true;
    o.extra.screen.protection = o.extra.screen.kitty_keyboard = o.extra.screen.charsets = true;
  }
  GhosttyFormatter f;
  if (ghostty_formatter_terminal_new(NULL, &f, t, o) != GHOSTTY_SUCCESS) exit(4);
  uint8_t *p;
  if (ghostty_formatter_format_alloc(f, NULL, &p, len) != GHOSTTY_SUCCESS) exit(5);
  ghostty_formatter_free(f);
  char *s = malloc(*len + 1);
  memcpy(s, p, *len);
  s[*len] = 0;
  ghostty_free(NULL, p, *len);
  return s;
}

static int compare(GhosttyTerminal a, GhosttyTerminal b, const char *what) {
  int bad = 0;
  GhosttyFormatterFormat kinds[2] = {GHOSTTY_FORMATTER_FORMAT_VT, GHOSTTY_FORMATTER_FORMAT_PLAIN};
  const char *names[2] = {"vt+extras", "plain"};
  for (int k = 0; k < 2; k++) {
    size_t la, lb;
    char *sa = fmt(a, kinds[k], &la), *sb = fmt(b, kinds[k], &lb);
    if (la != lb || memcmp(sa, sb, la)) {
      size_t i = 0;
      while (i < la && i < lb && sa[i] == sb[i]) i++;
      printf("    %s: %s differs (%zu vs %zu bytes, first at %zu)\n", what, names[k], la, lb, i);
      bad++;
    }
    free(sa);
    free(sb);
  }
  uint16_t ax, ay, bx, by;
  bool aw, bw;
  ghostty_terminal_get(a, GHOSTTY_TERMINAL_DATA_CURSOR_X, &ax);
  ghostty_terminal_get(a, GHOSTTY_TERMINAL_DATA_CURSOR_Y, &ay);
  ghostty_terminal_get(b, GHOSTTY_TERMINAL_DATA_CURSOR_X, &bx);
  ghostty_terminal_get(b, GHOSTTY_TERMINAL_DATA_CURSOR_Y, &by);
  ghostty_terminal_get(a, GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP, &aw);
  ghostty_terminal_get(b, GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP, &bw);
  if (ax != bx || ay != by || aw != bw) {
    printf("    %s: cursor %u,%u%s vs %u,%u%s\n", what, ax, ay, aw ? " wrap" : "", bx, by, bw ? " wrap" : "");
    bad++;
  }
  return bad;
}

static GhosttyResult decode(const uint8_t *snap, size_t n, GhosttyTerminal *out) {
  GhosttySnapshotDecoder d;
  GhosttyResult r = ghostty_snapshot_decoder_new_buf(NULL, &d, snap, n);
  if (r != GHOSTTY_SUCCESS) return r;
  r = ghostty_snapshot_decoder_decode(d, out);
  ghostty_snapshot_decoder_free(d);
  return r;
}

int main(int argc, char **argv) {
  if (argc >= 2 && !strcmp(argv[1], "info")) {
    GhosttyString v, pre, build;
    ghostty_build_info(GHOSTTY_BUILD_INFO_VERSION_STRING, &v);
    ghostty_build_info(GHOSTTY_BUILD_INFO_VERSION_PRE, &pre);
    ghostty_build_info(GHOSTTY_BUILD_INFO_VERSION_BUILD, &build);
    printf("version_string=%.*s pre=%.*s build=%.*s\n", (int)v.len, v.ptr, (int)pre.len, pre.ptr, (int)build.len,
           build.ptr);
    return 0;
  }
  if (argc >= 6 && !strcmp(argv[1], "encode")) {
    GhosttyTerminal t = feed(argv[2], atoi(argv[3]), atoi(argv[4]), argc > 6 ? argv[6] : NULL);
    uint8_t *p;
    size_t n;
    GhosttyResult r = ghostty_snapshot_encode_alloc(t, NULL, &p, &n);
    if (r != GHOSTTY_SUCCESS) { printf("encode: %s\n", rname(r)); return 1; }
    FILE *f = fopen(argv[5], "wb");
    fwrite(p, 1, n, f);
    fclose(f);
    ghostty_free(NULL, p, n);
    return 0;
  }
  if (argc >= 3 && !strcmp(argv[1], "decode")) {
    size_t n;
    uint8_t *snap = slurp(argv[2], &n);
    GhosttyTerminal b;
    GhosttyResult r = decode(snap, n, &b);
    printf("  decode: %s\n", rname(r));
    if (r != GHOSTTY_SUCCESS || argc < 6) return r == GHOSTTY_SUCCESS ? 0 : 1;
    const char *rs = argc > 6 ? argv[6] : NULL;
    int cols = atoi(argv[4]), rows = atoi(argv[5]);
    GhosttyTerminal a = feed(argv[3], cols, rows, rs);
    int bad = compare(a, b, "as restored");
    const char *probes[2] = {"\x1b" "8q@", "\x1b[?1049l\x1b" "8q@"};
    const char *pn[2] = {"after DECRC", "after 1049l+DECRC"};
    for (int i = 0; i < 2; i++) {
      GhosttyTerminal a2 = feed(argv[3], cols, rows, rs), b2;
      decode(snap, n, &b2);
      ghostty_terminal_vt_write(a2, (const uint8_t *)probes[i], strlen(probes[i]));
      ghostty_terminal_vt_write(b2, (const uint8_t *)probes[i], strlen(probes[i]));
      bad += compare(a2, b2, pn[i]);
      ghostty_terminal_free(a2);
      ghostty_terminal_free(b2);
    }
    printf("  %s\n", bad ? "DIFF" : "identical to the fixture fed into this build");
    return bad ? 1 : 0;
  }
  fprintf(stderr, "usage: see source\n");
  return 2;
}
