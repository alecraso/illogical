// An ios_system-style command: a dylib (in a framework) whose entry point
// runs on the caller's thread and writes to the stream it's handed.
#include <stdio.h>
#include <pthread.h>
int cmd_main(FILE *out, int argc, char **argv) {
    fprintf(out, "cmd_main on thread %p, argc %d:", (void *)pthread_self(), argc);
    for (int i = 0; i < argc; i++) fprintf(out, " %s", argv[i]);
    fprintf(out, "\n");
    return 42;
}
