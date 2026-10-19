/* The C twin of cases/micro/gc_pause.nx.
 *
 * The same algorithm, the same bytes, and the same clang with the same
 * flags as the Nexum case: 64 strings that stay reachable, one allocation
 * per iteration, and the dead one reclaimed.
 *
 * There is no collector in C, so the twin reclaims the dead object with
 * free - in both modes, so that the allocator sees the same pattern in
 * both - and the "case" mode additionally walks the live set the way a
 * collector's mark phase does. The difference between the two modes is
 * therefore the cost of marking a live heap, and the difference between
 * the Nexum case and the Nexum baseline is the cost of a collection: mark
 * and sweep. The ratio of the two is what the collector's tracing costs
 * over the same work done by hand.
 *
 *   usage: gc_pause <case|base> <ops>
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define A "0123456789abcdef0123456789abcdef"
#define B "0123456789abcdef0123456789abcdef"
#define LIVE 64

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "case";
    long ops = argc > 2 ? atol(argv[2]) : 10000;
    const char *volatile a = A;
    const char *volatile b = B;
    char *live[LIVE];
    char *prev = NULL;
    long marked = 0;
    /* The mark phase has to happen, so its result is written to a volatile
       place once per iteration: the optimiser may not remove the work, and
       the value is not part of the checksum. */
    static volatile long sink;

    /* The live set: 64 strings that stay reachable for the whole run. */
    for (int i = 0; i < LIVE; i++) {
        size_t la = strlen(a);
        size_t lb = strlen(b);
        live[i] = (char *)malloc(la + lb + 1);
        memcpy(live[i], a, la);
        memcpy(live[i] + la, b, lb + 1);
    }

    for (long j = 0; j < ops; j++) {
        size_t la = strlen(a);
        size_t lb = strlen(b);
        char *s = (char *)malloc(la + lb + 1);
        memcpy(s, a, la);
        memcpy(s + la, b, lb + 1);
        if (strcmp(mode, "case") == 0) {
            /* The mark phase: touch every byte of every live object, the
               way a collector walks the heap it has to keep. The object
               this iteration allocated is live too - the Nexum case makes
               it the live one before it collects - and marking it keeps
               the phase from being hoisted out of the loop. */
            for (int i = 0; i < LIVE; i++) {
                for (size_t k = 0; k < la + lb + 1; k++) {
                    marked += live[i][k];
                }
            }
            for (size_t k = 0; k < la + lb + 1; k++) {
                marked += s[k];
            }
            sink = marked;
        }
        /* The sweep: the object this iteration replaced is dead. */
        free(prev);
        prev = s;
    }

    long total = 0;
    for (int i = 0; i < LIVE; i++) {
        total += (long)strlen(live[i]);
    }

    printf("%ld\n", total);
    return 0;
}
