// S31: wasm3 (an interpreter, no JIT) inside the app: a CPU bench against
// the same C compiled natively, and a WASI program driven line by line over
// stdin/stdout, as a shell under an interpreter would be.
#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "wasm3.h"
#include "m3_api_wasi.h"

typedef void (*emit_fn)(const char *);
static emit_fn g_emit;

static void say(const char *fmt, ...) {
    char buf[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    g_emit(buf);
}

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

static uint8_t *slurp(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    *len = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *b = malloc(*len);
    fread(b, 1, *len, f);
    fclose(f);
    return b;
}

static IM3Runtime load(const char *path, IM3Module *mod_out, int wasi) {
    size_t len;
    uint8_t *bytes = slurp(path, &len);
    if (!bytes) { say("FAIL wasm read %s", path); return NULL; }
    IM3Environment env = m3_NewEnvironment();
    if (!env) { say("FAIL wasm3 environment"); return NULL; }
    IM3Runtime rt = m3_NewRuntime(env, 256 * 1024, NULL);
    if (!rt) { say("FAIL wasm3 runtime"); return NULL; }
    IM3Module mod;
    const char *step = "parse";
    M3Result r = m3_ParseModule(env, &mod, bytes, (uint32_t)len);
    if (!r) { step = "load"; r = m3_LoadModule(rt, mod); }
    if (!r && wasi) { step = "link wasi"; r = m3_LinkWASI(mod); }
    if (r) {
        M3ErrorInfo info;
        m3_GetErrorInfo(rt, &info);
        say("FAIL wasm3 %s %s: %s (%s)", step, path, r, info.message ? info.message : "");
        return NULL;
    }
    *mod_out = mod;
    return rt;
}

void s31_wasm_bench(const char *path, emit_fn emit) {
    g_emit = emit;
    IM3Module mod;
    IM3Runtime rt = load(path, &mod, 0);
    if (!rt) return;
    IM3Function f;
    M3Result r = m3_FindFunction(&f, rt, "bench");
    if (r) { say("FAIL wasm3 find bench: %s", r); return; }
    double t = now_ms();
    r = m3_CallV(f, 5);
    uint32_t ret = 0;
    if (!r) r = m3_GetResultsV(f, &ret);
    if (r) { say("FAIL wasm3 bench: %s", r); return; }
    say("info wasm3 bench(5): %.1f ms, checksum %u", now_ms() - t, ret);
}

static IM3Runtime g_rt;
static void *run_start(void *p) {
    (void)p;
    IM3Function f;
    M3Result r = m3_FindFunction(&f, g_rt, "_start");
    if (!r) r = m3_CallV(f);
    if (r && strcmp(r, m3Err_trapExit) != 0) fprintf(stderr, "wasm _start: %s\n", r);
    close(1); // EOF for the reader
    return NULL;
}

static ssize_t read_line(int fd, char *buf, size_t cap) {
    size_t n = 0;
    while (n + 1 < cap) {
        ssize_t k = read(fd, buf + n, 1);
        if (k <= 0) break;
        if (buf[n] == '\n') { buf[n] = 0; return (ssize_t)n; }
        n++;
    }
    buf[n] = 0;
    return n ? (ssize_t)n : -1;
}

void s31_wasm_repl(const char *path, emit_fn emit) {
    g_emit = emit;
    IM3Module mod;
    g_rt = load(path, &mod, 1);
    if (!g_rt) return;
    int in[2], out[2];
    pipe(in);
    pipe(out);
    int save0 = dup(0), save1 = dup(1);
    dup2(in[0], 0);
    dup2(out[1], 1);
    close(in[0]);
    close(out[1]);
    pthread_t t;
    pthread_create(&t, NULL, run_start, NULL);
    char line[512];
    read_line(out[0], line, sizeof line);
    say("ok   wasi repl started: \"%s\"", line);
    const char *inputs[] = {"hello", "ls -la", "bench", "a much longer line of input typed into the wasm program", "exit"};
    for (int i = 0; i < 5; i++) {
        double t0 = now_ms();
        dprintf(in[1], "%s\n", inputs[i]);
        ssize_t n = read_line(out[0], line, sizeof line);
        say("     -> %-10.10s answered in %.2f ms: \"%s\"", inputs[i], now_ms() - t0, n >= 0 ? line : "(eof)");
    }
    close(in[1]);
    pthread_join(t, NULL);
    close(out[0]);
    dup2(save0, 0);
    dup2(save1, 1);
    close(save0);
    close(save1);
}
