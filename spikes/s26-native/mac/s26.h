// S26's Rust core, as Swift sees it (ffi/src/lib.rs).
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct S26Term S26Term;
typedef struct S26Conn S26Conn;

typedef struct {
  uint16_t col;
  uint16_t cells;
  uint32_t fg;
  uint32_t bg;  // 0xRRGGBB, or UINT32_MAX for the default background
  uint8_t flags;  // 1 bold, 2 italic, 4 underline
  const uint8_t *text;
  size_t text_len;
} S26Run;

typedef struct {
  uint32_t dirty;  // 0 clean, 1 some rows, 2 everything
  uint16_t cols, rows;
  uint32_t fg, bg;
  bool cursor_visible;
  uint16_t cursor_x, cursor_y;
  uint32_t cursor_color;
} S26Frame;

typedef void (*S26RowFn)(void *ctx, uint16_t row, const S26Run *runs, size_t n);
typedef void (*S26NotifyFn)(void *ctx);

S26Term *s26_term_new(uint16_t cols, uint16_t rows);
void s26_term_free(S26Term *);
void s26_term_write(S26Term *, const uint8_t *data, size_t len);
void s26_term_reset(S26Term *);
void s26_term_resize(S26Term *, uint16_t cols, uint16_t rows, uint32_t cell_w, uint32_t cell_h);
size_t s26_term_key(S26Term *, uint16_t mac_code, uint32_t mods, const char *text, uint8_t *out, size_t cap);
void s26_term_render(S26Term *, void *ctx, S26RowFn row_fn, S26Frame *frame);

S26Conn *s26_conn_attach(const char *sock, uint32_t pane, uint16_t cols, uint16_t rows, S26NotifyFn notify, void *ctx);
int32_t s26_conn_drain(S26Conn *, S26Term *);
void s26_conn_input(S26Conn *, const uint8_t *data, size_t len);
void s26_conn_view(S26Conn *, uint16_t cols, uint16_t rows);

char *s26_tabs_json(const char *sock);
void s26_string_free(char *);
