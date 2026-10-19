/* The C twin of cases/micro/call_value.nx.
 *
 * The same algorithm, the same array, and the same clang with the same
 * flags as the Nexum case, so the difference between the two programs is
 * the language's front end and runtime and not the optimiser.
 *
 * The callee is marked noinline on purpose. The Nexum case cannot say so -
 * the language has no way to keep a call from being inlined - and has to
 * make its callee expensive instead; the C twin says it directly, so both
 * sides measure a call the optimiser cannot see into.
 *
 *   usage: call_value <case|base> <ops>
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define N 64

__attribute__((noinline)) static int64_t noinline(int64_t x)
{
    int64_t a[N];
    for (int i = 0; i < N; i++) {
        a[i] = 0;
    }
    a[x % N] = x + 1;
    return a[(x + 1) % N] + x + 1;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "case";
    long ops = argc > 2 ? atol(argv[2]) : 10000000;
    int64_t acc = 0;

    if (strcmp(mode, "base") == 0) {
        /* The paired baseline: the same array work, written inline. */
        for (long i = 0; i < ops; i++) {
            int64_t a[N];
            for (int j = 0; j < N; j++) {
                a[j] = 0;
            }
            a[i % N] = i + 1;
            acc += a[(i + 1) % N] + i + 1;
        }
    } else {
        for (long i = 0; i < ops; i++) {
            acc += noinline(i);
        }
    }

    printf("%lld\n", (long long)acc);
    return 0;
}
