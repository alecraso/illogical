// A WASI program that reads lines from stdin and answers each one: the
// shape of an interactive shell running under the interpreter.
#include <stdio.h>
#include <string.h>
#include <stdint.h>
uint32_t bench(int rounds);
int main(void) {
    char line[512];
    int n = 0;
    printf("wasi repl ready\n");
    fflush(stdout);
    while (fgets(line, sizeof line, stdin)) {
        line[strcspn(line, "\n")] = 0;
        n++;
        if (strcmp(line, "exit") == 0) break;
        if (strncmp(line, "bench", 5) == 0) printf("[%d] bench -> %u\n", n, bench(1));
        else printf("[%d] you said: %s (%zu bytes)\n", n, line, strlen(line));
        fflush(stdout);
    }
    printf("bye after %d lines\n", n);
    return 0;
}
