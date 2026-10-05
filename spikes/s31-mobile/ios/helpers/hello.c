// A separate executable shipped inside the app bundle, for posix_spawn.
#include <stdio.h>
#include <unistd.h>
int main(void) {
    printf("hello from a spawned binary, pid %d\n", getpid());
    return 0;
}
