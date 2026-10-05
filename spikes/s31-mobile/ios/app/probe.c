// S31: what an iOS app process may do toward running terminals. Each probe
// reports one line through `emit`.
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <util.h>

extern char **environ;
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

static void probe_fork(void) {
    pid_t pid = fork();
    if (pid == 0) _exit(7);
    if (pid < 0) { say("FAIL fork: %d %s", errno, strerror(errno)); return; }
    int st = 0;
    waitpid(pid, &st, 0);
    say("ok   fork: child %d exited %d", pid, WEXITSTATUS(st));
}

static void probe_spawn(const char *path) {
    int fds[2];
    pipe(fds);
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, fds[1], 1);
    pid_t pid;
    char *argv[] = {(char *)path, NULL};
    int rc = posix_spawn(&pid, path, &fa, NULL, argv, environ);
    close(fds[1]);
    if (rc != 0) { say("FAIL posix_spawn %s: %d %s", path, rc, strerror(rc)); close(fds[0]); return; }
    char buf[256] = {0};
    ssize_t n = read(fds[0], buf, sizeof buf - 1);
    close(fds[0]);
    int st = 0;
    waitpid(pid, &st, 0);
    if (n > 0 && buf[n - 1] == '\n') buf[n - 1] = 0;
    say("ok   posix_spawn %s: pid %d, status %d, said \"%s\"", path, pid, st, n > 0 ? buf : "");
}

static void probe_exists(const char *path) {
    struct stat sb;
    if (stat(path, &sb) == 0) say("info %s exists (mode %o)", path, sb.st_mode);
    else say("info %s: %s", path, strerror(errno));
}

static void probe_pty(void) {
    int m, s;
    char name[128];
    if (openpty(&m, &s, name, NULL, NULL) != 0) {
        say("FAIL openpty: %d %s", errno, strerror(errno));
    } else {
        // Round trip through the line discipline: write on the slave, read on
        // the master (ONLCR turns \n into \r\n).
        write(s, "pty-ok\n", 7);
        char buf[32] = {0};
        ssize_t n = read(m, buf, sizeof buf - 1);
        say("ok   openpty: %s, master read %zd bytes%s", name, n, (n == 8 && buf[6] == '\r') ? " (ONLCR applied)" : "");
        close(m);
        close(s);
    }
    int pm = posix_openpt(O_RDWR | O_NOCTTY);
    if (pm < 0) { say("FAIL posix_openpt: %d %s", errno, strerror(errno)); return; }
    int g = grantpt(pm), u = unlockpt(pm);
    say("%s posix_openpt: fd %d grantpt %d unlockpt %d ptsname %s", (g == 0 && u == 0) ? "ok  " : "FAIL", pm, g, u, ptsname(pm) ? ptsname(pm) : "(null)");
    close(pm);
}

struct cmd_args { int (*fn)(FILE *, int, char **); FILE *out; int rc; };
static void *cmd_thread(void *p) {
    struct cmd_args *a = p;
    char *argv[] = {"ls", "-l", "/tmp", NULL};
    a->rc = a->fn(a->out, 3, argv);
    return NULL;
}

static void probe_dlopen(const char *path, const char *label) {
    void *h = dlopen(path, RTLD_NOW);
    if (!h) { say("FAIL dlopen %s: %s", label, dlerror()); return; }
    int (*fn)(FILE *, int, char **) = dlsym(h, "cmd_main");
    if (!fn) { say("FAIL dlsym cmd_main in %s", label); return; }
    char *buf = NULL;
    size_t len = 0;
    FILE *out = open_memstream(&buf, &len);
    struct cmd_args a = {fn, out, -1};
    pthread_t t;
    pthread_create(&t, NULL, cmd_thread, &a);
    pthread_join(t, NULL);
    fclose(out);
    if (len && buf[len - 1] == '\n') buf[len - 1] = 0;
    say("ok   dlopen %s + cmd_main on a thread: rc %d, \"%s\"", label, a.rc, buf);
    free(buf);
}

static void copy_file(const char *from, const char *to) {
    FILE *a = fopen(from, "rb"), *b = fopen(to, "wb");
    if (!a || !b) { if (a) fclose(a); if (b) fclose(b); return; }
    char buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, a)) > 0) fwrite(buf, 1, n, b);
    fclose(a);
    fclose(b);
}

static void probe_jit(void) {
    void *p = mmap(NULL, 16384, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON, -1, 0);
    say("%s mmap RWX anonymous: %s", p == MAP_FAILED ? "FAIL" : "ok  ", p == MAP_FAILED ? strerror(errno) : "mapped");
    if (p != MAP_FAILED) munmap(p, 16384);
    p = mmap(NULL, 16384, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON | MAP_JIT, -1, 0);
    say("%s mmap MAP_JIT: %s", p == MAP_FAILED ? "FAIL" : "ok  ", p == MAP_FAILED ? strerror(errno) : "mapped");
    if (p != MAP_FAILED) munmap(p, 16384);
    void *q = mmap(NULL, 16384, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    int rc = mprotect(q, 16384, PROT_READ | PROT_EXEC);
    say("%s mprotect RW -> RX: %s", rc ? "FAIL" : "ok  ", rc ? strerror(errno) : "allowed");
    munmap(q, 16384);
}

uint32_t bench(int rounds);

void s31_probe(const char *bundle, const char *docs, emit_fn emit) {
    g_emit = emit;
    char path[1024], path2[1024];
    say("--- processes");
    probe_fork();
    snprintf(path, sizeof path, "%s/hello", bundle);
    probe_spawn(path);
    probe_spawn("/bin/sh");
    // A copy of the bundled binary in the writable data container.
    snprintf(path2, sizeof path2, "%s/hello-copy", docs);
    copy_file(path, path2);
    chmod(path2, 0755);
    probe_spawn(path2);
    probe_exists("/bin/sh");
    probe_exists("/usr/bin/env");
    probe_exists("/usr/lib/libSystem.B.dylib");
    say("--- pty");
    probe_pty();
    say("--- in-process commands");
    snprintf(path, sizeof path, "%s/Frameworks/S31Cmd.framework/S31Cmd", bundle);
    probe_dlopen(path, "bundled framework");
    snprintf(path2, sizeof path2, "%s/S31Cmd-copy.dylib", docs);
    copy_file(path, path2);
    probe_dlopen(path2, "a copy in Documents (runtime-installed code)");
    say("--- code generation");
    probe_jit();
    say("--- native bench");
    double t = now_ms();
    uint32_t c = bench(5);
    say("info native bench(5): %.1f ms, checksum %u", now_ms() - t, c);
}

#include <libkern/OSCacheControl.h>
// Last, because it may kill the process: write two instructions into an RWX
// page and call them. Runs only if the mapping was allowed.
void s31_jit_exec(emit_fn emit) {
    g_emit = emit;
    uint32_t *p = mmap(NULL, 16384, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (p == MAP_FAILED) { say("skip jit exec: no RWX page"); return; }
    p[0] = 0x52800540; // mov w0, #42
    p[1] = 0xd65f03c0; // ret
    sys_icache_invalidate(p, 8);
    say("info jit exec: calling generated code (a crash here means it's refused)");
    int (*fn)(void) = (int (*)(void))p;
    say("ok   jit exec: generated code returned %d", fn());
}
