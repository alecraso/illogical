// The same CPU work natively and in wasm: a sieve plus an FNV-1a hash loop.
// Returns a checksum so neither side can skip the work.
#include <stdint.h>
#include <string.h>
#define N 2000000
static unsigned char sieve[N];
uint32_t bench(int rounds) {
    uint32_t acc = 2166136261u;
    for (int r = 0; r < rounds; r++) {
        memset(sieve, 1, N);
        uint32_t count = 0;
        for (uint32_t i = 2; i < N; i++) {
            if (!sieve[i]) continue;
            count++;
            for (uint64_t j = (uint64_t)i * i; j < N; j += i) sieve[j] = 0;
        }
        for (uint32_t i = 0; i < count; i++) { acc ^= i; acc *= 16777619u; }
        acc ^= count;
    }
    return acc;
}
