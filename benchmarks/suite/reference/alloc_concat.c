/* The C twin of cases/micro/alloc_concat.nx.
 *
 * The same algorithm, the same bytes, and the same clang with the same
 * flags as the Nexum case: one allocation of 65 bytes, the two strings
 * copied into it, the previous one reclaimed.
 *
 * The runtime measures the length of its arguments at run time, so the
 * twin does the same: a literal's length is folded away by the compiler,
 * and the two programs would not do the same work. The volatile pointer is
 * what keeps the length a run-time value.
 *
 *   usage: alloc_concat <case|base> <ops>
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define A "0123456789abcdef0123456789abcdef"
#define B "0123456789abcdef0123456789abcdef"

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "case";
    long ops = argc > 2 ? atol(argv[2]) : 4000000;
    const char *volatile a = A;
    const char *volatile b = B;
    long acc = 0;
    char *last = NULL;

    if (strcmp(mode, "base") == 0) {
        /* The paired baseline: the same loop, without the allocation. */
        for (long i = 0; i < ops; i++) {
            acc += 1;
        }
    } else {
        for (long i = 0; i < ops; i++) {
            size_t la = strlen(a);
            size_t lb = strlen(b);
            char *s = (char *)malloc(la + lb + 1);
            memcpy(s, a, la);
            memcpy(s + la, b, lb + 1);
            free(last);
            last = s;
            acc += 1;
        }
        if (last != NULL && strcmp(last, A B) == 0) {
            acc += 1;
        }
    }

    printf("%ld\n", acc);
    return 0;
}
